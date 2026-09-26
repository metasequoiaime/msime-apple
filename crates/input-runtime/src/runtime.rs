//! `Runtime`: the orchestration between keystrokes, the engine, and everything the
//! providers return asynchronously.

use super::*;

pub enum Action {
    ResetCache,
    Character {
        value: u8,
        shift: bool,
    },
    Punctuation(u8),
    /// Finish the highlighted composition and append the literal ASCII mark.
    /// Linux uses this when IBus surrounding text says smart punctuation
    /// should stay ASCII; the Engine's normal punctuation table remains
    /// authoritative for every other punctuation action.
    PunctuationAscii(u8),
    Command(Command),
    SegmentBackspace,
    SegmentMoveLeft,
    SegmentMoveRight,
    Select(CandidateId),
    /// Select any candidate in the current Engine generation. This is reserved
    /// for hosts that explicitly requested [`Runtime::all_candidates`].
    SelectAnyCandidate(CandidateId),
    SelectEdge(CandidateId, CandidateEdge),
    PinCandidate(CandidateId),
    RemoveCandidate(CandidateId),
    FixCandidatePosition(CandidateId, u8),
    ClearCandidatePosition(CandidateId),
    ChooseNineKeySpelling(NineKeySpellingId),
    SelectHighlighted,
    Finish,
    NextPage,
    PreviousPage,
    NextCandidate,
    PreviousCandidate,
    FirstCandidate,
    LastCandidate,
}

/// What one selection of a phrase-in-progress can be taken back to.
///
/// `word_before` is the whole held phrase as it stood before the selection, not just the piece it
/// added: the reference restores its accumulated word wholesale for the same reason - a selection
/// that recorded nothing sits between two that did, and only the whole word puts them all back.
pub(crate) struct PhraseSelection {
    pub(crate) word_before: String,
    pub(crate) reading: String,
}

pub struct Runtime<E: InputEngine = Session> {
    pub(crate) engine: E,
    pub(crate) session: u64,
    pub(crate) generation: u64,
    pub(crate) focused: bool,
    pub(crate) page_size: usize,
    pub(crate) highlighted: usize,
    pub(crate) translations: HashMap<String, String>,
    pub(crate) cached: EngineSnapshot,
    /// The Engine's own index for each seat of `cached`.
    ///
    /// `rerank`, `demote_runner_up_readings` and `normalize_online_slots` reorder the cached list, but the Engine knows nothing of that and selects by its own order. Every call that names a candidate to the Engine goes through [`Self::engine_index`]; without it, an AI candidate seated in slot 1 committed whatever the Engine held at 1.
    pub(crate) engine_order: Vec<usize>,
    pub(crate) snapshot_valid: bool,
    pub(crate) character_width: CharacterWidth,
    pub(crate) touch_keyboard_layout: TouchKeyboardLayout,
    /// Whether the host draws a half-composed phrase itself instead of having it committed.
    ///
    /// Picking a candidate that consumes only part of the input leaves the Engine composing the
    /// rest, and it hands back the piece that was chosen. The reference keeps that piece inside its
    /// composition - `word_for_creating_word` is prepended to the reading and the caret is shifted
    /// past it - and commits the phrase as one piece when the composition ends. This runtime sent
    /// it to the document immediately, so half a phrase landed in the application while the user
    /// was still typing the rest of it.
    ///
    /// Off by default because a host that does not draw [`View::phrase_prefix`] would show nothing
    /// at all for that piece. Each host turns it on as it learns to draw it.
    pub(crate) phrase_preedit: bool,
    /// The piece already chosen for the phrase being composed, held back from the document.
    ///
    /// Non-empty while the Engine is still composing, and in one case after its reading is gone: a segment Backspace (Ctrl+Backspace) that empties a reading whose phrase still has a selection to take back leaves the phrase in the composition, as the reference's `keep_creating_word_after_empty_raw` does. A host must therefore count a non-empty [`View::phrase_prefix`] as a composition, alongside the reading and the candidates.
    pub(crate) phrase_prefix: String,
    /// One entry per selection that grew the held phrase, newest last.
    ///
    /// This is what lets the user go back: Backspace on the last of the reading puts the selection
    /// that consumed it back the way it was, instead of deleting a letter and ending the
    /// composition. The reference keeps the same stack in its Server
    /// (`CompositionState::selection_history`) and its two rules read exactly these fields.
    ///
    /// A selection that consumed no reading is not recorded, because there is nothing for it to
    /// restore - the same reason the reference refuses an empty `consumed_raw_input_with_cases`.
    pub(crate) phrase_selections: Vec<PhraseSelection>,
    /// Recently committed text, sent to the AI provider as context.
    ///
    /// The reference sends what the user has just written so a suggestion fits
    /// the sentence in progress. Every host but Linux left this empty, which
    /// made AI suggestions guess from the pinyin alone.
    pub(crate) ai_context: String,
    /// Reorders candidates the pinyin decoder assembled, when a host supplied a model.
    ///
    /// Absent unless a host calls [`Runtime::set_reranker`], and absent is the only state the
    /// hosts that ship no model ever see.
    pub(crate) reranker: Option<Reranker>,
    /// A second, larger model run once the user stops typing, when one is attached.
    ///
    /// Capacity is the most effective lever the model has — the 24M preset beats the 6.8M one by
    /// 49 points of top-1 on the harvested failure set — and it is also the one the keystroke path
    /// cannot afford: the same model measures p95 153ms against a 16ms frame, with the slowest
    /// keystroke at 342ms. Both numbers are real and they do not have to be reconciled, because
    /// they are answers to different questions. While the user is typing, the first row has to be
    /// plausible now; when the user stops to read the candidates, it has to be right. The fast
    /// model owns the first job and this one owns the second.
    pub(crate) settled_reranker: Option<Reranker>,
}

/// `CandidateSource::Generated`: a whole-sentence path the word lattice assembled. The one source
/// whose members really are alternative readings of the same key.
pub(crate) const LATTICE_SOURCE: u8 = 8;

/// Move the flagged elements to the end, keeping both groups in their existing order.
pub(crate) fn move_to_back<T>(items: &mut Vec<T>, moved: &[bool]) {
    let mut flags = moved.iter();
    let mut tail: Vec<T> = Vec::new();
    let mut head: Vec<T> = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        if flags.next().copied().unwrap_or(false) {
            tail.push(item);
        } else {
            head.push(item);
        }
    }
    head.append(&mut tail);
    *items = head;
}

/// Move the element at `index` to the front, keeping everything else in its existing order.
///
/// A rotation rather than a swap, so the rest of the list stays as the engine ranked it: promoting
/// one candidate is the whole change, not a reshuffle.
fn apply_order<T: Clone>(items: &mut Vec<T>, order: &[usize]) {
    *items = order.iter().map(|index| items[*index].clone()).collect();
}

fn rotate_to_front<T>(items: &mut [T], index: usize) {
    items[..=index].rotate_right(1);
}

impl Runtime<Session> {
    /// A live host mode changes neither composition nor candidate identity.
    pub fn set_chinese_punctuation_enabled(&mut self, enabled: bool) -> Result<(), RuntimeError> {
        self.engine
            .set_chinese_punctuation_enabled(enabled)
            .map_err(|error| RuntimeError::Engine(error.to_string()))
    }

    pub fn online_query(&self) -> Result<Option<OnlineQuery>, RuntimeError> {
        let query = self
            .engine
            .online_query()
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        if !query.available {
            return Ok(None);
        }
        Ok(Some(OnlineQuery {
            scheme: query.scheme,
            generation: query.generation,
            identity: query.identity,
            query_text: query.query_text,
            cache_key: query.cache_key,
            pinyin_segments: query.pinyin_segments,
            cloud_eligible: query.cloud_eligible,
            ai_eligible: query.ai_eligible,
            cloud_candidates: true,
            session_id: query.session_id,
            ai_context: self.ai_context.clone(),
            ai_assistant: None,
            ai_cache_only: false,
        }))
    }

    /// Queue the current eligible query for an injected provider. Hosts call
    /// this after dispatching input; the bounded worker performs I/O off-thread.
    pub fn submit_online_query(&self, worker: &OnlineProviderWorker) -> Result<bool, RuntimeError> {
        Ok(self
            .online_query()?
            .is_some_and(|query| worker.submit(query)))
    }

    pub fn apply_online_candidate(
        &mut self,
        query: &OnlineQuery,
        candidate: &str,
        source: u8,
    ) -> Result<bool, RuntimeError> {
        // Provider callbacks are asynchronous and can be malformed even when
        // their query identity is still current. Keep the single-item path
        // subject to the same bounds as the batch path before handing text to
        // Engine; the Windows source rejects empty callback results as well.
        if candidate.is_empty()
            || candidate.len() > 4096
            || candidate.chars().any(char::is_control)
            || source > 1
            // Windows only merges a cloud suggestion into an existing
            // candidate page.  A callback arriving after the local page was
            // cleared must not manufacture a new page from stale provider
            // state.  AI suggestions intentionally do not use this guard:
            // Windows accepts them for an otherwise eligible pinyin query
            // even when the local dictionary returned no rows.
            || (source == 0 && self.cached.candidates.is_empty())
            || (source == 0 && (!query.cloud_candidates || !query.cloud_eligible))
            || (source == 1 && !query.ai_eligible)
        {
            return Ok(false);
        }
        let query = OnlineQuerySnapshot {
            available: true,
            scheme: query.scheme,
            generation: query.generation,
            identity: query.identity.clone(),
            query_text: query.query_text.clone(),
            cache_key: query.cache_key.clone(),
            pinyin_segments: query.pinyin_segments.clone(),
            cloud_eligible: query.cloud_eligible,
            ai_eligible: query.ai_eligible,
            session_id: query.session_id,
        };
        let applied = self
            .engine
            .apply_online_candidate(&query, candidate, source)
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        if applied {
            // An asynchronous provider replaces the visible Engine candidate
            // set without going through dispatch(). Advance the host-owned
            // identity just as an input action does, so stale candidate IDs
            // cannot select the pre-provider page and Windows UI mailboxes can
            // recognize the replacement as a new rendered generation.
            self.advance()?;
            self.refresh()
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        }
        Ok(applied)
    }
    pub fn apply_online_candidates(
        &mut self,
        query: &OnlineQuery,
        candidates: &[String],
        source: u8,
    ) -> Result<bool, RuntimeError> {
        let limit = if source == 0 {
            1
        } else {
            query
                .ai_assistant
                .as_ref()
                .filter(|ai| ai.enabled)
                .map_or(0, |ai| usize::from(ai.candidate_limit.clamp(1, 10)))
        };
        if candidates.is_empty()
            || candidates.len() > limit
            || candidates.iter().any(|text| {
                text.is_empty() || text.len() > 4096 || text.chars().any(char::is_control)
            })
            || source > 1
            || (source == 0 && self.cached.candidates.is_empty())
            || (source == 0 && (!query.cloud_candidates || !query.cloud_eligible))
            || (source == 1 && !query.ai_eligible)
        {
            return Ok(false);
        }
        let query = OnlineQuerySnapshot {
            available: true,
            scheme: query.scheme,
            generation: query.generation,
            identity: query.identity.clone(),
            query_text: query.query_text.clone(),
            cache_key: query.cache_key.clone(),
            pinyin_segments: query.pinyin_segments.clone(),
            cloud_eligible: query.cloud_eligible,
            ai_eligible: query.ai_eligible,
            session_id: query.session_id,
        };
        let applied = self
            .engine
            .apply_online_candidates(&query, candidates, source)
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        if applied {
            self.advance()?;
            self.refresh()
                .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        }
        Ok(applied)
    }
}

impl<E: InputEngine> Runtime<E> {
    pub fn set_paired_punctuation_enabled(&mut self, enabled: bool) -> Result<(), RuntimeError> {
        self.engine.set_paired_punctuation_enabled(enabled)
    }

    pub fn balance_paired_punctuation_after_auto_close(
        &mut self,
        opening: u8,
    ) -> Result<(), RuntimeError> {
        if opening != b'<' {
            return Err(RuntimeError::InvalidPunctuation);
        }
        self.engine
            .balance_paired_punctuation_after_auto_close(opening)
    }

    pub fn set_punctuation_lock(&mut self, lock: u8) -> Result<(), RuntimeError> {
        self.engine.set_punctuation_lock(lock)
    }

    pub fn set_dedicated_english(&mut self, enabled: bool) -> Result<(), RuntimeError> {
        self.advance()?;
        self.engine.set_dedicated_english(enabled)?;
        self.refresh()
    }

    pub fn new(engine: E, page_size: u8) -> Result<Self, RuntimeError> {
        Self::new_with_touch_layout(engine, page_size, TouchKeyboardLayout::default())
    }

    pub fn new_with_touch_layout(
        engine: E,
        page_size: u8,
        touch_keyboard_layout: TouchKeyboardLayout,
    ) -> Result<Self, RuntimeError> {
        if !(1..=9).contains(&page_size) {
            return Err(RuntimeError::InvalidPageSize);
        }
        let session = NEXT_SESSION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| RuntimeError::IdentityExhausted)?;
        let cached = engine.snapshot()?;
        Ok(Self {
            engine,
            session,
            generation: 0,
            ai_context: String::new(),
            reranker: None,
            settled_reranker: None,
            focused: false,
            page_size: page_size.into(),
            highlighted: 0,
            translations: HashMap::new(),
            engine_order: (0..cached.candidates.len()).collect(),
            cached,
            snapshot_valid: true,
            character_width: CharacterWidth::Halfwidth,
            touch_keyboard_layout,
            phrase_preedit: false,
            phrase_prefix: String::new(),
            phrase_selections: Vec::new(),
        })
    }

    /// Hold a half-composed phrase in the composition instead of committing its parts.
    ///
    /// The host promises to draw [`View::phrase_prefix`] ahead of the editing text; see the field
    /// for why this is the host's call. Turning it off while a phrase is held commits what is held,
    /// because the alternative is dropping text the user already chose.
    pub fn set_phrase_preedit(&mut self, enabled: bool) -> Option<String> {
        self.phrase_preedit = enabled;
        if enabled || self.phrase_prefix.is_empty() {
            self.phrase_selections.clear();
            return None;
        }
        self.phrase_selections.clear();
        Some(std::mem::take(&mut self.phrase_prefix))
    }

    /// Attach a candidate reranker. Hosts load the model themselves, because where a model file
    /// lives is a packaging question that differs per platform and the runtime has no business
    /// guessing at it.
    pub fn set_reranker(&mut self, reranker: Option<Reranker>) {
        self.reranker = reranker;
    }

    /// Attach the model that runs after typing settles. Absent leaves the behaviour unchanged.
    pub fn set_settled_reranker(&mut self, reranker: Option<Reranker>) {
        self.settled_reranker = reranker;
    }

    /// Re-rank the current candidates with the settled model, reporting whether the order moved.
    ///
    /// The host decides when this is: it owns the clock and already runs a settle timer for cloud
    /// candidates. The runtime has no timer of its own and should not grow one — a keystroke that
    /// arrives while this is deciding makes the whole answer stale, and only the host knows that
    /// a keystroke arrived.
    ///
    /// Returns false when nothing changed, so a host can skip redrawing the candidate window. A
    /// window that repaints identically on every pause is a flicker the user cannot explain.
    pub fn rerank_settled(&mut self) -> bool {
        // A reorder has to advance the generation (old IDs would otherwise select by the new seats),
        // so an exhausted generation cannot reorder at all.
        if self.settled_reranker.is_none()
            || self.is_idle()
            || self.generation.checked_add(1).is_none()
        {
            return false;
        }
        let before = self.cached.candidates.clone();
        std::mem::swap(&mut self.reranker, &mut self.settled_reranker);
        self.rerank();
        std::mem::swap(&mut self.reranker, &mut self.settled_reranker);
        // The same passes the fast path runs after its rerank, so the seats they fix stay fixed.
        self.demote_runner_up_readings();
        self.normalize_online_slots();
        let moved = self.cached.candidates != before;
        if moved {
            self.snapshot_valid = true;
            self.highlighted = 0;
            let _ = self.advance();
        }
        moved
    }

    pub fn set_character_width(&mut self, width: CharacterWidth) {
        self.character_width = width;
    }

    /// Put text into the committed context without typing it.
    ///
    /// The context a candidate is ranked against is whatever the user just committed, and it is
    /// what lets the model tell 会议 from 回忆. An evaluation harness has to be able to establish
    /// that context: replaying it as keystrokes would make each case depend on how well the
    /// *previous* sentence converted, which is precisely the confound a per-case measurement is
    /// supposed to remove. Bounded and focus-gated exactly as a real commit is, so a seeded
    /// session is indistinguishable from one that typed its way there.
    pub fn seed_context(&mut self, text: &str) {
        self.remember_commit(text);
    }

    /// Switch the Engine's digit interpretation only after the host finishes composition.
    pub fn set_nine_key_enabled(&mut self, enabled: bool) -> Result<(), RuntimeError> {
        if enabled && self.cached.scheme != 0 {
            return Err(RuntimeError::InvalidNineKeyScheme);
        }
        if !self.is_idle() {
            return Err(RuntimeError::CompositionActive);
        }
        if self.cached.nine_key == enabled {
            return Ok(());
        }
        self.advance()?;
        self.engine.set_nine_key_enabled(enabled)?;
        self.refresh()
    }

    pub fn view(&self) -> View {
        let page = self.highlighted / self.page_size;
        let start = page * self.page_size;
        View {
            scheme: self.cached.scheme,
            nine_key: self.cached.nine_key,
            nine_key_spellings: self.cached.nine_key_spellings.clone(),
            touch_keyboard_layout: self.touch_keyboard_layout,
            character_width: self.character_width,
            microsoft_shuangpin: self.cached.microsoft_shuangpin,
            shuangpin_profile: self.cached.shuangpin_profile.clone(),
            answered_by_pinyin_fallback: self.cached.answered_by_pinyin_fallback,
            local_mode: self.cached.local_mode.clone(),
            dedicated_english: self.cached.dedicated_english,
            session: self.session,
            generation: self.generation,
            focused: self.focused,
            preedit: self.cached.preedit.clone(),
            phrase_prefix: self.phrase_prefix.clone(),
            reading: self.cached.reading.clone(),
            editing_text: self.cached.editing_text.clone(),
            caret_position: self.cached.caret_position,
            page,
            page_size: self.page_size,
            page_count: self.cached.candidates.len().div_ceil(self.page_size),
            candidates: self
                .cached
                .candidates
                .iter()
                .enumerate()
                .skip(start)
                .take(self.page_size)
                .map(|(index, text)| self.candidate(index, text))
                .collect(),
        }
    }

    /// Copy the complete candidate generation for an explicitly opened panel.
    ///
    /// The engine holds candidates back behind the initial answer and only releases them when asked
    /// (`expand_initial_candidates`), which until now happened solely on the way into the last page.
    /// A host that pages reaches them; a host that opens the whole list instead -- which the touch
    /// keyboards do, having dropped paging -- never did, so the panel that promises everything was
    /// quietly showing the first tranche. Release them here too: this call is the request for all of
    /// them. A refusal is not fatal; the caller still gets whatever the generation already holds.
    pub fn all_candidates(&mut self) -> CandidateSnapshot {
        // Candidate IDs are tied to the generation.  Once that identity space is
        // exhausted we cannot publish a reordered snapshot safely: advancing would
        // fail and retaining the old generation would let an ID from the previous
        // seat select a different candidate.  Keep the currently published view
        // stable; callers can still inspect the candidates already released.
        if self.generation == u64::MAX {
            return self.all_candidates_cached();
        }
        // The released tail reorders the list, so the page's IDs must not keep selecting by seat.
        if self.expand_cached_candidates().unwrap_or(false) {
            // `generation == u64::MAX` was handled above, so this cannot fail.
            debug_assert!(self.advance().is_ok());
        }
        self.all_candidates_cached()
    }

    /// The generation as it stands, without asking the engine for more.
    fn all_candidates_cached(&self) -> CandidateSnapshot {
        CandidateSnapshot {
            session: self.session,
            generation: self.generation,
            preedit: self.cached.preedit.clone(),
            reading: self.cached.reading.clone(),
            candidates: self
                .cached
                .candidates
                .iter()
                .enumerate()
                .map(|(index, text)| self.candidate(index, text))
                .collect(),
        }
    }

    fn candidate(&self, index: usize, text: &str) -> Candidate {
        Candidate {
            id: CandidateId {
                session: self.session,
                generation: self.generation,
                index,
            },
            text: text.to_owned(),
            code: self
                .cached
                .candidate_codes
                .get(index)
                .cloned()
                .unwrap_or_default(),
            annotation: self
                .cached
                .candidate_annotations
                .get(index)
                .cloned()
                .unwrap_or_default(),
            source: self
                .cached
                .candidate_sources
                .get(index)
                .copied()
                .unwrap_or_default(),
            corrected: self
                .cached
                .candidate_corrected
                .get(index)
                .copied()
                .unwrap_or(false),
            fixed_position: self
                .cached
                .candidate_positions
                .get(index)
                .copied()
                .unwrap_or_default(),
            highlighted: index == self.highlighted,
            translation: self.translations.get(text).cloned(),
        }
    }

    /// A held phrase counts as a composition even when its reading is empty.
    pub fn is_idle(&self) -> bool {
        self.snapshot_valid
            && self.phrase_prefix.is_empty()
            && self.cached.preedit.is_empty()
            && self.cached.editing_text.is_empty()
            && self.cached.candidates.is_empty()
    }

    /// Apply translations to the current candidate generation. Stale async
    /// responses are ignored so a newer candidate window cannot be polluted.
    pub fn apply_translations(
        &mut self,
        generation: u64,
        translations: impl IntoIterator<Item = (String, String)>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.translations = translations.into_iter().collect();
        true
    }

    /// Presentation-only resize; a live composition keeps its numeric key map.
    pub fn set_page_size(&mut self, page_size: u8) -> Result<(), RuntimeError> {
        if !(1..=9).contains(&page_size) {
            return Err(RuntimeError::InvalidPageSize);
        }
        if self.page_size == usize::from(page_size) {
            return Ok(());
        }
        if !self.is_idle() {
            return Err(RuntimeError::CompositionActive);
        }
        self.advance()?;
        self.page_size = page_size.into();
        Ok(())
    }

    /// Preserve the host handle/focus while invalidating every old candidate ID.
    /// Validate the replacement before changing any live state.
    pub fn replace_engine(&mut self, engine: E, page_size: u8) -> Result<(), RuntimeError> {
        self.replace_engine_with_touch_layout(engine, page_size, self.touch_keyboard_layout)
    }

    pub fn replace_engine_with_touch_layout(
        &mut self,
        engine: E,
        page_size: u8,
        touch_keyboard_layout: TouchKeyboardLayout,
    ) -> Result<(), RuntimeError> {
        if !(1..=9).contains(&page_size) {
            return Err(RuntimeError::InvalidPageSize);
        }
        if !self.is_idle() {
            return Err(RuntimeError::CompositionActive);
        }
        let cached = engine.snapshot()?;
        self.advance()?;
        self.engine = engine;
        self.load_snapshot(cached);
        self.snapshot_valid = true;
        self.page_size = page_size.into();
        self.highlighted = 0;
        self.touch_keyboard_layout = touch_keyboard_layout;
        Ok(())
    }

    /// The Engine caps a single-letter query at twenty-four candidates so the first page is cheap,
    /// and hands over the rest only when asked. Without this, paging stops at that cap and the rest
    /// of the dictionary is unreachable for those queries.
    ///
    /// Expanding when the next page would be the partial last one keeps that page full the first
    /// time it is shown, rather than showing a short page that silently grows.
    ///
    /// Answers whether the arrivals filled the page the caller is already on, in which case paging
    /// has to stay put: advancing would step over the candidates that just showed up.
    fn expand_for_next_page(&mut self) -> Result<bool, RuntimeError> {
        let len = self.cached.candidates.len();
        if len == 0 {
            return Ok(false);
        }
        let page = self.highlighted / self.page_size;
        let last_page = (len - 1) / self.page_size;
        let next_is_partial_last = page + 1 == last_page && !len.is_multiple_of(self.page_size);
        if page != last_page && !next_is_partial_last {
            return Ok(false);
        }
        let page_was_full = (page + 1) * self.page_size <= len;
        if !self.expand_cached_candidates()? {
            return Ok(false);
        }
        Ok(page == last_page && !page_was_full)
    }

    /// Moving the highlight off the end of the loaded list has to release the withheld candidates
    /// too, not only paging.
    ///
    /// The same cap sits behind both. A host that walks the list one candidate at a time - which is
    /// every arrow key and every mouse wheel notch - would otherwise stop at the twenty-fourth
    /// candidate and be unable to reach the rest of the dictionary, while pressing page-down on the
    /// same query walks straight past it. The second condition mirrors the paging one: stepping into
    /// the partial last page fills it first, so it is never shown short and then grown.
    fn expand_for_next_candidate(&mut self) -> Result<(), RuntimeError> {
        let len = self.cached.candidates.len();
        if len == 0 {
            return Ok(());
        }
        let page = self.highlighted / self.page_size;
        let last_page = (len - 1) / self.page_size;
        let at_last_candidate = self.highlighted + 1 == len;
        let at_page_end = (self.highlighted + 1).is_multiple_of(self.page_size);
        let next_is_partial_last = page + 1 == last_page && !len.is_multiple_of(self.page_size);
        if !at_last_candidate && !(at_page_end && next_is_partial_last) {
            return Ok(());
        }
        self.expand_cached_candidates()?;
        Ok(())
    }

    /// Ask the Engine for what it held back, and re-apply the orderings the cached page carries:
    /// the arrivals are ranked against the candidates already on screen, not appended raw.
    fn expand_cached_candidates(&mut self) -> Result<bool, RuntimeError> {
        if !self.engine.expand_initial_candidates()? {
            return Ok(false);
        }
        let snapshot = self.engine.snapshot()?;
        self.load_snapshot(snapshot);
        self.rerank();
        self.demote_runner_up_readings();
        self.normalize_online_slots();
        Ok(true)
    }

    /// Take a snapshot straight from the Engine, whose seats are still in the Engine's order.
    fn load_snapshot(&mut self, snapshot: EngineSnapshot) {
        self.engine_order = (0..snapshot.candidates.len()).collect();
        self.cached = snapshot;
    }

    /// The Engine's index for the candidate sitting at `seat` of the cached list. A seat past the end is passed through unchanged, so the Engine keeps answering for an empty page exactly as it did.
    fn engine_index(&self, seat: usize) -> usize {
        self.engine_order.get(seat).copied().unwrap_or(seat)
    }

    fn advance(&mut self) -> Result<(), RuntimeError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(RuntimeError::IdentityExhausted)?;
        Ok(())
    }

    /// Keep the tail of what was committed, cut on a character boundary.
    ///
    /// Bounded at 1024 bytes because `query_candidates` refuses anything longer
    /// outright - an over-long context would silently disable the whole query
    /// rather than being trimmed for us.
    pub(crate) fn remember_commit(&mut self, text: &str) {
        if !self.focused {
            self.ai_context.clear();
            return;
        }
        self.ai_context.push_str(text);
        if self.ai_context.len() > 1024 {
            let mut cut = self.ai_context.len() - 1024;
            while cut < self.ai_context.len() && !self.ai_context.is_char_boundary(cut) {
                cut += 1;
            }
            self.ai_context.drain(..cut);
        }
    }

    /// Take the last selection of a phrase-in-progress back, when the key asks for it.
    ///
    /// Two rules, both the reference's (`ShouldRetreatCreatingWordSelection` and
    /// `ShouldDropCreatingWordSegment` in its `input_key_policy.h`), and both about the same
    /// situation: the user has picked a candidate that covered part of the input, is looking at the
    /// piece it produced, and wants it back.
    ///
    /// - Backspace with at most one character of reading left, the caret at its end: the key would
    ///   otherwise delete that character and end the composition, taking the chosen piece with it.
    ///   Instead the newest selection is undone - its reading comes back and the held phrase
    ///   returns to what it was before it - so the user can pick again.
    /// - Segment Backspace (Ctrl+Backspace) with nothing before the caret: the reading this key
    ///   deletes by units has already been emptied, so it deletes the selection itself. Its reading
    ///   is *not* restored: the user asked to remove the segment, not to edit its spelling.
    ///
    /// Returns `None` for every other key, which then runs as usual.
    ///
    /// The reference also requires a client that negotiated `CompositionRestore` and a host that is
    /// not UILess, because its TSF side has to rebuild the composition from a reply it may not
    /// understand. Here that condition is `phrase_preedit`: a host only turns it on once it draws
    /// the held phrase from the view, and the view is how every host here learns the composition
    /// changed.
    fn retreat_phrase_selection(
        &mut self,
        action: &Action,
    ) -> Result<Option<Transition>, RuntimeError> {
        if !self.phrase_preedit || self.phrase_selections.is_empty() {
            return Ok(None);
        }
        let reading = self.cached.editing_text.as_str();
        let caret = self.cached.caret_position;
        let restore = match action {
            Action::Command(Command::Backspace) => {
                if reading.chars().count() > 1 || caret != reading.len() {
                    return Ok(None);
                }
                true
            }
            Action::SegmentBackspace => {
                if caret != 0 {
                    return Ok(None);
                }
                false
            }
            _ => return Ok(None),
        };
        let selection = self
            .phrase_selections
            .pop()
            .expect("the stack was checked above");
        self.phrase_prefix = selection.word_before;
        if !restore {
            // The segment is gone and its spelling with it. The reading is already empty, so the
            // Engine has nothing to say; the view still changes, because the held phrase is
            // shorter - or gone, which ends the composition with nothing committed.
            self.refresh()?;
            return Ok(Some(self.transition(empty_result(true))));
        }
        // Put the reading back. The reference hands its Engine the spelling directly
        // (`set_pinyin_sequence` then `recompute_candidates`); this Engine is only reachable
        // through the keys that built the composition, so the composition is thrown away and the
        // spelling typed again. The user sees the same thing either way: the pinyin that selection
        // consumed, with its candidates, and the caret at its end.
        self.engine.command(Command::Cancel)?;
        for byte in selection.reading.bytes() {
            self.engine.character(byte, byte.is_ascii_uppercase())?;
        }
        self.refresh()?;
        Ok(Some(self.transition(empty_result(true))))
    }

    /// Keep a chosen piece of a phrase out of the document until the phrase is done.
    ///
    /// Three things can happen to what the Engine hands back:
    ///
    /// - it picked a candidate and is still composing the rest, so the piece is held;
    /// - something ended the composition and committed, so the held pieces lead that commit - the
    ///   reference does the same on Enter, which commits `word_for_creating_word` together with the
    ///   remaining raw input;
    /// - the reading is gone with nothing committed. A cancel means the user threw the whole thing away, so the held pieces go with it. A segment Backspace (`keep_empty`) that emptied the reading while a selection can still be taken back keeps the phrase in the composition, as the reference's `keep_creating_word_after_empty_raw` does: the next Backspace puts the last reading back and the next Ctrl+Backspace deletes the last chosen piece, both in [`Runtime::retreat_phrase_selection`]. Anything else commits what is held rather than dropping letters the user chose. A plain Backspace only empties the reading here with nothing to go back to - a selection that can be taken back takes that key first - and the reference ends the word in that case too.
    ///
    /// When the held phrase is all there is to send or throw away, the key acted on the composition, so it counts as handled: an Enter or Space the Engine does not want with an empty reading must not also reach the application.
    fn hold_phrase_progress(
        &mut self,
        picked: bool,
        discard: bool,
        keep_empty: bool,
        consumed: &str,
        result: &mut EngineResult,
    ) {
        if !self.phrase_preedit {
            return;
        }
        let composing = !self.cached.editing_text.is_empty();
        if picked && result.has_commit && composing {
            if !consumed.is_empty() {
                self.phrase_selections.push(PhraseSelection {
                    word_before: self.phrase_prefix.clone(),
                    reading: consumed.to_owned(),
                });
            }
            self.phrase_prefix.push_str(&result.commit);
            result.has_commit = false;
            result.commit = String::new();
            return;
        }
        if self.phrase_prefix.is_empty() || composing {
            return;
        }
        if keep_empty && !discard && !result.has_commit && !self.phrase_selections.is_empty() {
            result.handled = true;
            return;
        }
        let held = std::mem::take(&mut self.phrase_prefix);
        self.phrase_selections.clear();
        if result.has_commit {
            if !discard {
                result.commit = held + &result.commit;
            }
            return;
        }
        result.handled = true;
        if !discard {
            result.has_commit = true;
            result.commit = held;
        }
    }

    fn transition(&mut self, result: EngineResult) -> Transition {
        // Every commit passes through here, so this is the one place the AI
        // context has to be fed from.
        if result.has_commit {
            let committed = result.commit.clone();
            self.remember_commit(&committed);
        }
        Transition {
            commit_context: result.has_commit.then(|| OutputContext {
                scheme: self.cached.scheme,
                local_mode: self.cached.local_mode.clone(),
            }),
            handled: result.handled,
            commit: result.has_commit.then_some(result.commit),
            diagnostic: (!result.diagnostic.is_empty()).then_some(result.diagnostic),
            view: self.view(),
        }
    }

    /// Let the model promote a candidate the pinyin decoder assembled, if a host attached one.
    ///
    /// Reordering happens here because this is the one place a candidate list enters the runtime,
    /// so everything downstream — the view, `all_candidates`, the evaluation harness — sees the
    /// same order the user does.
    ///
    /// The candidate arrays run in parallel and every one of them has to move together. Rotating
    /// only the texts would leave each candidate wearing another's code, annotation and source.
    /// Seat the online candidates the way the reference does.
    ///
    /// `candidate_selection_policy.h` writes the arrangement out:
    ///
    /// ```text
    /// no cloud:    Chinese, English, AI, emoji, kaomoji
    /// cloud:       Chinese, cloud, AI, English, emoji, kaomoji
    /// cloud only:  Chinese, cloud, English, emoji, kaomoji
    /// base:        Chinese, English, emoji, kaomoji
    /// ```
    ///
    /// The reference applies it in its Server, on top of what the Engine returned. This client
    /// replaced that Server with this runtime and the step did not come across, so the Engine's own
    /// placement was what the user saw - and the two agree until an online candidate arrives.
    /// Injecting an AI suggestion moved the English candidate from the second seat to the fourth
    /// and put a second Chinese candidate in front of it, which is the last line of the table read
    /// backwards.
    ///
    /// Only the online case is touched: with neither a cloud nor an AI candidate present the
    /// Engine already produces the fourth line, so there is nothing to rearrange and nothing to
    /// risk.
    fn normalize_online_slots(&mut self) {
        const CLOUD: u8 = 2;
        const AI: u8 = 3;
        const ENGLISH: u8 = 4;
        const EMOJI: u8 = 6;
        const KAOMOJI: u8 = 7;

        let snapshot = &self.cached;
        let count = snapshot.candidates.len();
        if count < 2
            || snapshot.candidate_sources.len() != count
            || snapshot.candidate_codes.len() != count
            || snapshot.candidate_annotations.len() != count
            || snapshot.candidate_positions.len() != count
            || snapshot.candidate_corrected.len() != count
            || snapshot.candidate_answers_key.len() != count
        {
            return;
        }
        if !snapshot
            .candidate_sources
            .iter()
            .any(|source| *source == CLOUD || *source == AI)
        {
            return;
        }

        let (mut locals, mut cloud, mut ai, mut english, mut emoji, mut kaomoji) = (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        for (index, source) in snapshot.candidate_sources.iter().enumerate() {
            match *source {
                CLOUD => cloud.push(index),
                AI => ai.push(index),
                ENGLISH => english.push(index),
                EMOJI => emoji.push(index),
                KAOMOJI => kaomoji.push(index),
                _ => locals.push(index),
            }
        }

        // A provider may answer with several candidates - the AI limit reaches ten - and they take
        // their seat as a group. The reference has only one of each to place and silently drops the
        // rest; dropping a candidate the user was offered is not an option here.
        //
        // The reference then moves an English candidate whose learned weight is the unique maximum of the whole list to the first seat, which is how a pinned or promoted English word comes before the Chinese candidates. The snapshot carries no weights, but the Engine applies that same rule before this step and otherwise never puts English first while a Chinese candidate exists, so an English candidate at index zero with locals present is the promoted one. It keeps the first seat and the leading English seat is not filled a second time, exactly as the reference's move to index zero leaves it.
        let promoted_english = english.first() == Some(&0) && !locals.is_empty();
        let mut order = Vec::with_capacity(count);
        let mut english = english.into_iter();
        if promoted_english {
            order.extend(english.next());
        }
        // The hiragana/katakana pair of a single complete kana keeps seats 1 and 2 ahead of every online candidate, as the reference's `preserve_single_kana_pair` does (server/src/ipc/event_listener.cpp); the reading is the converted kana, so one character in U+3041..U+3096 is its `IsSingleKanaConversion`.
        const JAPANESE_ROMAJI: u8 = 3;
        let mut reading = snapshot.reading.chars();
        let single_kana = snapshot.scheme == JAPANESE_ROMAJI
            && matches!((reading.next(), reading.next()), (Some(kana), None) if ('\u{3041}'..='\u{3096}').contains(&kana));
        let local_prefix = if single_kana { 2 } else { 1 };
        let mut locals = locals.into_iter();
        order.extend(locals.by_ref().take(local_prefix));
        if !cloud.is_empty() {
            order.append(&mut cloud);
            order.append(&mut ai);
        }
        if !promoted_english {
            order.extend(english.next());
        }
        order.append(&mut ai);
        let mut emoji = emoji.into_iter();
        let mut kaomoji = kaomoji.into_iter();
        order.extend(emoji.next());
        order.extend(kaomoji.next());
        order.extend(locals);
        order.extend(english);
        order.extend(emoji);
        order.extend(kaomoji);
        // An English candidate the user fixed to a seat goes back to that seat after the seating, so a cloud or AI reply does not push it behind the online candidates (reference: server/src/ipc/candidate_selection_policy.h, the fixed-English pass at the end of NormalizeMixedCandidateOrder). Seats are 1-based and 0 means unfixed; a seat past the end clamps to the end, as the reference's `insert_at` does.
        let mut fixed_english = Vec::new();
        order.retain(|index| {
            let fixed = snapshot.candidate_sources[*index] == ENGLISH
                && snapshot.candidate_positions[*index] > 0;
            if fixed {
                fixed_english.push(*index);
            }
            !fixed
        });
        fixed_english.sort_by_key(|index| snapshot.candidate_positions[*index]);
        for index in fixed_english {
            let seat = usize::from(snapshot.candidate_positions[index] - 1).min(order.len());
            order.insert(seat, index);
        }
        // A permutation or nothing: a missing or repeated index would silently drop a candidate.
        debug_assert_eq!(order.len(), count);
        if order.len() != count {
            return;
        }
        if order.iter().enumerate().all(|(seat, index)| seat == *index) {
            return;
        }

        let snapshot = &mut self.cached;
        apply_order(&mut snapshot.candidates, &order);
        apply_order(&mut snapshot.candidate_codes, &order);
        apply_order(&mut snapshot.candidate_annotations, &order);
        apply_order(&mut snapshot.candidate_sources, &order);
        apply_order(&mut snapshot.candidate_positions, &order);
        apply_order(&mut snapshot.candidate_corrected, &order);
        apply_order(&mut snapshot.candidate_answers_key, &order);
        if self.engine_order.len() == count {
            apply_order(&mut self.engine_order, &order);
        }
    }

    fn rerank(&mut self) {
        let Some(reranker) = self.reranker.as_mut() else {
            return;
        };
        let snapshot = &self.cached;
        let count = snapshot.candidates.len();
        if count < 2
            || snapshot.candidate_sources.len() != count
            || snapshot.candidate_codes.len() != count
            || snapshot.candidate_annotations.len() != count
            || snapshot.candidate_positions.len() != count
            || snapshot.candidate_corrected.len() != count
            || snapshot.candidate_answers_key.len() != count
        {
            return;
        }
        let texts: Vec<&str> = snapshot.candidates.iter().map(String::as_str).collect();
        // A dictionary hit earns the model's deference because it carries corpus frequency for the
        // key the user typed. That premise fails the moment the engine offers a correction of that
        // key: the frequency then belongs to the letters that arrived rather than to the word they
        // were aiming at, and the list holds both readings. So the whole list loses the exemption,
        // not the corrected rows — the row that would wrongly win is the uncorrected one.
        //
        // With correction off, or with nothing corrected, this is exactly the previous behaviour,
        // which is what the 2052-case dictionary measurement was taken on.
        let corrected_key = snapshot
            .candidate_corrected
            .iter()
            .any(|&corrected| corrected);
        let Some(promote) = reranker.best_where(&self.ai_context, &texts, |index| CandidateFacts {
            answers_key: snapshot.candidate_answers_key[index],
            trusted_dictionary_hit: DICTIONARY_SOURCES.contains(&snapshot.candidate_sources[index])
                && !corrected_key,
        }) else {
            return;
        };
        let snapshot = &mut self.cached;
        rotate_to_front(&mut snapshot.candidates, promote);
        rotate_to_front(&mut snapshot.candidate_codes, promote);
        rotate_to_front(&mut snapshot.candidate_annotations, promote);
        rotate_to_front(&mut snapshot.candidate_sources, promote);
        rotate_to_front(&mut snapshot.candidate_positions, promote);
        rotate_to_front(&mut snapshot.candidate_corrected, promote);
        rotate_to_front(&mut snapshot.candidate_answers_key, promote);
        if self.engine_order.len() == count {
            rotate_to_front(&mut self.engine_order, promote);
        }
    }

    /// Move the runner-up sentence readings behind the rest of the list.
    ///
    /// The lattice searches several readings of the whole key so that something can choose between
    /// them. Leaving all of them at the front fills the candidate page with near-duplicate
    /// sentences and pushes the short candidates a user actually wants off it, which is why the
    /// search used to be pinned to a single path.
    ///
    /// They are moved rather than removed. A candidate page needs its *first* row to be the chosen
    /// reading; it does not need the others gone. Deleting them threw away the model's second and
    /// third choices, so a reading the model ranked third was unreachable even when it was right.
    ///
    /// Only lattice readings are touched. An earlier version of this keyed on "any source that is
    /// not a dictionary", which is wrong twice over: a source number says which code produced a
    /// candidate, not that two candidates are spellings of one answer, and most of the other
    /// sources are plural by design — English words, emoji, kaomoji, quick phrases and AI
    /// suggestions all arrive as lists, and that version silently dropped all but one of each.
    fn demote_runner_up_readings(&mut self) {
        // The lattice never runs on fewer than three syllables, so a shorter candidate reached the
        // list some other way and is not a reading of the same sentence. Japanese kana are the case
        // that proves it: あ and ア are both Generated and both one character.
        const SENTENCE_SYLLABLES: usize = 3;

        let snapshot = &self.cached;
        let count = snapshot.candidates.len();
        if count < 2 || snapshot.candidate_sources.len() != count {
            return;
        }
        let Some(width) = snapshot
            .candidates
            .iter()
            .zip(&snapshot.candidate_sources)
            .find(|(_, source)| **source == LATTICE_SOURCE)
            .map(|(text, _)| text.chars().count())
        else {
            return;
        };
        if width < SENTENCE_SYLLABLES {
            return;
        }
        // Everything after the first lattice reading of the full key is a runner-up.
        let mut kept_one = false;
        let mut demote: Vec<bool> = Vec::with_capacity(count);
        for (text, source) in snapshot.candidates.iter().zip(&snapshot.candidate_sources) {
            let reading = *source == LATTICE_SOURCE && text.chars().count() == width;
            demote.push(reading && kept_one);
            kept_one |= reading;
        }
        if !demote.iter().any(|moved| *moved) {
            return;
        }
        let snapshot = &mut self.cached;
        move_to_back(&mut snapshot.candidates, &demote);
        move_to_back(&mut snapshot.candidate_codes, &demote);
        move_to_back(&mut snapshot.candidate_annotations, &demote);
        move_to_back(&mut snapshot.candidate_sources, &demote);
        move_to_back(&mut snapshot.candidate_positions, &demote);
        move_to_back(&mut snapshot.candidate_corrected, &demote);
        move_to_back(&mut snapshot.candidate_answers_key, &demote);
        if self.engine_order.len() == count {
            move_to_back(&mut self.engine_order, &demote);
        }
    }

    pub(crate) fn refresh(&mut self) -> Result<(), RuntimeError> {
        self.snapshot_valid = false;
        self.translations.clear();
        // Drop cached candidate identities even if fetching the replacement fails.
        let previous = std::mem::replace(
            &mut self.cached,
            EngineSnapshot {
                scheme: 255,
                nine_key: false,
                nine_key_spellings: Vec::new(),
                candidate_annotations: Vec::new(),
                candidate_codes: Vec::new(),
                candidate_sources: Vec::new(),
                candidate_positions: Vec::new(),
                candidate_corrected: Vec::new(),
                candidate_answers_key: Vec::new(),
                microsoft_shuangpin: false,
                shuangpin_profile: String::new(),
                answered_by_pinyin_fallback: true,
                wubi_unique_four_code: false,
                local_mode: "unknown".into(),
                dedicated_english: false,
                preedit: String::new(),
                reading: String::new(),
                editing_text: String::new(),
                caret_position: 0,
                segment_raw_boundaries: vec![],
                candidates: Vec::new(),
            },
        );
        self.engine_order.clear();
        let previous_highlight = self.highlighted;
        self.highlighted = 0;
        let snapshot = self.engine.snapshot()?;
        self.load_snapshot(snapshot);
        self.rerank();
        self.demote_runner_up_readings();
        self.normalize_online_slots();
        self.snapshot_valid = true;
        if self.cached.editing_text == previous.editing_text
            && self.cached.scheme == previous.scheme
            && self.cached.local_mode == previous.local_mode
            && self.cached.reading == previous.reading
            && self.cached.dedicated_english == previous.dedicated_english
            && self.cached.candidates == previous.candidates
            && self.cached.candidate_codes == previous.candidate_codes
            && self.cached.candidate_annotations == previous.candidate_annotations
            && self.cached.candidate_sources == previous.candidate_sources
            && self.cached.candidate_positions == previous.candidate_positions
            && self.cached.candidate_corrected == previous.candidate_corrected
            && self.cached.candidate_answers_key == previous.candidate_answers_key
        {
            self.highlighted =
                previous_highlight.min(self.cached.candidates.len().saturating_sub(1));
        }
        Ok(())
    }

    pub fn focus(&mut self, focused: bool) -> Result<Transition, RuntimeError> {
        self.advance()?;
        // Invalidate the client before cancellation, including on engine failure.
        self.focused = false;
        let result = self.engine.command(Command::Cancel);
        self.refresh()?;
        let mut result = result?;
        // Leaving the client cancels the composition, but a phrase piece being held back is text
        // the user chose and, before it was held back, would already be in the document. Send it.
        self.hold_phrase_progress(false, false, false, "", &mut result);
        self.focused = focused;
        // A different client is a different sentence, so context never leaks
        // from one application into another.
        self.ai_context.clear();
        Ok(self.transition(result))
    }

    fn punctuation(&mut self, value: u8) -> Result<EngineResult, RuntimeError> {
        // Finish through Engine with the host highlight BEFORE asking it to translate.
        // Calling Engine punctuation on an active composition would choose candidate zero.
        let mut finished = self.engine.finish(self.engine_index(self.highlighted))?;
        let punctuation = match self.engine.punctuation(value) {
            Ok(result) => result,
            Err(error) if finished.has_commit => {
                // Completion already changed Engine state: never discard that commit.
                finished.commit.push(char::from(value));
                finished.handled = true;
                finished.diagnostic =
                    format!("{} Punctuation failed: {error}", finished.diagnostic)
                        .trim()
                        .to_owned();
                return Ok(finished);
            }
            Err(error) => return Err(error),
        };
        if !finished.has_commit {
            return Ok(punctuation);
        }
        finished.handled = true;
        if punctuation.has_commit {
            finished.commit.push_str(&punctuation.commit);
        } else if !punctuation.handled {
            // ASCII mode/unsupported symbols still terminate composition in one commit.
            finished.commit.push(char::from(value));
        }
        if !punctuation.diagnostic.is_empty() {
            finished.diagnostic = format!("{} {}", finished.diagnostic, punctuation.diagnostic)
                .trim()
                .to_owned();
        }
        Ok(finished)
    }

    fn punctuation_ascii(&mut self, value: u8) -> Result<EngineResult, RuntimeError> {
        // Keep the same highlighted-candidate completion semantics as normal
        // punctuation, but do not ask Engine to translate the trailing mark.
        // The Linux host has already applied its surrounding-text policy.
        let mut finished = self.engine.finish(self.engine_index(self.highlighted))?;
        if !finished.has_commit {
            return Ok(finished);
        }
        finished.handled = true;
        finished.commit.push(char::from(value));
        Ok(finished)
    }

    pub fn dispatch(&mut self, action: Action) -> Result<Transition, RuntimeError> {
        if matches!(&action, Action::Punctuation(value) | Action::PunctuationAscii(value) if !value.is_ascii_punctuation())
        {
            return Err(RuntimeError::InvalidPunctuation);
        }
        // Cache maintenance belongs to the session, including while its host
        // has no focus. Ordinary input must still pass through unchanged.
        if !self.focused && !matches!(&action, Action::ResetCache) {
            return Ok(self.transition(empty_result(false)));
        }
        if let Action::SelectAnyCandidate(id) = &action {
            if id.session != self.session
                || id.generation != self.generation
                || id.index >= self.cached.candidates.len()
            {
                return Err(RuntimeError::StaleCandidate);
            }
        }
        if let Action::Select(id)
        | Action::SelectEdge(id, _)
        | Action::PinCandidate(id)
        | Action::RemoveCandidate(id)
        | Action::FixCandidatePosition(id, _)
        | Action::ClearCandidatePosition(id) = &action
        {
            let start = (self.highlighted / self.page_size) * self.page_size;
            if id.session != self.session
                || id.generation != self.generation
                || id.index < start
                || id.index >= (start + self.page_size).min(self.cached.candidates.len())
            {
                return Err(RuntimeError::StaleCandidate);
            }
        }
        if let Action::ChooseNineKeySpelling(id) = &action {
            if id.session != self.session
                || id.generation != self.generation
                || !self.cached.nine_key
                || id.index >= self.cached.nine_key_spellings.len()
            {
                return Err(RuntimeError::StaleNineKeySpelling);
            }
        }
        self.advance()?;
        let filled_current_page =
            matches!(action, Action::NextPage) && self.expand_for_next_page()?;
        if matches!(action, Action::NextCandidate) {
            self.expand_for_next_candidate()?;
        }
        // The last candidate means the last one there is. The Engine caps what it returns to a
        // single-letter query and hands the rest over on request, so without this End would stop at
        // the end of what happened to be cached and move again the next time it was pressed.
        if matches!(action, Action::LastCandidate) {
            self.expand_cached_candidates()?;
        }
        let len = self.cached.candidates.len();
        let next_highlight = match &action {
            // Staying keeps the highlight exactly where it was: the page did not change, it only
            // stopped being short.
            Action::NextPage if filled_current_page => Some(self.highlighted),
            Action::NextPage if len > 0 => Some(
                (self.highlighted / self.page_size + 1).min((len - 1) / self.page_size)
                    * self.page_size,
            ),
            Action::PreviousPage if len > 0 => {
                Some((self.highlighted / self.page_size).saturating_sub(1) * self.page_size)
            }
            Action::NextCandidate if len > 0 => Some((self.highlighted + 1).min(len - 1)),
            Action::PreviousCandidate if len > 0 => Some(self.highlighted.saturating_sub(1)),
            // The ends of the list, not the ends of the page. The reference's Home and End are
            // FUNCTION_MOVE_PAGE_TOP and FUNCTION_MOVE_PAGE_BOTTOM, and its presenter answers both
            // with SetSelection - index 0, or -1 read as Count() - 1 - which then pulls the page
            // along to wherever that candidate sits. Every host here routes its own Home and End to
            // this action, so all four used to stop at the edges of the page the user was already
            // looking at, which is a keystroke that does almost nothing.
            Action::FirstCandidate if len > 0 => Some(0),
            Action::LastCandidate if len > 0 => Some(len - 1),
            _ => None,
        };
        if let Some(index) = next_highlight {
            self.highlighted = index;
            return Ok(self.transition(empty_result(true)));
        }
        let commit_context = OutputContext {
            scheme: self.cached.scheme,
            local_mode: self.cached.local_mode.clone(),
        };
        // Going back into the phrase, before the Engine sees the key: both rules replace what the
        // key would otherwise do.
        if let Some(transition) = self.retreat_phrase_selection(&action)? {
            return Ok(transition);
        }
        // What the reading held before the Engine saw this key. A selection that consumes part of
        // it has to record the piece it took, and only the difference says what that was.
        let reading_before = self.cached.editing_text.clone();
        // A digit on the candidate page picks a candidate; the Engine is asked the same question as
        // for Select, so it can begin a phrase the same way.
        let mut selected_by_digit = false;
        let character_action = matches!(action, Action::Character { .. });
        // Wubi top-commit (顶字): a letter typed after a complete four-letter code the Wubi table
        // answered, unique or not, commits the first candidate and starts the next composition
        // with that letter. The Engine caps a native Wubi code at four letters and would drop the
        // fifth, so without this the user loses the key they typed. It is judged on the reading
        // before the key, with the caret at its end: a caret moved back into the code is an edit
        // of the code, not the start of the next character. A held phrase stays open, matching
        // the reference's creating-word guard.
        let wubi_top_commit = matches!(action, Action::Character { value, .. } if value.is_ascii_alphabetic())
            && self.snapshot_valid
            && self.phrase_prefix.is_empty()
            && wubi_four_code_is_complete(&self.cached);
        let result = match action {
            Action::ResetCache => {
                self.engine.reset_cache()?;
                Ok(EngineResult {
                    handled: true,
                    has_commit: false,
                    commit: String::new(),
                    diagnostic: String::new(),
                })
            }
            Action::Punctuation(value) => self.punctuation(value),
            Action::PunctuationAscii(value) => self.punctuation_ascii(value),
            Action::Finish => self.engine.finish(self.engine_index(self.highlighted)),
            Action::Character { value, shift } if wubi_top_commit => self
                .engine
                .select(self.engine_index(0))
                .and_then(|committed| {
                    self.engine.character(value, shift)?;
                    Ok(committed)
                }),
            Action::Character { value, shift } => {
                self.engine.character(value, shift).and_then(|result| {
                    // The nine-key separator is a layout action, not Chinese quote punctuation.
                    if !result.handled && self.cached.nine_key && value == b'\'' {
                        return Ok(result);
                    }
                    if !result.handled && value.is_ascii_punctuation() {
                        return self.punctuation(value);
                    }
                    // Let Engine consume numeric input (Unicode mode, nine-key, etc.) first.
                    if result.handled
                        || self.cached.nine_key
                        || !(b'1'..=b'9').contains(&value)
                        || len == 0
                    {
                        return Ok(result);
                    }
                    let page_start = (self.highlighted / self.page_size) * self.page_size;
                    let slot = usize::from(value - b'1');
                    if slot >= self.page_size || page_start + slot >= len {
                        return Ok(empty_result(true));
                    }
                    selected_by_digit = true;
                    self.engine.select(self.engine_index(page_start + slot))
                })
            }
            Action::Command(command) => self.engine.command(command),
            Action::SegmentBackspace => self.engine.segment_command(SegmentCommand::Backspace),
            Action::SegmentMoveLeft => self.engine.segment_command(SegmentCommand::MoveLeft),
            Action::SegmentMoveRight => self.engine.segment_command(SegmentCommand::MoveRight),
            Action::Select(id) => self.engine.select(self.engine_index(id.index)),
            Action::SelectAnyCandidate(id) => self.engine.select(self.engine_index(id.index)),
            Action::SelectEdge(id, edge) => {
                self.engine.select_edge(self.engine_index(id.index), edge)
            }
            Action::PinCandidate(id) => self.engine.pin_candidate(self.engine_index(id.index)),
            Action::RemoveCandidate(id) => {
                self.engine.remove_candidate(self.engine_index(id.index))
            }
            Action::FixCandidatePosition(id, position) => {
                if !(1..=5).contains(&position) {
                    return Err(RuntimeError::Engine(
                        "Candidate position must be between 1 and 5".into(),
                    ));
                }
                self.engine
                    .fix_candidate_position(self.engine_index(id.index), position)
            }
            Action::ClearCandidatePosition(id) => self
                .engine
                .clear_candidate_position(self.engine_index(id.index)),
            Action::ChooseNineKeySpelling(id) => self.engine.choose_nine_key_spelling(id.index),
            Action::SelectHighlighted if len > 0 => {
                self.engine.select(self.engine_index(self.highlighted))
            }
            Action::SelectHighlighted => self.engine.command(Command::CommitCandidate),
            _ => return Ok(self.transition(empty_result(false))),
        };
        let refresh = self.refresh();
        let mut result = result?;
        if let Err(error) = refresh {
            // A successful engine commit must survive a presentation refresh failure.
            result.diagnostic = format!("Candidate refresh failed: {error}");
        }
        // The Engine owns the definition of a complete, native, unique Wubi code. Every host gets
        // the same fourth-key behavior here; platform adapters only decide how that commit crosses
        // their native composition boundary. A held phrase is still being assembled and must stay
        // open, matching the reference's creating-word guard.
        if character_action
            && self.snapshot_valid
            && self.cached.wubi_unique_four_code
            && self.phrase_prefix.is_empty()
        {
            result = self.engine.select(self.engine_index(0))?;
            if let Err(error) = self.refresh() {
                result.diagnostic = format!("Candidate refresh failed: {error}");
            }
        }
        // The Engine takes what it used off the front of the reading, so what is gone from the
        // front is what the selection consumed. A reading that did not simply shrink - a special
        // mode rewriting it, a fallback replacing it - leaves nothing to restore, and that
        // selection is recorded as unretractable rather than guessed at.
        let consumed = reading_before
            .strip_suffix(self.cached.editing_text.as_str())
            .unwrap_or("")
            .to_owned();
        let picked = selected_by_digit
            || matches!(
                action,
                Action::Select(_)
                    | Action::SelectAnyCandidate(_)
                    | Action::SelectEdge(..)
                    | Action::SelectHighlighted
            );
        // Escape throws the whole composition away, the chosen pieces with it - the reference's
        // _HandleCancel clears `word_for_creating_word` in the same breath.
        let discarded = matches!(action, Action::Command(Command::Cancel));
        // Segment editing on an emptied reading leaves the held phrase for the next segment key or Backspace, instead of sending it to the document (the reference's `keep_creating_word_after_empty_raw`).
        let keep_empty = matches!(
            action,
            Action::SegmentBackspace | Action::SegmentMoveLeft | Action::SegmentMoveRight
        );
        self.hold_phrase_progress(picked, discarded, keep_empty, &consumed, &mut result);
        let mut transition = self.transition(result);
        if transition.commit.is_some() {
            transition.commit_context = Some(commit_context);
        }
        Ok(transition)
    }
}

pub(crate) fn empty_result(handled: bool) -> EngineResult {
    EngineResult {
        handled,
        has_commit: false,
        commit: String::new(),
        diagnostic: String::new(),
    }
}

/// The reference Engine's `wubi_four_code_is_complete`, read off the snapshot this Engine already
/// publishes: the guards of `wubi_unique_four_code` without the candidate count. The code is a
/// native Wubi one (not dedicated English, no local mode, not answered by the pinyin fallback), it
/// is exactly the four letters the Wubi scheme caps a table-answered code at, the caret is at its
/// end, and there is a candidate to commit: a four-letter spelling no row matched was not answered
/// by the table, and committing nothing would still drop the key.
pub(crate) fn wubi_four_code_is_complete(snapshot: &EngineSnapshot) -> bool {
    const WUBI_COMPLETE_CODE_LENGTH: usize = 4;
    snapshot.scheme == 2
        && !snapshot.dedicated_english
        && snapshot.local_mode == "none"
        && !snapshot.nine_key
        && !snapshot.answered_by_pinyin_fallback
        && snapshot.editing_text.len() == WUBI_COMPLETE_CODE_LENGTH
        && snapshot
            .editing_text
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic())
        && snapshot.caret_position == WUBI_COMPLETE_CODE_LENGTH
        && !snapshot.candidates.is_empty()
}
