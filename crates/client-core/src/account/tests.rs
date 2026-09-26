//! Unit tests for the parent module, in their own file because the module
//! is large enough that mixing them with the implementation obscured both.
//! Same `mod tests` as before, so `use super::*` still names the parent.

use super::*;
use std::collections::HashMap;
use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

fn token(byte: u8) -> String {
    std::iter::repeat_n(char::from(byte), 64).collect()
}

fn user() -> AccountUser {
    AccountUser {
        id: "fixture-user".into(),
        display_name: "Fixture".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
    }
}

fn tokens(access: u8, refresh: u8, expires_in: u64) -> AccountTokens {
    AccountTokens {
        access_token: token(access),
        refresh_token: token(refresh),
        token_type: "Bearer".into(),
        expires_in,
        user: user(),
    }
}

#[test]
fn validates_clipboard_boundaries() {
    let valid_id = "0123456789abcdef".repeat(4);
    assert!(validate_clipboard_id(&valid_id).is_ok());
    assert!(validate_clipboard_id(&valid_id.to_uppercase()).is_err());
    assert!(validate_clipboard_id(&format!("{valid_id}0")).is_err());

    assert!(validate_clipboard_search(&"a".repeat(1024)).is_ok());
    assert!(validate_clipboard_search(&"a".repeat(1025)).is_err());
    assert!(validate_clipboard_search("safe\u{7f}query").is_err());

    let valid_text = "😀".repeat(2000);
    assert!(validate_clipboard_text(&valid_text).is_ok());
    assert!(validate_clipboard_text(&format!("{valid_text}😀")).is_err());
    assert!(validate_clipboard_text("\n\r\t").is_err());

    let item = || AccountClipboardItem {
        id: valid_id.clone(),
        text: "fixture clipboard text".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    };
    assert!(validate_clipboard_page(&AccountClipboardPage {
        enabled: true,
        items: vec![item(); 50],
    })
    .is_ok());
    let mut too_many = vec![item(); 50];
    too_many.push(item());
    assert!(validate_clipboard_page(&AccountClipboardPage {
        enabled: true,
        items: too_many,
    })
    .is_err());
}

#[test]
fn validates_chat_catalog_and_request_boundaries() {
    let models = AccountChatModels {
        data: vec![
            AccountChatModel {
                id: "fixture-chat".into(),
            },
            AccountChatModel {
                id: "fixture-fast".into(),
            },
        ],
        default_model: "fixture-chat".into(),
    };
    assert!(validate_chat_models(&models).is_ok());

    let messages = vec![AccountChatMessage {
        role: "user".into(),
        content: "fixture message".into(),
    }];
    assert!(validate_chat_request(&messages, "fixture-chat").is_ok());
    assert!(validate_chat_request(&messages, "").is_err());
    assert!(validate_chat_request(
        &[AccountChatMessage {
            role: "tool".into(),
            content: "x".into(),
        }],
        "fixture-chat",
    )
    .is_err());
    assert!(validate_chat_request(
        &[AccountChatMessage {
            role: "user".into(),
            content: "\n".into(),
        }],
        "fixture-chat",
    )
    .is_err());
    let chat = |content: &str| {
        validate_chat_request(
            &[AccountChatMessage {
                role: "user".into(),
                content: content.into(),
            }],
            "fixture-chat",
        )
    };
    assert!(chat("第一行\n第二行").is_ok());
    assert!(chat("a\r\n\tb").is_ok());
    for rejected in ["  \n", "a\u{0}b", "a\u{1b}[31mb"] {
        assert!(chat(rejected).is_err(), "{rejected:?}");
    }
    assert!(!chat_text_has_disallowed_control("第一段\n\n第二段"));
    assert!(chat_text_has_disallowed_control("a\u{7f}"));

    let mut duplicate = models.clone();
    duplicate.data.push(AccountChatModel {
        id: "fixture-chat".into(),
    });
    assert!(validate_chat_models(&duplicate).is_err());
}

#[test]
fn validates_dictionary_boundaries() {
    let valid_id = "0123456789abcdef".repeat(4);
    assert_eq!(
        dictionary_path(DictionaryKind::Pinyin, 2, "ni hao").unwrap(),
        "/v1/users/me/dictionaries/pinyin?q=ni%20hao&offset=2&limit=100"
    );
    assert_eq!(
        dictionary_catalog_path(DictionaryKind::Pinyin, "nihc", 0, "shuangpin", "xiaohe")
            .unwrap(),
        "/v1/users/me/dictionaries/pinyin/catalog?q=nihc&offset=0&limit=100&scheme=shuangpin&profile=xiaohe"
    );
    assert!(dictionary_path(DictionaryKind::Wubi, 1_000_001, "").is_err());
    assert!(dictionary_catalog_path(DictionaryKind::Pinyin, "", 1_000_001, "pinyin", "x").is_err());
    assert!(validate_dictionary_catalog_query("", 0, "", "x").is_err());
    assert!(validate_dictionary_id(&valid_id).is_ok());
    assert!(validate_dictionary_id(&valid_id.to_uppercase()).is_err());

    assert!(validate_dictionary_value(DictionaryKind::Pinyin, "ni' hao", "你好", 1).is_ok());
    assert!(validate_dictionary_value(DictionaryKind::Wubi, "abcd", "字", 0).is_ok());
    assert!(validate_dictionary_value(DictionaryKind::Wubi, "abcde", "字", 0).is_err());
    assert!(validate_dictionary_value(DictionaryKind::Quick, "k2", &"字".repeat(199), 1).is_ok());
    assert!(validate_dictionary_value(DictionaryKind::Quick, "k2", &"字".repeat(200), 1).is_err());
    // Inbound rows tolerate a digit in a quick phrase code; a value this client writes does not.
    assert!(validate_new_dictionary_value(DictionaryKind::Quick, "k2", "字", 1).is_err());
    assert!(validate_new_dictionary_value(DictionaryKind::Quick, "kk", "字", 1).is_ok());
    assert!(validate_dictionary_value(DictionaryKind::English, "hello", "word", 1).is_ok());
    assert!(validate_dictionary_value(DictionaryKind::English, "hello1", "word", 1).is_err());
    assert!(validate_dictionary_import(DictionaryKind::Pinyin, "hans", "你好").is_ok());
    assert!(validate_dictionary_import(DictionaryKind::Wubi, "hans", "你好").is_err());
    assert!(validate_dictionary_import(DictionaryKind::Pinyin, "standard", "bad\u{0001}").is_err());

    let entry = || AccountDictionaryEntry {
        id: valid_id.clone(),
        kind: DictionaryKind::Pinyin,
        code: "ni".into(),
        word: "你".into(),
        weight: 1,
        revision: 2,
    };
    assert!(validate_dictionary_page(
        &AccountDictionaryPage {
            entries: vec![entry(); 100],
            has_more: true,
            offset: 0,
        },
        DictionaryKind::Pinyin
    )
    .is_ok());
    assert!(validate_dictionary_page(
        &AccountDictionaryPage {
            entries: vec![entry(); 101],
            has_more: true,
            offset: 0,
        },
        DictionaryKind::Pinyin
    )
    .is_err());
    assert!(validate_dictionary_catalog_page(
        &AccountDictionaryCatalogPage {
            entries: vec![AccountDictionaryCatalogEntry {
                kind: DictionaryKind::Pinyin,
                code: "ni".into(),
                word: "你".into(),
                weight: 1,
            }],
            has_more: false,
            offset: 0,
            revision: 2,
            normalized: "ni".into(),
        },
        DictionaryKind::Pinyin
    )
    .is_ok());
}

#[derive(Clone, Default)]
struct MemoryStorage(Arc<Mutex<Option<SavedAccountSession>>>);

impl AccountSessionStorage for MemoryStorage {
    fn load(&self) -> Result<Option<SavedAccountSession>, AccountError> {
        self.0
            .lock()
            .map(|value| value.clone())
            .map_err(|_| AccountError::Storage)
    }

    fn save(&self, session: &SavedAccountSession) -> Result<(), AccountError> {
        *self.0.lock().map_err(|_| AccountError::Storage)? = Some(session.clone());
        Ok(())
    }

    fn clear(&self) -> Result<(), AccountError> {
        *self.0.lock().map_err(|_| AccountError::Storage)? = None;
        Ok(())
    }
}

#[derive(Clone)]
struct FakeApi {
    refreshes: Arc<AtomicUsize>,
    reject_refresh: Arc<AtomicBool>,
    refresh_gate: Option<Arc<(Mutex<bool>, Condvar)>>,
}

impl FakeApi {
    fn new() -> Self {
        Self {
            refreshes: Arc::new(AtomicUsize::new(0)),
            reject_refresh: Arc::new(AtomicBool::new(false)),
            refresh_gate: None,
        }
    }
}

impl AccountApi for FakeApi {
    fn providers(&self) -> Result<HashMap<String, bool>, AccountError> {
        Ok(HashMap::from([("email".into(), true)]))
    }

    fn challenge(&self, _provider: &str, _target: &str) -> Result<AccountChallenge, AccountError> {
        Ok(AccountChallenge {
            challenge_id: "fixture-challenge".into(),
            expires_in: 300,
            nonce: None,
            authorization_url: None,
        })
    }

    fn login(&self, _challenge: &str, _credential: &str) -> Result<AccountTokens, AccountError> {
        Ok(tokens(b'a', b'b', 900))
    }

    fn refresh(&self, _refresh_token: &str) -> Result<AccountTokens, AccountError> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.refresh_gate {
            let (lock, ready) = &**gate;
            let mut open = lock.lock().map_err(|_| AccountError::Unavailable)?;
            while !*open {
                open = ready.wait(open).map_err(|_| AccountError::Unavailable)?;
            }
        }
        if self.reject_refresh.load(Ordering::SeqCst) {
            Err(AccountError::Unauthorized)
        } else {
            Ok(tokens(b'c', b'd', 900))
        }
    }

    fn profile(&self, _access_token: &str) -> Result<AccountProfile, AccountError> {
        Ok(AccountProfile {
            user: user(),
            identities: vec![AccountProfileIdentity {
                provider: "email".into(),
                subject: "masked-fixture".into(),
            }],
        })
    }

    fn rename(&self, _display_name: &str, _access_token: &str) -> Result<(), AccountError> {
        Ok(())
    }

    fn logout(&self, _access_token: &str, _all: bool) -> Result<(), AccountError> {
        Ok(())
    }

    fn delete_account(&self, _access_token: &str) -> Result<(), AccountError> {
        Ok(())
    }

    fn preference_schema(
        &self,
        access_token: &str,
    ) -> Result<AccountPreferenceSchema, AccountError> {
        if access_token == token(b'a') {
            return Err(AccountError::Unauthorized);
        }
        Ok(AccountPreferenceSchema {
            fields: BTreeMap::from([
                (
                    "input.schema".into(),
                    AccountPreferenceField {
                        value_type: "string".into(),
                    },
                ),
                (
                    "platform.ios.nine_key".into(),
                    AccountPreferenceField {
                        value_type: "boolean".into(),
                    },
                ),
            ]),
            maximum_bytes: 65_536,
            update_mode: "replace".into(),
            revision_required: true,
        })
    }

    fn preferences(&self, access_token: &str) -> Result<AccountPreferences, AccountError> {
        if access_token == token(b'a') {
            return Err(AccountError::Unauthorized);
        }
        Ok(AccountPreferences {
            revision: 42,
            settings: BTreeMap::from([
                (
                    "input.schema".into(),
                    AccountPreferenceValue::String("quanpin".into()),
                ),
                (
                    "platform.ios.nine_key".into(),
                    AccountPreferenceValue::Boolean(true),
                ),
            ]),
        })
    }

    fn put_preferences(
        &self,
        preferences: &AccountPreferences,
        access_token: &str,
    ) -> Result<AccountPreferences, AccountError> {
        if access_token == token(b'a') {
            return Err(AccountError::Unauthorized);
        }
        Ok(AccountPreferences {
            revision: preferences.revision + 1,
            settings: preferences.settings.clone(),
        })
    }
}

fn installed(storage: &MemoryStorage, expires_at_unix_ms: u64) {
    *storage.0.lock().unwrap() = Some(SavedAccountSession {
        tokens: tokens(b'a', b'b', 900),
        expires_at_unix_ms,
    });
}

#[test]
fn validates_public_inputs_and_tokens() {
    assert!(validate_identity(&AccountIdentity {
        user_id: "user-1".into()
    })
    .is_ok());
    assert!(validate_identity(&AccountIdentity {
        user_id: "bad\n".into()
    })
    .is_err());
    assert_eq!(
        validate_provider_target("email", " fixture@example.test"),
        Err(AccountError::Invalid)
    );
    assert_eq!(validate_provider_target("apple", ""), Ok(()));
    assert_eq!(
        validate_provider_target("apple", "unexpected-target"),
        Err(AccountError::Invalid)
    );
    assert_eq!(
        validate_login("challenge", "１２３４５６"),
        Err(AccountError::Invalid)
    );
    assert_eq!(
        validate_apple_login("challenge", "identity-token\n"),
        Err(AccountError::Invalid)
    );
    let mut invalid = tokens(b'a', b'b', 900);
    invalid.access_token = token(b'A');
    assert_eq!(validate_tokens(&invalid), Err(AccountError::Unavailable));
}

#[test]
fn account_preferences_validate_and_merge_preserves_other_platforms() {
    let base = AccountPreferences {
        revision: 42,
        settings: BTreeMap::from([
            (
                "input.schema".into(),
                AccountPreferenceValue::String("quanpin".into()),
            ),
            (
                "platform.ios.nine_key".into(),
                AccountPreferenceValue::Boolean(true),
            ),
        ]),
    };
    let schema = AccountPreferenceSchema {
        fields: BTreeMap::from([
            (
                "input.schema".into(),
                AccountPreferenceField {
                    value_type: "string".into(),
                },
            ),
            (
                "platform.android.nine_key".into(),
                AccountPreferenceField {
                    value_type: "boolean".into(),
                },
            ),
        ]),
        maximum_bytes: 65_536,
        update_mode: "replace".into(),
        revision_required: true,
    };
    let replacing = BTreeMap::from([
        (
            "input.schema".into(),
            AccountPreferenceValue::String("shuangpin".into()),
        ),
        (
            "platform.android.nine_key".into(),
            AccountPreferenceValue::Boolean(false),
        ),
    ]);
    let merged = merge_account_preferences(&base, &replacing, &schema).unwrap();
    assert_eq!(merged.revision, 42);
    assert_eq!(
        merged.settings["input.schema"],
        AccountPreferenceValue::String("shuangpin".into())
    );
    assert_eq!(
        merged.settings["platform.ios.nine_key"],
        AccountPreferenceValue::Boolean(true)
    );
    assert_eq!(
        merged.settings["platform.android.nine_key"],
        AccountPreferenceValue::Boolean(false)
    );
    assert_eq!(
        merge_account_preferences(
            &base,
            &BTreeMap::from([("input.schema".into(), AccountPreferenceValue::Boolean(true),)]),
            &schema
        ),
        Err(AccountError::Invalid)
    );
}

#[test]
fn account_preferences_keep_photo_sized_strings_within_the_negotiated_limit() {
    let photo = "A".repeat(4 * 512_000_usize.div_ceil(3));
    let design = format!(r#"{{"photo":"{photo}"}}"#);
    let key = "platform.harmony.custom_keyboard_skin";
    let base = AccountPreferences {
        revision: 1,
        settings: BTreeMap::new(),
    };
    let mut schema = AccountPreferenceSchema {
        fields: BTreeMap::from([(
            key.into(),
            AccountPreferenceField {
                value_type: "string".into(),
            },
        )]),
        maximum_bytes: MAX_JSON_BYTES,
        update_mode: "replace".into(),
        revision_required: true,
    };
    let replacing = BTreeMap::from([(key.into(), AccountPreferenceValue::String(design.clone()))]);

    let merged = merge_account_preferences(&base, &replacing, &schema).unwrap();
    assert_eq!(merged.settings[key], AccountPreferenceValue::String(design));

    schema.maximum_bytes = 65_536;
    assert_eq!(
        merge_account_preferences(&base, &replacing, &schema),
        Err(AccountError::Invalid)
    );
}

#[test]
fn account_preferences_refresh_after_unauthorized_and_preserve_revision_conflicts() {
    let storage = MemoryStorage::default();
    installed(&storage, u64::MAX);
    let api = FakeApi::new();
    let refreshes = Arc::clone(&api.refreshes);
    let session = BackendAccountSession::new(api, storage);
    let schema = session.preference_schema().unwrap();
    assert!(schema.fields.contains_key("input.schema"));
    let cloud = session.preferences().unwrap();
    assert_eq!(cloud.revision, 42);
    let updated = session
        .put_preferences(&cloud)
        .expect("refresh should make the write succeed");
    assert_eq!(updated.revision, 43);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        AccountError::from_status(StatusCode::CONFLICT),
        AccountError::Conflict
    );
    assert_eq!(AccountError::Conflict.code(), "account_conflict");
}

#[test]
fn refreshes_once_for_concurrent_callers() {
    let storage = MemoryStorage::default();
    installed(&storage, 0);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let mut api = FakeApi::new();
    api.refresh_gate = Some(Arc::clone(&gate));
    let count = Arc::clone(&api.refreshes);
    let session = Arc::new(BackendAccountSession::new(api, storage));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let session = Arc::clone(&session);
            thread::spawn(move || session.access_token(None))
        })
        .collect();
    while count.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    let (lock, ready) = &*gate;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    let values: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap())
        .collect();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(values.iter().all(|value| value == &token(b'c')));
}

#[test]
fn unauthorized_refresh_clears_storage() {
    let storage = MemoryStorage::default();
    installed(&storage, 0);
    let api = FakeApi::new();
    api.reject_refresh.store(true, Ordering::SeqCst);
    let session = BackendAccountSession::new(api, storage.clone());
    assert_eq!(session.access_token(None), Err(AccountError::Unauthorized));
    assert!(storage.load().unwrap().is_none());
    assert_eq!(session.status().unwrap(), None);
}

#[test]
fn late_refresh_cannot_restore_forgotten_session() {
    let storage = MemoryStorage::default();
    installed(&storage, 0);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let mut api = FakeApi::new();
    api.refresh_gate = Some(Arc::clone(&gate));
    let count = Arc::clone(&api.refreshes);
    let session = Arc::new(BackendAccountSession::new(api, storage.clone()));
    let worker = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.access_token(None))
    };
    while count.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    session.forget().unwrap();
    let (lock, ready) = &*gate;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    assert_eq!(worker.join().unwrap(), Err(AccountError::Cancelled));
    assert!(storage.load().unwrap().is_none());
}

#[test]
fn generation_exhaustion_refuses_async_account_operations() {
    let storage = MemoryStorage::default();
    installed(&storage, 0);
    let session = BackendAccountSession::new(FakeApi::new(), storage);
    session.set_generation_for_test(u64::MAX - 1);

    assert_eq!(
        session.sign_in("synthetic-challenge", "123456"),
        Err(AccountError::Unavailable)
    );
    session.set_generation_for_test(u64::MAX);
    assert_eq!(
        session.access_token(Some(&token(b'a'))),
        Err(AccountError::Unavailable)
    );
    session.forget().unwrap();
    assert_eq!(session.status().unwrap(), None);
}

#[test]
fn logout_clears_local_session_before_remote_result() {
    let storage = MemoryStorage::default();
    installed(&storage, u64::MAX);
    let session = BackendAccountSession::new(FakeApi::new(), storage.clone());
    session.logout(true).unwrap();
    assert!(storage.load().unwrap().is_none());
    assert_eq!(session.status().unwrap(), None);
}

fn serve_once(response: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        std::io::Write::write_all(&mut stream, &response).unwrap();
    });
    format!("http://{address}")
}

fn serve_once_and_capture(response: Vec<u8>) -> (String, Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let expected_bytes = loop {
            let read = stream.read(&mut buffer).unwrap();
            assert_ne!(read, 0, "request ended before its headers");
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = std::str::from_utf8(&request[..header_end]).unwrap();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length: ")
                        .or_else(|| line.strip_prefix("Content-Length: "))
                })
                .unwrap()
                .parse::<usize>()
                .unwrap();
            break header_end + 4 + content_length;
        };
        while request.len() < expected_bytes {
            let read = stream.read(&mut buffer).unwrap();
            assert_ne!(read, 0, "request ended before its body");
            request.extend_from_slice(&buffer[..read]);
        }
        sender.send(request).unwrap();
        std::io::Write::write_all(&mut stream, &response).unwrap();
    });
    (format!("http://{address}"), receiver)
}

#[test]
fn transport_rejects_redirects() {
    let origin = serve_once(
        b"HTTP/1.1 302 Found\r\nLocation: https://example.test/\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
    );
    let client = BackendAccountClient::loopback(&origin).unwrap();
    assert_eq!(client.providers(), Err(AccountError::Unavailable));
}

#[test]
fn transport_rejects_oversized_responses() {
    let body = vec![b'x'; MAX_JSON_BYTES + 1];
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n",
        body.len()
    );
    let mut response = header.into_bytes();
    response.extend(body);
    let origin = serve_once(response);
    let client = BackendAccountClient::loopback(&origin).unwrap();
    assert_eq!(client.providers(), Err(AccountError::Unavailable));
}

#[test]
fn restores_dictionary_snapshot_only_after_a_new_cloud_revision() {
    let body = serde_json::json!({ "revision": 8, "reset": true }).to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        body.len(),
        body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let result = client
        .restore_dictionary_snapshot(b"{\"type\":\"header\"}\n", 7, &token(b'a'))
        .unwrap();
    assert_eq!(
        result,
        AccountDictionarySnapshotRestore {
            revision: 8,
            reset: true
        }
    );

    let body = serde_json::json!({ "revision": 8, "reset": false }).to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        body.len(),
        body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    assert_eq!(
        client.restore_dictionary_snapshot(b"snapshot", 7, &token(b'a')),
        Err(AccountError::Unavailable)
    );
}

#[test]
fn streams_dictionary_snapshot_file_with_exact_body_and_media_type() {
    let body = serde_json::json!({ "revision": 8, "reset": true }).to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        body.len(),
        body
    );
    let (origin, request) = serve_once_and_capture(response.into_bytes());
    let directory = tempfile::tempdir().unwrap();
    let snapshot = directory.path().join("fixture.ndjson");
    let contents = b"{\"type\":\"header\"}\n{\"type\":\"footer\"}\n";
    std::fs::write(&snapshot, contents).unwrap();

    let client = BackendAccountClient::loopback(&origin).unwrap();
    assert_eq!(
        client
            .restore_dictionary_snapshot_file(&snapshot, 7, &token(b'a'))
            .unwrap(),
        AccountDictionarySnapshotRestore {
            revision: 8,
            reset: true,
        }
    );

    let request = request.recv().unwrap();
    let header_end = request
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap();
    let headers = std::str::from_utf8(&request[..header_end]).unwrap();
    assert!(headers.starts_with("PUT /v1/users/me/dictionary/snapshot?revision=7 HTTP/1.1\r\n"));
    assert!(headers.contains("content-type: application/x-ndjson\r\n"));
    assert!(headers.contains(&format!("content-length: {}\r\n", contents.len())));
    assert_eq!(&request[header_end + 4..], contents);
}

#[test]
fn dictionary_snapshot_file_restore_validates_file_and_revision_bounds() {
    let directory = tempfile::tempdir().unwrap();
    let empty = directory.path().join("empty.ndjson");
    std::fs::write(&empty, []).unwrap();
    let oversized = directory.path().join("oversized.ndjson");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(MAX_DICTIONARY_SNAPSHOT_BYTES as u64 + 1)
        .unwrap();
    let client = BackendAccountClient::loopback("http://127.0.0.1:9").unwrap();

    for path in [
        Path::new("relative.ndjson"),
        empty.as_path(),
        oversized.as_path(),
    ] {
        assert_eq!(
            client.restore_dictionary_snapshot_file(path, 7, &token(b'a')),
            Err(AccountError::Invalid)
        );
    }
    assert_eq!(
        client.restore_dictionary_snapshot_file(&empty, -1, &token(b'a')),
        Err(AccountError::Invalid)
    );
    assert_eq!(
        client.restore_dictionary_snapshot_file(&empty, 7, "short"),
        Err(AccountError::Invalid)
    );
}

#[test]
fn dictionary_snapshot_file_restore_rejects_nonadvancing_response() {
    let body = serde_json::json!({ "revision": 7, "reset": true }).to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        body.len(),
        body
    );
    let directory = tempfile::tempdir().unwrap();
    let snapshot = directory.path().join("fixture.ndjson");
    std::fs::write(&snapshot, b"fixture\n").unwrap();
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();

    assert_eq!(
        client.restore_dictionary_snapshot_file(&snapshot, 7, &token(b'a')),
        Err(AccountError::Unavailable)
    );
}

#[test]
fn dictionary_snapshot_restore_requires_json_response_media_type() {
    let body = serde_json::json!({ "revision": 8, "reset": true }).to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{}",
        body.len(),
        body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();

    assert_eq!(
        client.restore_dictionary_snapshot(b"fixture\n", 7, &token(b'a')),
        Err(AccountError::Unavailable)
    );
}

#[test]
fn account_dictionary_transport_maps_flattened_responses() {
    let id = "0123456789abcdef".repeat(4);
    let page_body = serde_json::json!({
        "entries": [{
            "id": id,
            "kind": "pinyin",
            "code": "ni",
            "word": "fixture",
            "weight": 1,
            "revision": 2
        }],
        "has_more": false,
        "offset": 0
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        page_body.len(),
        page_body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let page = client
        .dictionary(DictionaryKind::Pinyin, "fixture", 0, &token(b'a'))
        .unwrap();
    assert_eq!(page.entries[0].word, "fixture");
    assert_eq!(page.entries[0].revision, 2);

    let change_body = serde_json::json!({
        "revision": 3,
        "previous": null,
        "replacement": {
            "id": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
            "kind": "pinyin",
            "code": "ni",
            "word": "fixture",
            "weight": 1,
            "revision": 3
        }
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        change_body.len(),
        change_body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let change = client
        .add_dictionary(DictionaryKind::Pinyin, "ni", "fixture", 1, &token(b'a'))
        .unwrap();
    assert_eq!(change.revision, 3);
    assert_eq!(change.replacement.unwrap().id.len(), 64);

    let export = b"ni\tfixture\t1\n".to_vec();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n",
        export.len()
    )
    .into_bytes()
    .into_iter()
    .chain(export)
    .collect();
    let client = BackendAccountClient::loopback(&serve_once(response)).unwrap();
    let exported = client
        .export_dictionary(DictionaryKind::Pinyin, "standard", &token(b'a'))
        .unwrap();
    assert_eq!(exported.text, "ni\tfixture\t1\n");
    assert_eq!(exported.filename, "dictionary-pinyin.tsv");

    let catalog_body = serde_json::json!({
        "entries": [{
            "kind": "pinyin",
            "code": "ni'hao",
            "word": "你好",
            "weight": 100000
        }],
        "offset": 0,
        "has_more": false,
        "revision": 42,
        "normalized": "ni'hao"
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        catalog_body.len(),
        catalog_body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let catalog = client
        .dictionary_catalog(
            DictionaryKind::Pinyin,
            "nihc",
            0,
            "shuangpin",
            "xiaohe",
            &token(b'a'),
        )
        .unwrap();
    assert_eq!(catalog.revision, 42);
    assert_eq!(catalog.normalized, "ni'hao");

    let change_body = serde_json::json!({
        "revision": 43,
        "previous": null,
        "replacement": null
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        change_body.len(),
        change_body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let change = client
        .edit_dictionary_catalog(DictionaryKind::Pinyin, "ni", "你", 42, None, &token(b'a'))
        .unwrap();
    assert_eq!(change.revision, 43);

    let changes_body = serde_json::json!({
        "changes": [{
            "revision": 44,
            "previous": null,
            "replacement": null
        }],
        "next": 44,
        "has_more": false
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        changes_body.len(),
        changes_body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let changes = client.dictionary_changes(43, 1, &token(b'a')).unwrap();
    assert_eq!(changes.next, 44);
    assert!(!changes.has_more);

    let snapshot = b"{\"type\":\"header\"}\n".to_vec();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/x-ndjson\r\n\r\n",
        snapshot.len()
    )
    .into_bytes()
    .into_iter()
    .chain(snapshot)
    .collect();
    let client = BackendAccountClient::loopback(&serve_once(response)).unwrap();
    assert_eq!(
        client.dictionary_snapshot(&token(b'a')).unwrap(),
        b"{\"type\":\"header\"}\n"
    );

    let snapshot = b"streamed snapshot\n".to_vec();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/x-ndjson\r\n\r\n",
        snapshot.len()
    )
    .into_bytes()
    .into_iter()
    .chain(snapshot.clone())
    .collect();
    let client = BackendAccountClient::loopback(&serve_once(response)).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("snapshot.ndjson");
    let size = client
        .dictionary_snapshot_to_file(&destination, &token(b'a'))
        .unwrap();
    assert_eq!(size, snapshot.len() as u64);
    assert_eq!(std::fs::read(destination).unwrap(), snapshot);

    let invalid_changes = serde_json::json!({
        "changes": [{"revision": 44, "previous": null, "replacement": null}],
        "next": 43,
        "has_more": false
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        invalid_changes.len(),
        invalid_changes
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    assert_eq!(
        client.dictionary_changes(43, 1, &token(b'a')),
        Err(AccountError::Unavailable)
    );
    assert_eq!(
        client.dictionary_changes(-1, 1, &token(b'a')),
        Err(AccountError::Invalid)
    );
}

#[test]
fn validates_candidate_transport_boundaries() {
    let query = AccountCandidateQuery {
        text: "nihc".into(),
        kind: "pinyin".into(),
        scheme: "shuangpin".into(),
        profile: "xiaohe".into(),
        limit: 100,
    };
    assert!(validate_candidate_query(&query).is_ok());
    assert!(validate_candidate_query(&AccountCandidateQuery {
        text: "".into(),
        ..query.clone()
    })
    .is_err());
    assert!(validate_ranking_arguments(&query, 42, "pin", 1, 1).is_ok());
    assert!(validate_ranking_arguments(
        &AccountCandidateQuery {
            kind: "quick".into(),
            ..query.clone()
        },
        42,
        "pin",
        1,
        1
    )
    .is_err());
    assert!(validate_candidate_value(&query, "nihc", "你好").is_ok());
    assert!(validate_candidate_value(&query, "", "你好").is_err());
}

#[test]
fn account_candidate_transport_maps_canonical_and_fixed_state() {
    let candidate_body = serde_json::json!({
        "candidates": [{
            "code": "nihc",
            "word": "你好",
            "weight": 10,
            "canonical_pinyin": "ni'hao"
        }],
        "context": "server:context",
        "revision": 42
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        candidate_body.len(),
        candidate_body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let query = AccountCandidateQuery {
        text: "nihc".into(),
        kind: "pinyin".into(),
        scheme: "shuangpin".into(),
        profile: "xiaohe".into(),
        limit: 100,
    };
    let candidates = client.personal_candidates(&query, &token(b'a')).unwrap();
    assert_eq!(candidates.candidates[0].mutation_code(), "ni'hao");

    let ranking_body = serde_json::json!({
        "revision": 43,
        "changed": true,
        "selection": { "count": 0 }
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        ranking_body.len(),
        ranking_body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let ranking = client
        .rank_candidate(
            &query,
            "ni'hao",
            "你好",
            42,
            "pin",
            1,
            1,
            false,
            &token(b'a'),
        )
        .unwrap();
    assert!(ranking.changed);
    assert_eq!(ranking.revision, 43);

    let positions_body = serde_json::json!({
        "positions": [{
            "context": "server:context",
            "code": "ni'hao",
            "word": "你好",
            "position": 1
        }],
        "offset": 0,
        "has_more": false
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        positions_body.len(),
        positions_body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let positions = client
        .fixed_positions("server:context", 0, &token(b'a'))
        .unwrap();
    assert_eq!(positions.positions[0].position, 1);

    let revision_body = r#"{"revision":44}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
        revision_body.len(),
        revision_body
    );
    let client = BackendAccountClient::loopback(&serve_once(response.into_bytes())).unwrap();
    let revision = client
        .set_fixed_position("server:context", "ni'hao", "你好", None, 43, &token(b'a'))
        .unwrap();
    assert_eq!(revision.revision, 44);
}
