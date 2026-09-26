//! `BackendAccountSession`: token refresh, single-flight, and the saved session
//! the host persists.

use super::validate::*;
use super::*;

struct SessionState {
    loaded: bool,
    saved: Option<SavedAccountSession>,
    generation: u64,
    refresh: Option<Arc<RefreshFlight>>,
}

struct RefreshFlight {
    result: Mutex<Option<Result<String, AccountError>>>,
    ready: Condvar,
}

impl RefreshFlight {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn finish(&self, result: Result<String, AccountError>) {
        if let Ok(mut slot) = self.result.lock() {
            *slot = Some(result);
            self.ready.notify_all();
        }
    }

    fn wait(&self) -> Result<String, AccountError> {
        let mut slot = self.result.lock().map_err(|_| AccountError::Unavailable)?;
        while slot.is_none() {
            slot = self
                .ready
                .wait(slot)
                .map_err(|_| AccountError::Unavailable)?;
        }
        slot.clone().ok_or(AccountError::Unavailable)?
    }
}

pub struct BackendAccountSession<A: AccountApi, S: AccountSessionStorage> {
    api: A,
    storage: S,
    state: Mutex<SessionState>,
}

impl<A: AccountApi, S: AccountSessionStorage> BackendAccountSession<A, S> {
    pub fn new(api: A, storage: S) -> Self {
        Self {
            api,
            storage,
            state: Mutex::new(SessionState {
                loaded: false,
                saved: None,
                generation: 0,
                refresh: None,
            }),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, SessionState>, AccountError> {
        self.state.lock().map_err(|_| AccountError::Unavailable)
    }

    #[cfg(test)]
    pub(crate) fn set_generation_for_test(&self, generation: u64) {
        self.state.lock().unwrap().generation = generation;
    }

    /// Reserve an identity for an operation that may complete asynchronously.
    /// The terminal value is never handed to such an operation: once it is
    /// reached, there is no later value available to invalidate it on logout.
    fn next_generation(state: &mut SessionState) -> Result<u64, AccountError> {
        let next = state
            .generation
            .checked_add(1)
            .filter(|&generation| generation < u64::MAX)
            .ok_or(AccountError::Unavailable)?;
        state.generation = next;
        Ok(next)
    }

    fn load_locked(&self, state: &mut SessionState) -> Result<(), AccountError> {
        if !state.loaded {
            let saved = self.storage.load()?;
            if let Some(value) = &saved {
                validate_tokens(&value.tokens).map_err(|_| AccountError::Storage)?;
            }
            state.saved = saved;
            state.loaded = true;
        }
        Ok(())
    }

    pub fn status(&self) -> Result<Option<AccountUser>, AccountError> {
        let mut state = self.lock()?;
        self.load_locked(&mut state)?;
        Ok(state.saved.as_ref().map(|saved| saved.tokens.user.clone()))
    }

    pub fn providers(&self) -> Result<std::collections::HashMap<String, bool>, AccountError> {
        self.api.providers()
    }

    pub fn request_code(
        &self,
        provider: &str,
        target: &str,
    ) -> Result<AccountChallenge, AccountError> {
        validate_provider_target(provider, target)?;
        self.api.challenge(provider, target)
    }

    pub fn sign_in(&self, challenge: &str, credential: &str) -> Result<AccountUser, AccountError> {
        validate_login(challenge, credential)?;
        self.sign_in_validated(challenge, credential)
    }

    /// Completes an Apple challenge using the identity token returned by the
    /// native AuthenticationServices flow. The token never crosses the UI
    /// boundary; platform hosts pass it directly into the session.
    pub fn sign_in_apple(
        &self,
        challenge: &str,
        credential: &str,
    ) -> Result<AccountUser, AccountError> {
        validate_apple_login(challenge, credential)?;
        self.sign_in_validated(challenge, credential)
    }

    fn sign_in_validated(
        &self,
        challenge: &str,
        credential: &str,
    ) -> Result<AccountUser, AccountError> {
        let version = {
            let mut state = self.lock()?;
            let version = Self::next_generation(&mut state)?;
            state.refresh = None;
            version
        };
        let tokens = self.api.login(challenge, credential)?;
        validate_tokens(&tokens)?;
        let value = saved_session(tokens)?;
        let user = value.tokens.user.clone();
        let mut state = self.lock()?;
        if state.generation != version {
            return Err(AccountError::Cancelled);
        }
        self.storage.save(&value)?;
        state.saved = Some(value);
        state.loaded = true;
        Ok(user)
    }

    pub fn access_token(&self, rejected_token: Option<&str>) -> Result<String, AccountError> {
        let (flight, version, refresh_token) = {
            let mut state = self.lock()?;
            self.load_locked(&mut state)?;
            let current = state.saved.as_ref().ok_or(AccountError::Unauthorized)?;
            if current.expires_at_unix_ms > refresh_deadline_ms()
                && rejected_token != Some(current.tokens.access_token.as_str())
            {
                return Ok(current.tokens.access_token.clone());
            }
            if let Some(flight) = &state.refresh {
                let flight = Arc::clone(flight);
                drop(state);
                return flight.wait();
            }
            if state.generation == u64::MAX {
                return Err(AccountError::Unavailable);
            }
            let version = state.generation;
            let refresh_token = current.tokens.refresh_token.clone();
            let flight = Arc::new(RefreshFlight::new());
            state.refresh = Some(Arc::clone(&flight));
            (flight, version, refresh_token)
        };

        let api_result = self.api.refresh(&refresh_token);
        let result = {
            let mut state = self.lock()?;
            let result = if state.generation != version {
                Err(AccountError::Cancelled)
            } else {
                match api_result {
                    Ok(tokens) => {
                        match validate_tokens(&tokens).and_then(|_| saved_session(tokens)) {
                            Ok(value) => match self.storage.save(&value) {
                                Ok(()) => {
                                    let token = value.tokens.access_token.clone();
                                    state.saved = Some(value);
                                    state.loaded = true;
                                    Ok(token)
                                }
                                Err(error) => Err(error),
                            },
                            Err(error) => Err(error),
                        }
                    }
                    Err(AccountError::Unauthorized) => {
                        state.saved = None;
                        state.loaded = true;
                        self.storage.clear().and(Err(AccountError::Unauthorized))
                    }
                    Err(error) => Err(error),
                }
            };
            if state
                .refresh
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &flight))
            {
                state.refresh = None;
            }
            result
        };
        flight.finish(result.clone());
        result
    }

    pub fn credentials(
        &self,
        rejected_token: Option<&str>,
        expected_user_id: Option<&str>,
    ) -> Result<(String, String), AccountError> {
        {
            let mut state = self.lock()?;
            self.load_locked(&mut state)?;
            if expected_user_id.is_some_and(|expected| {
                state
                    .saved
                    .as_ref()
                    .map(|saved| saved.tokens.user.id.as_str())
                    != Some(expected)
            }) {
                return Err(AccountError::Cancelled);
            }
        }
        let token = self.access_token(rejected_token)?;
        let mut state = self.lock()?;
        self.load_locked(&mut state)?;
        let saved = state.saved.as_ref().ok_or(AccountError::Cancelled)?;
        if saved.tokens.access_token != token
            || expected_user_id.is_some_and(|expected| saved.tokens.user.id != expected)
        {
            return Err(AccountError::Cancelled);
        }
        Ok((saved.tokens.user.id.clone(), token))
    }

    pub fn profile(&self) -> Result<AccountProfile, AccountError> {
        let (user_id, token) = self.credentials(None, None)?;
        let profile = match self.api.profile(&token) {
            Err(AccountError::Unauthorized) => {
                let (_, replacement) = self.credentials(Some(&token), Some(&user_id))?;
                self.api.profile(&replacement)?
            }
            result => result?,
        };
        validate_profile(&profile)?;
        if profile.user.id != user_id {
            return Err(AccountError::Cancelled);
        }
        self.update_user(profile.user.clone())?;
        Ok(profile)
    }

    pub fn rename(&self, display_name: &str) -> Result<AccountProfile, AccountError> {
        validate_display_name(display_name)?;
        let (user_id, token) = self.credentials(None, None)?;
        if let Err(error) = self.api.rename(display_name, &token) {
            if error != AccountError::Unauthorized {
                return Err(error);
            }
            let (_, replacement) = self.credentials(Some(&token), Some(&user_id))?;
            self.api.rename(display_name, &replacement)?;
        }
        self.profile()
    }

    pub fn logout(&self, all: bool) -> Result<(), AccountError> {
        let token = match self.access_token(None) {
            Ok(token) => token,
            Err(error) => {
                self.forget()?;
                return Err(error);
            }
        };
        self.forget()?;
        self.api.logout(&token, all)
    }

    pub fn delete_account(&self) -> Result<(), AccountError> {
        let (user_id, token) = self.credentials(None, None)?;
        let result = match self.api.delete_account(&token) {
            Err(AccountError::Unauthorized) => {
                let (_, replacement) = self.credentials(Some(&token), Some(&user_id))?;
                self.api.delete_account(&replacement)
            }
            result => result,
        };
        result?;
        self.forget()
    }

    pub fn chat_models(&self) -> Result<AccountChatModels, AccountError> {
        self.authenticated(|api, token| api.chat_models(token))
    }

    pub fn chat(
        &self,
        messages: &[AccountChatMessage],
        model: &str,
    ) -> Result<String, AccountError> {
        self.authenticated(|api, token| api.chat(messages, model, token))
    }

    fn authenticated<T, F>(&self, operation: F) -> Result<T, AccountError>
    where
        F: Fn(&A, &str) -> Result<T, AccountError>,
    {
        let (user_id, token) = self.credentials(None, None)?;
        match operation(&self.api, &token) {
            Err(AccountError::Unauthorized) => {
                let (_, replacement) = self.credentials(Some(&token), Some(&user_id))?;
                operation(&self.api, &replacement)
            }
            result => result,
        }
    }

    pub fn preference_schema(&self) -> Result<AccountPreferenceSchema, AccountError> {
        let schema = self.authenticated(|api, token| api.preference_schema(token))?;
        validate_preference_schema(&schema)?;
        Ok(schema)
    }

    pub fn preferences(&self) -> Result<AccountPreferences, AccountError> {
        let preferences = self.authenticated(|api, token| api.preferences(token))?;
        validate_account_preferences(&preferences)?;
        Ok(preferences)
    }

    pub fn put_preferences(
        &self,
        preferences: &AccountPreferences,
    ) -> Result<AccountPreferences, AccountError> {
        validate_account_preferences(preferences)?;
        let updated = self.authenticated(|api, token| api.put_preferences(preferences, token))?;
        validate_account_preferences(&updated)?;
        Ok(updated)
    }

    pub fn clipboard(&self, search: &str) -> Result<AccountClipboardPage, AccountError> {
        self.authenticated(|api, token| api.clipboard(search, token))
    }

    pub fn set_clipboard_enabled(&self, enabled: bool) -> Result<(), AccountError> {
        self.authenticated(|api, token| api.set_clipboard_enabled(enabled, token))
    }

    pub fn add_clipboard(&self, text: &str) -> Result<AccountClipboardItem, AccountError> {
        self.authenticated(|api, token| api.add_clipboard(text, token))
    }

    pub fn delete_clipboard(&self, id: Option<&str>) -> Result<(), AccountError> {
        self.authenticated(|api, token| api.delete_clipboard(id, token))
    }

    pub fn dictionary(
        &self,
        kind: DictionaryKind,
        search: &str,
        offset: usize,
    ) -> Result<AccountDictionaryPage, AccountError> {
        self.authenticated(|api, token| api.dictionary(kind, search, offset, token))
    }

    pub fn dictionary_catalog(
        &self,
        kind: DictionaryKind,
        code: &str,
        offset: usize,
        scheme: &str,
        profile: &str,
    ) -> Result<AccountDictionaryCatalogPage, AccountError> {
        self.authenticated(|api, token| {
            api.dictionary_catalog(kind, code, offset, scheme, profile, token)
        })
    }

    pub fn dictionary_changes(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<AccountDictionaryChangePage, AccountError> {
        self.authenticated(|api, token| api.dictionary_changes(after, limit, token))
    }

    pub fn dictionary_snapshot(&self) -> Result<Vec<u8>, AccountError> {
        self.authenticated(|api, token| api.dictionary_snapshot(token))
    }

    pub fn dictionary_snapshot_to_file(&self, destination: &Path) -> Result<u64, AccountError> {
        self.authenticated(|api, token| api.dictionary_snapshot_to_file(destination, token))
    }

    pub fn restore_dictionary_snapshot(
        &self,
        snapshot: &[u8],
        revision: i64,
    ) -> Result<AccountDictionarySnapshotRestore, AccountError> {
        self.authenticated(|api, token| api.restore_dictionary_snapshot(snapshot, revision, token))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn edit_dictionary_catalog(
        &self,
        kind: DictionaryKind,
        code: &str,
        word: &str,
        revision: i64,
        replacement: Option<(&str, &str, i64)>,
    ) -> Result<AccountDictionaryChange, AccountError> {
        self.authenticated(|api, token| {
            api.edit_dictionary_catalog(kind, code, word, revision, replacement, token)
        })
    }

    pub fn personal_candidates(
        &self,
        query: &AccountCandidateQuery,
    ) -> Result<AccountPersonalCandidates, AccountError> {
        self.authenticated(|api, token| api.personal_candidates(query, token))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rank_candidate(
        &self,
        query: &AccountCandidateQuery,
        code: &str,
        word: &str,
        revision: i64,
        mode: &str,
        linear_step: i64,
        trigger_count: i64,
        force_top: bool,
    ) -> Result<AccountRankingResult, AccountError> {
        self.authenticated(|api, token| {
            api.rank_candidate(
                query,
                code,
                word,
                revision,
                mode,
                linear_step,
                trigger_count,
                force_top,
                token,
            )
        })
    }

    pub fn remove_candidate(
        &self,
        query: &AccountCandidateQuery,
        code: &str,
        word: &str,
        revision: i64,
    ) -> Result<AccountDictionaryChange, AccountError> {
        self.authenticated(|api, token| api.remove_candidate(query, code, word, revision, token))
    }

    pub fn fixed_positions(
        &self,
        context: &str,
        offset: usize,
    ) -> Result<AccountFixedPositions, AccountError> {
        self.authenticated(|api, token| api.fixed_positions(context, offset, token))
    }

    pub fn set_fixed_position(
        &self,
        context: &str,
        code: &str,
        word: &str,
        position: Option<i64>,
        revision: i64,
    ) -> Result<AccountDictionaryRevision, AccountError> {
        self.authenticated(|api, token| {
            api.set_fixed_position(context, code, word, position, revision, token)
        })
    }

    pub fn add_dictionary(
        &self,
        kind: DictionaryKind,
        code: &str,
        word: &str,
        weight: i64,
    ) -> Result<AccountDictionaryChange, AccountError> {
        self.authenticated(|api, token| api.add_dictionary(kind, code, word, weight, token))
    }

    pub fn update_dictionary(
        &self,
        kind: DictionaryKind,
        id: &str,
        code: &str,
        word: &str,
        weight: i64,
        revision: i64,
    ) -> Result<AccountDictionaryChange, AccountError> {
        self.authenticated(|api, token| {
            api.update_dictionary(kind, id, code, word, weight, revision, token)
        })
    }

    pub fn delete_dictionary(
        &self,
        kind: DictionaryKind,
        id: &str,
        revision: i64,
    ) -> Result<AccountDictionaryChange, AccountError> {
        self.authenticated(|api, token| api.delete_dictionary(kind, id, revision, token))
    }

    pub fn import_dictionary(
        &self,
        kind: DictionaryKind,
        format: &str,
        text: &str,
    ) -> Result<AccountDictionaryImportResult, AccountError> {
        self.authenticated(|api, token| api.import_dictionary(kind, format, text, token))
    }

    pub fn export_dictionary(
        &self,
        kind: DictionaryKind,
        format: &str,
    ) -> Result<AccountDictionaryExport, AccountError> {
        self.authenticated(|api, token| api.export_dictionary(kind, format, token))
    }

    pub fn forget(&self) -> Result<(), AccountError> {
        let mut state = self.lock()?;
        // No asynchronous operation can be running at the terminal value:
        // next_generation refuses to issue it there. Keep the value stable
        // while still clearing the account state.
        state.generation = state.generation.saturating_add(1);
        state.refresh = None;
        state.saved = None;
        state.loaded = true;
        self.storage.clear()
    }

    fn update_user(&self, user: AccountUser) -> Result<(), AccountError> {
        let mut state = self.lock()?;
        self.load_locked(&mut state)?;
        let current = state.saved.as_mut().ok_or(AccountError::Cancelled)?;
        if current.tokens.user.id != user.id {
            return Err(AccountError::Cancelled);
        }
        current.tokens.user = user;
        self.storage.save(current)
    }
}

fn saved_session(tokens: AccountTokens) -> Result<SavedAccountSession, AccountError> {
    let now = unix_ms()?;
    let duration = tokens
        .expires_in
        .checked_mul(1000)
        .ok_or(AccountError::Unavailable)?;
    let expires_at_unix_ms = now.checked_add(duration).ok_or(AccountError::Unavailable)?;
    Ok(SavedAccountSession {
        tokens,
        expires_at_unix_ms,
    })
}

fn unix_ms() -> Result<u64, AccountError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AccountError::Unavailable)?
        .as_millis();
    u64::try_from(millis).map_err(|_| AccountError::Unavailable)
}

fn refresh_deadline_ms() -> u64 {
    unix_ms()
        .unwrap_or(u64::MAX)
        .saturating_add(REFRESH_EARLY_SECONDS * 1000)
}

pub trait AccountSession {
    type Error;
    fn identity(&self) -> impl Future<Output = Result<AccountIdentity, Self::Error>> + Send;
    fn bearer_token(&self) -> impl Future<Output = Result<String, Self::Error>> + Send;
    fn refresh(&self) -> impl Future<Output = Result<String, Self::Error>> + Send;
}

impl<A: AccountApi, S: AccountSessionStorage> AccountSession for BackendAccountSession<A, S> {
    type Error = AccountError;

    async fn identity(&self) -> Result<AccountIdentity, Self::Error> {
        self.status()?
            .map(|user| AccountIdentity { user_id: user.id })
            .ok_or(AccountError::Unauthorized)
    }

    async fn bearer_token(&self) -> Result<String, Self::Error> {
        self.access_token(None)
    }

    async fn refresh(&self) -> Result<String, Self::Error> {
        let current = self.access_token(None)?;
        self.access_token(Some(&current))
    }
}
