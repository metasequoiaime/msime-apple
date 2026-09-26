//! Unit tests for the parent module, in their own file because the module
//! is large enough that mixing them with the implementation obscured both.
//! Same `mod tests` as before, so `use super::*` still names the parent.

use super::runtime::empty_result;
use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
#[derive(Default)]
struct Fixture {
    scheme: u8,
    dedicated_english: bool,
    nine_key: bool,
    nine_key_spellings: Vec<String>,
    local_mode: String,
    words: Vec<String>,
    codes: Vec<String>,
    text: String,
    snapshot_fails: bool,
    balanced_openings: Vec<u8>,
    cache_resets: usize,
    /// Candidates this engine holds back until asked, standing in for the Engine's cap on a
    /// single-letter query. Empty means an engine that already returns everything it has.
    withheld: Vec<String>,
    /// Where each candidate came from, parallel to `words`. Empty means an engine answering from
    /// the local dictionary alone, which is what most of these tests are about.
    sources: Vec<u8>,
    /// What is left to compose after a candidate is picked, standing in for an Engine that answered
    /// with a candidate covering only part of the input. `None` is an engine that finishes.
    remaining_after_select: Option<String>,
    /// The seat each candidate is fixed to, parallel to `words`: 1-based, 0 for unfixed. Empty means nothing is fixed.
    positions: Vec<u8>,
    /// What the Engine reports as the reading, which the Japanese scheme sets to the converted kana. Empty means no reading.
    reading: String,
}

#[cfg(unix)]
fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

#[cfg(unix)]
#[test]
fn provider_connect_rejects_untrusted_filesystem_endpoints() {
    let root = private_tempdir();
    let socket = root.path().join("provider.sock");
    std::fs::write(&socket, b"synthetic").unwrap();
    assert!(UnixSocketProvider::new(&socket).connect().is_none());
    std::fs::remove_file(&socket).unwrap();

    let target = root.path().join("target.sock");
    let listener = UnixListener::bind(&target).unwrap();
    let alias = root.path().join("alias.sock");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    assert!(UnixSocketProvider::new(&alias).connect().is_none());
    drop(listener);

    let socket = root.path().join("private.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(UnixSocketProvider::new(&socket).connect().is_none());
    drop(listener);
}

#[cfg(unix)]
#[test]
fn cloud_dictionary_provider_forwards_bounded_request() {
    let directory = private_tempdir();
    let socket = directory.path().join("cloud-dictionary.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["version"], 1);
        assert_eq!(request["kind"], "cloud_dictionary");
        assert_eq!(request["request"]["operation"], "changes");
        let mut stream = stream;
        std::io::Write::write_all(&mut stream, br#"{"changes":[],"next":0}"#).unwrap();
        std::io::Write::write_all(&mut stream, b"\n").unwrap();
    });
    let request = json!({"operation":"changes","after":0,"limit":1});
    let response = UnixSocketProvider::new(socket)
        .cloud_dictionary(request)
        .unwrap();
    assert_eq!(response["next"], 0);
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
fn credential_test_provider_keeps_request_and_response_bounded() {
    let directory = private_tempdir();
    let socket = directory.path().join("online.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["version"], 1);
        assert_eq!(request["kind"], "credential_test");
        assert_eq!(request["query"]["service"], "ai.assistant");
        assert_eq!(request["query"]["config"]["provider"], "deepseek");
        let mut stream = stream;
        std::io::Write::write_all(
            &mut stream,
            br#"{"ok":true,"message":"configuration accepted"}"#,
        )
        .unwrap();
        std::io::Write::write_all(&mut stream, b"\n").unwrap();
    });
    let response = UnixSocketProvider::new(socket)
        .test_credential("ai.assistant", &json!({"provider":"deepseek"}))
        .unwrap();
    assert!(response.ok);
    assert_eq!(response.message, "configuration accepted");
    server.join().unwrap();

    assert!(
        UnixSocketProvider::new(directory.path().join("missing.sock"))
            .test_credential("unknown", &json!({}))
            .is_none()
    );
    assert!(
        UnixSocketProvider::new(directory.path().join("missing.sock"))
            .test_credential("voice.asr", &json!({"value":"x".repeat(16_384)}))
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn online_provider_forwards_the_ai_cache_probe_flag() {
    let mut query: OnlineQuery = serde_json::from_value(json!({
        "scheme": 0, "generation": 3, "identity": "identity", "query_text": "nihao",
        "cache_key": "cache", "pinyin_segments": ["ni", "hao"], "cloud_eligible": true,
        "ai_eligible": true, "cloud_candidates": false, "session_id": 5,
        "ai_assistant": {"enabled": true, "provider": "synthetic", "model": "synthetic-model",
                         "endpoint": "https://ai.invalid/v1/chat/completions"},
    }))
    .unwrap();
    // Absent in every document a host already produces, so only the Linux probe carries it.
    assert!(!query.ai_cache_only);
    assert!(serde_json::to_value(&query)
        .unwrap()
        .get("ai_cache_only")
        .is_none());
    query.ai_cache_only = true;

    let directory = private_tempdir();
    let socket = directory.path().join("online.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["kind"], "online");
        assert_eq!(request["query"]["ai_cache_only"], true);
        let mut stream = stream;
        std::io::Write::write_all(
            &mut stream,
            "{\"candidates\":[{\"text\":\"你好\",\"source\":1}]}\n".as_bytes(),
        )
        .unwrap();
    });
    assert_eq!(
        UnixSocketProvider::new(socket).query_candidates(query),
        Some(vec![("你好".to_owned(), 1)])
    );
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
fn translation_provider_rejects_controls_at_the_socket_boundary() {
    let directory = private_tempdir();
    let request_socket = directory.path().join("translation-request.sock");
    let listener = UnixListener::bind(&request_socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_done = done.clone();
    let server_accepted = accepted.clone();
    let server = std::thread::spawn(move || loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                server_accepted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                std::io::Write::write_all(
                    &mut stream,
                    br#"{"translations":[{"text":"safe","translation":"safe"}]}"#,
                )
                .unwrap();
                std::io::Write::write_all(&mut stream, b"\n").unwrap();
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if server_done.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("translation fixture failed: {error}"),
        }
    });
    let provider = UnixSocketProvider::new(&request_socket);
    for codepoint in (0..=0x1f).chain(0x7f..=0x9f) {
        let control = char::from_u32(codepoint).unwrap();
        assert!(provider
            .translate(TranslationQuery {
                generation: 1,
                target_language: "en".into(),
                candidates: vec![format!("safe{control}")],
                sentence: false,
                provider: None,
                translation_account: false,
                custom_translation: None,
                niutrans: None,
            })
            .is_none());
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    server.join().unwrap();
    assert_eq!(accepted.load(std::sync::atomic::Ordering::Relaxed), 0);

    let response_socket = directory.path().join("translation-response.sock");
    let listener = UnixListener::bind(&response_socket).unwrap();
    let server = std::thread::spawn(move || {
        let reply = |mut stream: std::os::unix::net::UnixStream, response: &str| {
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            std::io::BufRead::read_line(&mut reader, &mut request).unwrap();
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
            std::io::Write::write_all(&mut stream, b"\n").unwrap();
        };
        for codepoint in (0..=0x1f).chain(0x7f..=0x9f) {
            let (stream, _) = listener.accept().unwrap();
            let control = char::from_u32(codepoint).unwrap();
            let response = json!({
                "translations": [{"text":"safe","translation":format!("before{control}after")}]
            })
            .to_string();
            reply(stream, &response);
        }
        let (stream, _) = listener.accept().unwrap();
        reply(
            stream,
            r#"{"translations":[{"text":"safe","translation":"translated"}]}"#,
        );
    });
    let provider = UnixSocketProvider::new(response_socket);
    let query = TranslationQuery {
        generation: 1,
        target_language: "en".into(),
        candidates: vec!["safe".into()],
        sentence: false,
        provider: None,
        translation_account: false,
        custom_translation: None,
        niutrans: None,
    };
    for _ in (0..=0x1f).chain(0x7f..=0x9f) {
        assert!(provider.translate(query.clone()).is_none());
    }
    assert_eq!(
        provider.translate(query).unwrap(),
        vec![TranslationResult {
            text: "safe".into(),
            translation: "translated".into(),
        }]
    );
    server.join().unwrap();
}

#[test]
fn translation_query_carries_the_selected_service() {
    for (service, name) in [
        (TranslationService::Off, "none"),
        (TranslationService::Account, "account"),
        (TranslationService::Tencent, "tencent"),
        (TranslationService::NiuTrans, "niutrans"),
        (TranslationService::Custom, "custom"),
    ] {
        let document = json!({"generation": 1, "candidates": ["中"], "provider": name});
        let query: TranslationQuery = serde_json::from_value(document).unwrap();
        assert_eq!(query.provider, Some(service));
        assert_eq!(serde_json::to_value(&query).unwrap()["provider"], name);
    }
    // A document from a host that predates the field stays without one, so the provider keeps its legacy choice instead of being told Tencent.
    let legacy: TranslationQuery =
        serde_json::from_value(json!({"generation": 1, "candidates": ["中"]})).unwrap();
    assert_eq!(legacy.provider, None);
    assert!(serde_json::to_value(&legacy)
        .unwrap()
        .get("provider")
        .is_none());
    assert!(serde_json::from_value::<TranslationQuery>(
        json!({"generation": 1, "candidates": ["中"], "provider": "deepl"})
    )
    .is_err());
    let sentence: TranslationQuery = serde_json::from_value(json!({
        "generation": 1,
        "candidates": ["这是一个手动触发的整句翻译请求"],
        "sentence": true
    }))
    .unwrap();
    assert!(sentence.sentence);
    assert_eq!(serde_json::to_value(sentence).unwrap()["sentence"], true);
    let account: TranslationQuery = serde_json::from_value(json!({
        "generation": 1,
        "candidates": ["中"],
        "provider": "none",
        "translation_account": true
    }))
    .unwrap();
    assert!(account.translation_account);
    assert_eq!(
        serde_json::to_value(account).unwrap()["translation_account"],
        true
    );
}

#[cfg(unix)]
#[test]
fn sentence_translation_is_single_item_and_bounded() {
    let provider = UnixSocketProvider::new("/this/provider-does-not-exist");
    let too_long = TranslationQuery {
        generation: 1,
        target_language: "en".into(),
        candidates: vec!["中".repeat(513)],
        sentence: true,
        provider: None,
        translation_account: false,
        custom_translation: None,
        niutrans: None,
    };
    assert!(provider.translate(too_long).is_none());
    let two_items = TranslationQuery {
        generation: 1,
        target_language: "en".into(),
        candidates: vec!["第一句".into(), "第二句".into()],
        sentence: true,
        provider: None,
        translation_account: false,
        custom_translation: None,
        niutrans: None,
    };
    assert!(provider.translate(two_items).is_none());
}

#[cfg(unix)]
#[test]
fn translation_switched_off_never_reaches_the_provider() {
    let directory = private_tempdir();
    let socket = directory.path().join("translation-off.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let query = TranslationQuery {
        generation: 1,
        target_language: "en".into(),
        candidates: vec!["中".into()],
        sentence: false,
        provider: Some(TranslationService::Off),
        translation_account: false,
        custom_translation: None,
        niutrans: None,
    };
    assert_eq!(
        UnixSocketProvider::new(&socket).translate(query),
        Some(Vec::new())
    );
    assert!(
        listener.accept().is_err(),
        "a switched-off query connected to the provider"
    );
}

#[cfg(unix)]
#[test]
fn voice_provider_rejects_events_without_generation_binding() {
    let directory = private_tempdir();
    let socket = directory.path().join("voice.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(stream.try_clone().unwrap()),
            &mut request,
        )
        .unwrap();
        std::io::Write::write_all(&mut stream, br#"{"text":"stale","type":"final"}"#).unwrap();
        std::io::Write::write_all(&mut stream, b"\n").unwrap();
    });
    let provider = UnixSocketProvider::new(socket);
    assert!(provider
        .voice_stream_with_options_feedback(
            "zh-cn",
            7,
            &Value::Null,
            None,
            &mut |_, _| {},
            None,
            None,
        )
        .is_none());
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
fn voice_provider_names_only_known_missing_dependencies() {
    for (reply, expected) in [
        (
            r#"{"generation":7,"type":"final","text":"","ok":false,"error":"voice_dependency_missing","detail":"websockets"}"#,
            Err(Some("websockets")),
        ),
        (
            r#"{"generation":7,"type":"final","text":"","ok":false,"error":"voice_dependency_missing","detail":"recorder"}"#,
            Err(Some("recorder")),
        ),
        (
            r#"{"generation":7,"type":"final","text":"","ok":false,"error":"voice_dependency_missing","detail":"local_asr"}"#,
            Err(Some("local_asr")),
        ),
        (
            r#"{"generation":7,"type":"final","text":"","ok":false,"error":"voice_dependency_missing","detail":"token=secret"}"#,
            Err(None),
        ),
        (
            r#"{"generation":7,"type":"final","text":"","ok":false}"#,
            Err(None),
        ),
        (
            r#"{"generation":7,"type":"final","text":"水杉","ok":true}"#,
            Ok("水杉".to_owned()),
        ),
    ] {
        let directory = private_tempdir();
        let socket = directory.path().join("voice.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            std::io::BufRead::read_line(
                &mut std::io::BufReader::new(stream.try_clone().unwrap()),
                &mut request,
            )
            .unwrap();
            std::io::Write::write_all(&mut stream, reply.as_bytes()).unwrap();
            std::io::Write::write_all(&mut stream, b"\n").unwrap();
        });
        let result = UnixSocketProvider::new(socket).voice_stream_with_options_diagnosed(
            "zh-cn",
            7,
            &Value::Null,
            None,
            &mut |_, _| {},
            None,
            None,
        );
        assert_eq!(result, expected, "{reply}");
        server.join().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn dangling_segment_delimiter_cleanup_matches_windows_policy() {
    assert!(needs_dangling_segment_delimiter_backspace("ni'", 3));
    assert!(needs_dangling_segment_delimiter_backspace("ni''ma", 3));
    assert!(!needs_dangling_segment_delimiter_backspace("ni", 2));
    assert!(!needs_dangling_segment_delimiter_backspace("'ma", 0));
    assert!(!needs_dangling_segment_delimiter_backspace("ni'", 2));
    assert!(!needs_dangling_segment_delimiter_backspace("ni'", 9));
}

#[test]
fn voice_control_rejects_zero_generation_without_connecting() {
    let directory = private_tempdir();
    let socket = directory.path().join("voice-control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let provider = UnixSocketProvider::new(&socket);
    assert!(!provider.voice_cancel(0));
    assert!(!provider.voice_stop(0));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}
impl InputEngine for Fixture {
    fn reset_cache(&mut self) -> Result<(), RuntimeError> {
        self.cache_resets += 1;
        Ok(())
    }
    fn balance_paired_punctuation_after_auto_close(
        &mut self,
        opening: u8,
    ) -> Result<(), RuntimeError> {
        self.balanced_openings.push(opening);
        Ok(())
    }
    /// Model what the engine does: a real change to the mode resets the composition.
    ///
    /// `InputSession::set_dedicated_english_mode` calls `reset_composition()` when the flag
    /// actually flips, which is the source's `SetEnglishInputMode` followed by `ClearState`. The
    /// default here was a no-op, so nothing on this side held the rule and a change in the engine
    /// -- pinned 467 commits ahead of the reference -- would have gone unnoticed.
    fn set_dedicated_english(&mut self, enabled: bool) -> Result<(), RuntimeError> {
        if self.dedicated_english == enabled {
            return Ok(());
        }
        self.dedicated_english = enabled;
        self.text.clear();
        Ok(())
    }
    fn expand_initial_candidates(&mut self) -> Result<bool, RuntimeError> {
        if self.withheld.is_empty() {
            return Ok(false);
        }
        self.words.append(&mut self.withheld);
        Ok(true)
    }
    fn set_nine_key_enabled(&mut self, enabled: bool) -> Result<(), RuntimeError> {
        self.nine_key = enabled;
        self.nine_key_spellings.clear();
        Ok(())
    }
    fn choose_nine_key_spelling(&mut self, index: usize) -> Result<EngineResult, RuntimeError> {
        if !self.nine_key || index >= self.nine_key_spellings.len() {
            return Ok(empty_result(false));
        }
        self.text = self.nine_key_spellings[index].clone();
        self.nine_key_spellings = vec![self.text.clone()];
        Ok(empty_result(true))
    }
    fn punctuation(&mut self, value: u8) -> Result<EngineResult, RuntimeError> {
        if value == b'!' {
            return Err(RuntimeError::Engine("injected punctuation failure".into()));
        }
        if value != b',' {
            return Ok(empty_result(false));
        }
        Ok(EngineResult {
            handled: true,
            has_commit: true,
            commit: "，".into(),
            diagnostic: String::new(),
        })
    }
    fn finish(&mut self, index: usize) -> Result<EngineResult, RuntimeError> {
        if self.text.is_empty() {
            return Ok(empty_result(false));
        }
        let mut result = self.select(index)?;
        result.commit.push_str("-remaining-segments");
        Ok(result)
    }
    fn snapshot(&self) -> Result<EngineSnapshot, RuntimeError> {
        if self.snapshot_fails {
            return Err(RuntimeError::Engine("injected snapshot failure".into()));
        }
        Ok(EngineSnapshot {
            scheme: self.scheme,
            nine_key: self.nine_key,
            nine_key_spellings: self.nine_key_spellings.clone(),
            candidate_codes: self.codes.clone(),
            candidate_annotations: self
                .words
                .iter()
                .enumerate()
                .map(|(index, _)| format!("({index})"))
                .collect(),
            candidate_sources: if self.sources.len() == self.words.len() {
                self.sources.clone()
            } else {
                vec![0; self.words.len()]
            },
            candidate_positions: if self.positions.len() == self.words.len() {
                self.positions.clone()
            } else {
                vec![0; self.words.len()]
            },
            candidate_corrected: vec![false; self.words.len()],
            candidate_answers_key: vec![true; self.words.len()],
            microsoft_shuangpin: false,
            shuangpin_profile: "xiaohe".into(),
            answered_by_pinyin_fallback: false,
            wubi_unique_four_code: self.scheme == 2
                && self.text.len() == 4
                && self.words.len() == 1,
            local_mode: self.local_mode.clone(),
            dedicated_english: self.dedicated_english,
            preedit: self.text.clone(),
            reading: self.reading.clone(),
            editing_text: self.text.clone(),
            caret_position: self.text.len(),
            segment_raw_boundaries: vec![],
            candidates: if self.text.is_empty() {
                vec![]
            } else {
                self.words.clone()
            },
        })
    }
    fn character(&mut self, value: u8, _shift: bool) -> Result<EngineResult, RuntimeError> {
        if self.nine_key && (b'2'..=b'9').contains(&value) {
            self.text.push(value as char);
            self.nine_key_spellings = vec!["ni".into(), "mi".into()];
            return Ok(empty_result(true));
        }
        if value.is_ascii_digit() || value.is_ascii_punctuation() {
            return Ok(empty_result(false));
        }
        self.text.push(value as char);
        Ok(empty_result(true))
    }
    fn command(&mut self, _command: Command) -> Result<EngineResult, RuntimeError> {
        self.text.clear();
        self.nine_key_spellings.clear();
        Ok(empty_result(true))
    }
    fn select(&mut self, index: usize) -> Result<EngineResult, RuntimeError> {
        if let Some(remaining) = self.remaining_after_select.take() {
            self.text = remaining;
            self.local_mode = "none".into();
            return Ok(EngineResult {
                handled: true,
                has_commit: true,
                commit: self.words[index].clone(),
                diagnostic: String::new(),
            });
        }
        self.text.clear();
        self.nine_key_spellings.clear();
        self.local_mode = "none".into();
        Ok(EngineResult {
            handled: true,
            has_commit: true,
            commit: self.words[index].clone(),
            diagnostic: String::new(),
        })
    }
    fn select_edge(
        &mut self,
        index: usize,
        edge: CandidateEdge,
    ) -> Result<EngineResult, RuntimeError> {
        let mut result = self.select(index)?;
        result.commit.push_str(match edge {
            CandidateEdge::FirstHan => "-first",
            CandidateEdge::LastHan => "-last",
        });
        Ok(result)
    }
}

#[test]
fn unique_complete_wubi_code_auto_commits_unless_a_phrase_is_being_built() {
    let create = |fixture| {
        let mut runtime = Runtime::new(fixture, 5).unwrap();
        runtime.focus(true).unwrap();
        runtime
    };
    let mut unique = create(Fixture {
        scheme: 2,
        words: vec!["合成候选".into()],
        ..Fixture::default()
    });
    let mut last = None;
    for value in b"wqaa" {
        last = Some(
            unique
                .dispatch(Action::Character {
                    value: *value,
                    shift: false,
                })
                .unwrap(),
        );
    }
    let last = last.unwrap();
    assert_eq!(last.commit.as_deref(), Some("合成候选"));
    assert!(last.view.editing_text.is_empty());

    // The next physical key belongs to a new composition. Windows carries the
    // committed prefix through its TSF continuation payload, then replays this
    // key into the fresh composition instead of dropping it with the automatic
    // four-code commit.
    let fifth = unique
        .dispatch(Action::Character {
            value: b'b',
            shift: false,
        })
        .unwrap();
    assert!(fifth.commit.is_none());
    assert_eq!(fifth.view.editing_text, "b");

    let mut ambiguous = create(Fixture {
        scheme: 2,
        words: vec!["合成甲".into(), "合成乙".into()],
        ..Fixture::default()
    });
    for value in b"wqab" {
        ambiguous
            .dispatch(Action::Character {
                value: *value,
                shift: false,
            })
            .unwrap();
    }
    assert_eq!(ambiguous.view().editing_text, "wqab");

    let mut phrase = create(Fixture {
        scheme: 2,
        words: vec!["合成候选".into()],
        ..Fixture::default()
    });
    phrase.phrase_prefix = "合成前缀".into();
    for value in b"wqaa" {
        phrase
            .dispatch(Action::Character {
                value: *value,
                shift: false,
            })
            .unwrap();
    }
    assert_eq!(phrase.view().editing_text, "wqaa");
    assert_eq!(phrase.view().phrase_prefix, "合成前缀");
}
#[test]
fn a_letter_after_a_complete_wubi_code_commits_the_first_candidate_and_starts_the_next() {
    let create = |fixture| {
        let mut runtime = Runtime::new(fixture, 5).unwrap();
        runtime.focus(true).unwrap();
        runtime
    };
    let type_all = |runtime: &mut Runtime<Fixture>, keys: &[u8]| {
        let mut last = None;
        for value in keys {
            last = Some(
                runtime
                    .dispatch(Action::Character {
                        value: *value,
                        shift: false,
                    })
                    .unwrap(),
            );
        }
        last.unwrap()
    };

    // An ambiguous four-letter code stays open on its fourth key, and the fifth letter commits
    // the first candidate and becomes the next composition instead of being dropped.
    let mut ambiguous = create(Fixture {
        scheme: 2,
        words: vec!["合成甲".into(), "合成乙".into()],
        local_mode: "none".into(),
        ..Fixture::default()
    });
    let fourth = type_all(&mut ambiguous, b"wqab");
    assert!(fourth.commit.is_none());
    assert_eq!(fourth.view.editing_text, "wqab");
    let fifth = type_all(&mut ambiguous, b"x");
    assert_eq!(fifth.commit.as_deref(), Some("合成甲"));
    assert_eq!(
        fifth.commit_context.as_ref().map(|context| context.scheme),
        Some(2)
    );
    assert_eq!(fifth.view.editing_text, "x");

    // Shorter codes, other schemes, local modes, dedicated English and a held phrase keep the
    // letter in the composition.
    let mut short = create(Fixture {
        scheme: 2,
        words: vec!["合成甲".into(), "合成乙".into()],
        local_mode: "none".into(),
        ..Fixture::default()
    });
    let shorter = type_all(&mut short, b"wqa");
    assert_eq!(shorter.view.editing_text, "wqa");
    for (case, fixture) in [
        Fixture {
            scheme: 0,
            words: vec!["候选".into(), "后续".into()],
            local_mode: "none".into(),
            ..Fixture::default()
        },
        Fixture {
            scheme: 2,
            local_mode: "unicode".into(),
            words: vec!["合成甲".into(), "合成乙".into()],
            ..Fixture::default()
        },
        Fixture {
            scheme: 2,
            dedicated_english: true,
            words: vec!["wqab".into(), "wqabx".into()],
            local_mode: "none".into(),
            ..Fixture::default()
        },
    ]
    .into_iter()
    .enumerate()
    {
        let mut other = create(fixture);
        let last = type_all(&mut other, b"wqabx");
        assert!(last.commit.is_none(), "case {case}");
        assert_eq!(last.view.editing_text, "wqabx", "case {case}");
    }
    let mut phrase = create(Fixture {
        scheme: 2,
        words: vec!["合成甲".into(), "合成乙".into()],
        local_mode: "none".into(),
        ..Fixture::default()
    });
    phrase.phrase_prefix = "合成前缀".into();
    let held = type_all(&mut phrase, b"wqabx");
    assert!(held.commit.is_none());
    assert_eq!(held.view.editing_text, "wqabx");

    // A four-letter code with nothing to commit is not a complete code.
    let mut unanswered = create(Fixture {
        scheme: 2,
        words: Vec::new(),
        local_mode: "none".into(),
        ..Fixture::default()
    });
    let empty = type_all(&mut unanswered, b"wqabx");
    assert!(empty.commit.is_none());
    assert_eq!(empty.view.editing_text, "wqabx");
}
fn runtime() -> Runtime<Fixture> {
    Runtime::new(
        Fixture {
            scheme: 0,
            dedicated_english: false,
            nine_key: false,
            nine_key_spellings: Vec::new(),
            local_mode: "none".into(),
            words: (0..12).map(|n| format!("candidate-{n}")).collect(),
            codes: Vec::new(),
            text: String::new(),
            snapshot_fails: false,
            balanced_openings: Vec::new(),
            cache_resets: 0,
            withheld: Vec::new(),
            sources: Vec::new(),
            remaining_after_select: None,
            positions: Vec::new(),
            reading: String::new(),
        },
        5,
    )
    .unwrap()
}

#[test]
fn auto_close_balance_accepts_only_the_book_title_opening() {
    let mut runtime = runtime();
    for invalid in [b'(', b'>', b'a', b' ', 0, 128, 255] {
        assert!(matches!(
            runtime.balance_paired_punctuation_after_auto_close(invalid),
            Err(RuntimeError::InvalidPunctuation)
        ));
    }
    assert!(runtime.engine.balanced_openings.is_empty());
    runtime
        .balance_paired_punctuation_after_auto_close(b'<')
        .unwrap();
    assert_eq!(runtime.engine.balanced_openings, vec![b'<']);
}

// The AI context accumulator. Every host but Linux sent an empty context,
// so AI suggestions had to guess from the pinyin alone.
// The seating table in candidate_selection_policy.h places one candidate per provider, because the
// reference has one of each. A provider here answers with several - the AI limit reaches ten - and
// the first attempt at this treated everything past the first as a local candidate. That is not a
// cosmetic mistake: a local candidate is what takes the first seat, so the second AI suggestion was
// promoted over the first and landed on the space bar.
#[test]
fn several_candidates_from_one_provider_take_their_seat_as_a_group() {
    let seated = |words: &[&str], sources: Vec<u8>| {
        let mut runtime = Runtime::new(
            Fixture {
                scheme: 0,
                dedicated_english: false,
                nine_key: false,
                nine_key_spellings: Vec::new(),
                local_mode: "none".into(),
                words: words.iter().map(|word| (*word).into()).collect(),
                // The seating only runs on a snapshot whose parallel arrays all match, so the
                // codes have to be as long as the words for this to exercise anything.
                codes: (0..words.len()).map(|n| format!("code-{n}")).collect(),
                text: String::new(),
                snapshot_fails: false,
                balanced_openings: Vec::new(),
                cache_resets: 0,
                withheld: Vec::new(),
                sources,
                remaining_after_select: None,
                positions: Vec::new(),
                reading: String::new(),
            },
            9,
        )
        .unwrap();
        runtime.focus(true).unwrap();
        type_key(&mut runtime)
            .view
            .candidates
            .into_iter()
            .map(|candidate| (candidate.text, candidate.annotation))
            .collect::<Vec<_>>()
    };

    // Chinese first, then the whole AI group in the order it arrived. The annotation travels with
    // its candidate, so it also says the parallel arrays were rotated together rather than the text
    // alone: 本地 arrived third and keeps "(2)".
    assert_eq!(
        seated(&["AI 一", "AI 二", "本地"], vec![3, 3, 0]),
        vec![
            ("本地".to_string(), "(2)".to_string()),
            ("AI 一".to_string(), "(0)".to_string()),
            ("AI 二".to_string(), "(1)".to_string()),
        ]
    );
    // Same for a cloud reply of more than one, and the AI group still follows the cloud group.
    assert_eq!(
        seated(&["云一", "云二", "AI", "本地"], vec![2, 2, 3, 0])
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<_>>(),
        vec!["本地", "云一", "云二", "AI"]
    );
    // With a cloud candidate present English sits after AI, and a second English candidate waits
    // behind the seated ones rather than displacing anything.
    assert_eq!(
        seated(
            &["AI 一", "AI 二", "英一", "英二", "云", "本地"],
            vec![3, 3, 4, 4, 2, 0]
        )
        .into_iter()
        .map(|(text, _)| text)
        .collect::<Vec<_>>(),
        vec!["本地", "云", "AI 一", "AI 二", "英一", "英二"]
    );
}

// A single complete kana in Japanese romaji offers its hiragana and katakana as the first two local candidates, and the reference keeps that pair ahead of the cloud word (`JapaneseSingleKanaPairStaysAheadOfCloudCandidate`, which expects か, カ, then the cloud candidate). Anything else keeps the one-seat local prefix.
#[test]
fn japanese_single_kana_pair_stays_ahead_of_the_cloud_candidate() {
    let seated = |scheme: u8, reading: &str, words: &[&str], sources: Vec<u8>| {
        let mut runtime = Runtime::new(
            Fixture {
                scheme,
                local_mode: "none".into(),
                words: words.iter().map(|word| (*word).into()).collect(),
                codes: (0..words.len()).map(|n| format!("code-{n}")).collect(),
                sources,
                reading: reading.into(),
                ..Fixture::default()
            },
            9,
        )
        .unwrap();
        runtime.focus(true).unwrap();
        type_key(&mut runtime)
            .view
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>()
    };
    let words = ["か", "カ", "蚊", "科"];

    assert_eq!(
        seated(3, "か", &words, vec![0, 0, 2, 0]),
        vec!["か", "カ", "蚊", "科"]
    );
    // Two kana are not a single-kana conversion, so the cloud word takes the second seat.
    assert_eq!(
        seated(3, "かき", &words, vec![0, 0, 2, 0]),
        vec!["か", "蚊", "カ", "科"]
    );
    // Pending romaji is an incomplete conversion.
    assert_eq!(
        seated(3, "k", &words, vec![0, 0, 2, 0]),
        vec!["か", "蚊", "カ", "科"]
    );
    // The rule belongs to the Japanese scheme only.
    assert_eq!(
        seated(0, "か", &words, vec![0, 0, 2, 0]),
        vec!["か", "蚊", "カ", "科"]
    );
    // With a single local candidate the two-seat prefix takes what there is.
    assert_eq!(seated(3, "か", &["か", "蚊"], vec![0, 2]), vec!["か", "蚊"]);
    assert_eq!(seated(3, "か", &["蚊", "か"], vec![2, 0]), vec!["か", "蚊"]);
}

// A pinned or promoted English word keeps the first seat when online candidates arrive. The Engine seats it first because its learned weight is the unique maximum of the mixed list, and the reference's `PromotedEnglishCandidateCanBecomeTheFirstMixedCandidate` expects exactly this order, so Space commits the English word rather than the Chinese one.
#[test]
fn promoted_english_candidate_keeps_the_first_seat_with_cloud_and_ai() {
    let seated = |words: &[&str], sources: Vec<u8>| {
        let mut runtime = Runtime::new(
            Fixture {
                scheme: 0,
                dedicated_english: false,
                nine_key: false,
                nine_key_spellings: Vec::new(),
                local_mode: "none".into(),
                words: words.iter().map(|word| (*word).into()).collect(),
                codes: (0..words.len()).map(|n| format!("code-{n}")).collect(),
                text: String::new(),
                snapshot_fails: false,
                balanced_openings: Vec::new(),
                cache_resets: 0,
                withheld: Vec::new(),
                sources,
                remaining_after_select: None,
                positions: Vec::new(),
                reading: String::new(),
            },
            9,
        )
        .unwrap();
        runtime.focus(true).unwrap();
        type_key(&mut runtime)
            .view
            .candidates
            .into_iter()
            .map(|candidate| candidate.text)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        seated(
            &["GitHub", "个", "给", "云候选", "AI联想"],
            vec![4, 0, 0, 2, 3]
        ),
        vec!["GitHub", "个", "云候选", "AI联想", "给"]
    );
    // Only the Engine's first seat signals a promotion. An English candidate it placed after the leading Chinese one is seated behind cloud and AI as usual.
    assert_eq!(
        seated(
            &["个", "GitHub", "给", "云候选", "AI联想"],
            vec![0, 4, 0, 2, 3]
        ),
        vec!["个", "云候选", "AI联想", "GitHub", "给"]
    );
    // A promoted English word does not pull a second English candidate into the leading English seat; the rest wait behind the Chinese candidates.
    assert_eq!(
        seated(
            &["GitHub", "个", "Gitter", "给", "云候选"],
            vec![4, 0, 4, 0, 2]
        ),
        vec!["GitHub", "个", "云候选", "给", "Gitter"]
    );
}

// An English word the user fixed to a seat stays there when a cloud or AI reply arrives, as the reference's `FixedEnglishCandidateKeepsItsMixedCandidatePosition` expects. Without the fixed-English pass the seating would put it behind the online candidates.
#[test]
fn fixed_english_candidate_keeps_its_seat_when_online_candidates_arrive() {
    let runtime = |words: &[&str], sources: Vec<u8>, positions: Vec<u8>| {
        let mut runtime = Runtime::new(
            Fixture {
                local_mode: "none".into(),
                words: words.iter().map(|word| (*word).into()).collect(),
                codes: (0..words.len()).map(|n| format!("code-{n}")).collect(),
                sources,
                positions,
                reading: String::new(),
                ..Fixture::default()
            },
            9,
        )
        .unwrap();
        runtime.focus(true).unwrap();
        let page = type_key(&mut runtime).view.candidates;
        (runtime, page)
    };
    let texts = |page: &[Candidate]| {
        page.iter()
            .map(|candidate| candidate.text.clone())
            .collect::<Vec<_>>()
    };

    // Fixed to the first seat, with a cloud candidate.
    let (mut cloud_only, cloud_page) = runtime(
        &["个", "GitHub", "给", "云候选"],
        vec![0, 4, 0, 2],
        vec![0, 1, 0, 0],
    );
    assert_eq!(texts(&cloud_page), vec!["GitHub", "个", "云候选", "给"]);

    // Still first once an AI candidate joins the cloud one.
    let (_, page) = runtime(
        &["个", "GitHub", "给", "云候选", "AI联想"],
        vec![0, 4, 0, 2, 3],
        vec![0, 1, 0, 0, 0],
    );
    assert_eq!(texts(&page), vec!["GitHub", "个", "云候选", "AI联想", "给"]);

    // Fixed to the third seat among local, AI and cloud candidates.
    let (_, page) = runtime(
        &["个", "GitHub", "AI联想", "云候选", "给"],
        vec![0, 4, 3, 2, 0],
        vec![0, 3, 0, 0, 0],
    );
    assert_eq!(texts(&page)[2], "GitHub");
    assert_eq!(texts(&page), vec!["个", "云候选", "GitHub", "AI联想", "给"]);

    // Picking the first seat commits the re-seated English word, not the Engine's first candidate.
    let done = cloud_only
        .dispatch(Action::Select(cloud_page[0].id))
        .unwrap();
    assert_eq!(done.commit.as_deref(), Some("GitHub"));
}

// Half a phrase belongs in the composition, not in the document. Picking a candidate that covers
// only part of the input leaves the Engine composing the rest and hands back the piece that was
// picked; sending that piece straight out puts half a phrase into the application - a search box
// searches for it, an editor records an undo step for it - while the user is still typing.
#[test]
fn a_chosen_phrase_piece_waits_for_the_rest_of_the_phrase() {
    let start = |remaining: Option<&str>| {
        let mut runtime = Runtime::new(
            Fixture {
                scheme: 0,
                dedicated_english: false,
                nine_key: false,
                nine_key_spellings: Vec::new(),
                local_mode: "none".into(),
                words: vec!["海滩".into(), "跑步".into()],
                codes: Vec::new(),
                text: String::new(),
                snapshot_fails: false,
                balanced_openings: Vec::new(),
                cache_resets: 0,
                withheld: Vec::new(),
                sources: Vec::new(),
                remaining_after_select: remaining.map(str::to_owned),
                positions: Vec::new(),
                reading: String::new(),
            },
            5,
        )
        .unwrap();
        runtime.focus(true).unwrap();
        runtime
    };
    let pick = |runtime: &mut Runtime<Fixture>| {
        let id = runtime.view().candidates[0].id;
        runtime.dispatch(Action::Select(id)).unwrap()
    };

    // Off, which is what a host that cannot draw the piece gets: unchanged behaviour.
    let mut runtime = start(Some("paobu"));
    type_key(&mut runtime);
    let held = pick(&mut runtime);
    assert_eq!(held.commit.as_deref(), Some("海滩"));
    assert!(held.view.phrase_prefix.is_empty());

    // On: the piece is held, shown to the host separately from the editing text, and the whole
    // phrase goes out as one commit when the composition ends.
    let mut runtime = start(Some("paobu"));
    runtime.set_phrase_preedit(true);
    type_key(&mut runtime);
    let held = pick(&mut runtime);
    assert_eq!(held.commit, None);
    assert_eq!(held.view.phrase_prefix, "海滩");
    assert_eq!(held.view.editing_text, "paobu");
    let rest = runtime.view().candidates[1].id;
    let done = runtime.dispatch(Action::Select(rest)).unwrap();
    assert_eq!(done.commit.as_deref(), Some("海滩跑步"));
    assert!(done.view.phrase_prefix.is_empty());
    assert!(done.view.editing_text.is_empty());

    // Escape throws away what was chosen along with what was typed, as the reference's _HandleCancel
    // does - it clears word_for_creating_word in the same breath as terminating the composition.
    let mut runtime = start(Some("paobu"));
    runtime.set_phrase_preedit(true);
    type_key(&mut runtime);
    pick(&mut runtime);
    let cancelled = runtime.dispatch(Action::Command(Command::Cancel)).unwrap();
    assert_eq!(cancelled.commit, None);
    assert!(cancelled.view.phrase_prefix.is_empty());

    // Leaving the client cancels the composition too, but there the piece goes to the document:
    // before it was ever held back it would already be there, and a click into another window is
    // not the user throwing the phrase away.
    let mut runtime = start(Some("paobu"));
    runtime.set_phrase_preedit(true);
    type_key(&mut runtime);
    pick(&mut runtime);
    let blurred = runtime.focus(false).unwrap();
    assert_eq!(blurred.commit.as_deref(), Some("海滩"));
    assert!(blurred.view.phrase_prefix.is_empty());

    // A commit that no candidate was picked for is not part of a phrase. Punctuation finishes the
    // composition and sends the mark out with it; that commit has to read the same either way.
    let mut plain = start(None);
    type_key(&mut plain);
    let expected = plain.dispatch(Action::Punctuation(b',')).unwrap().commit;
    assert!(expected.is_some());
    let mut runtime = start(None);
    runtime.set_phrase_preedit(true);
    type_key(&mut runtime);
    let punctuated = runtime.dispatch(Action::Punctuation(b',')).unwrap();
    assert_eq!(punctuated.commit, expected);
    assert!(punctuated.view.phrase_prefix.is_empty());

    // Turning it off with a piece in hand hands the piece back rather than dropping it.
    let mut runtime = start(Some("paobu"));
    runtime.set_phrase_preedit(true);
    type_key(&mut runtime);
    pick(&mut runtime);
    assert_eq!(runtime.set_phrase_preedit(false).as_deref(), Some("海滩"));
    assert!(runtime.view().phrase_prefix.is_empty());
}

// The one place this leaves the reference: there, backspacing the remaining reading away keeps the
// chosen piece on screen with nothing after it. Holding text with no composition under it would
// make every host's "is there a composition" test lie, so the piece is committed instead.
#[test]
fn a_phrase_piece_survives_the_reading_being_deleted() {
    let mut runtime = Runtime::new(
        Fixture {
            scheme: 0,
            dedicated_english: false,
            nine_key: false,
            nine_key_spellings: Vec::new(),
            local_mode: "none".into(),
            words: vec!["海滩".into(), "跑步".into()],
            codes: Vec::new(),
            text: String::new(),
            snapshot_fails: false,
            balanced_openings: Vec::new(),
            cache_resets: 0,
            withheld: Vec::new(),
            sources: Vec::new(),
            remaining_after_select: Some("p".into()),
            positions: Vec::new(),
            reading: String::new(),
        },
        5,
    )
    .unwrap();
    runtime.set_phrase_preedit(true);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    let id = runtime.view().candidates[0].id;
    let held = runtime.dispatch(Action::Select(id)).unwrap();
    assert_eq!(held.view.phrase_prefix, "海滩");

    let emptied = runtime
        .dispatch(Action::Command(Command::Backspace))
        .unwrap();
    assert!(emptied.view.editing_text.is_empty());
    assert_eq!(emptied.commit.as_deref(), Some("海滩"));
    assert!(emptied.view.phrase_prefix.is_empty());
}

#[test]
fn ai_context_keeps_the_recent_tail_on_a_character_boundary() {
    let mut runtime = runtime();
    runtime.focused = true;
    runtime.remember_commit("你好");
    runtime.remember_commit("世界");
    assert_eq!(runtime.ai_context, "你好世界");

    // Bounded at 1024 bytes, because query_candidates refuses anything
    // longer outright rather than trimming it.
    for _ in 0..400 {
        runtime.remember_commit("字");
    }
    assert!(runtime.ai_context.len() <= 1024);
    // The cut lands on a character boundary, so the context is still valid
    // UTF-8 and does not start with half a character.
    assert!(runtime.ai_context.is_char_boundary(0));
    assert!(std::str::from_utf8(runtime.ai_context.as_bytes()).is_ok());
    assert!(runtime.ai_context.ends_with('字'));
    // It is the tail that is kept, not the head.
    assert!(!runtime.ai_context.starts_with("你好"));
}

#[test]
fn ai_context_does_not_leak_between_clients() {
    let mut runtime = runtime();
    runtime.focused = true;
    runtime.remember_commit("上一个应用里的句子");
    assert!(!runtime.ai_context.is_empty());

    // A commit while unfocused is not context at all, and clears what was
    // there: the user has left.
    runtime.focused = false;
    runtime.remember_commit("anything");
    assert!(runtime.ai_context.is_empty());
}
// An engine that models the one thing the phrase rules turn on: a selection takes its reading off
// the front and leaves the rest, and the caret can sit somewhere other than the end.
//
// `Fixture` cannot express either - its `select` replaces the whole reading and its caret is always
// at the end - and a rule about what is left in front of the caret cannot be tested against an
// engine that has no such thing.
struct PhraseEngine {
    reading: String,
    caret: usize,
    /// How much of the reading each selection takes off the front, oldest first. A selection past
    /// the end of this list finishes the composition.
    consumes: Vec<usize>,
    words: Vec<String>,
}

impl PhraseEngine {
    fn new(consumes: Vec<usize>) -> Self {
        Self {
            reading: String::new(),
            caret: 0,
            consumes,
            words: vec!["海滩".into(), "跑步".into()],
        }
    }
}

impl InputEngine for PhraseEngine {
    fn snapshot(&self) -> Result<EngineSnapshot, RuntimeError> {
        Ok(EngineSnapshot {
            scheme: 0,
            nine_key: false,
            nine_key_spellings: Vec::new(),
            candidate_codes: Vec::new(),
            candidate_annotations: vec![String::new(); self.words.len()],
            candidate_sources: vec![0; self.words.len()],
            candidate_positions: vec![0; self.words.len()],
            candidate_corrected: vec![false; self.words.len()],
            candidate_answers_key: vec![true; self.words.len()],
            microsoft_shuangpin: false,
            shuangpin_profile: "xiaohe".into(),
            answered_by_pinyin_fallback: false,
            wubi_unique_four_code: false,
            local_mode: "none".into(),
            dedicated_english: false,
            preedit: self.reading.clone(),
            reading: String::new(),
            editing_text: self.reading.clone(),
            caret_position: self.caret.min(self.reading.len()),
            segment_raw_boundaries: Vec::new(),
            candidates: if self.reading.is_empty() {
                Vec::new()
            } else {
                self.words.clone()
            },
        })
    }
    fn character(&mut self, value: u8, _shift: bool) -> Result<EngineResult, RuntimeError> {
        self.reading.push(value as char);
        self.caret = self.reading.len();
        Ok(empty_result(true))
    }
    fn command(&mut self, command: Command) -> Result<EngineResult, RuntimeError> {
        match command {
            Command::MoveHome => self.caret = 0,
            Command::MoveEnd => self.caret = self.reading.len(),
            Command::Backspace => {
                if self.caret > 0 {
                    self.reading.remove(self.caret - 1);
                    self.caret -= 1;
                }
            }
            _ => {
                // Like the real Engine, a key that ends a composition is not wanted when there is no reading to end.
                let composing = !self.reading.is_empty();
                self.reading.clear();
                self.caret = 0;
                return Ok(empty_result(composing));
            }
        }
        Ok(empty_result(true))
    }
    // The whole reading before the caret is one segment, so a segment Backspace takes all of it.
    fn segment_command(&mut self, command: SegmentCommand) -> Result<EngineResult, RuntimeError> {
        if !matches!(command, SegmentCommand::Backspace) || self.caret == 0 {
            return Ok(empty_result(!self.reading.is_empty()));
        }
        self.reading = self.reading.split_off(self.caret);
        self.caret = 0;
        Ok(empty_result(true))
    }
    fn select(&mut self, index: usize) -> Result<EngineResult, RuntimeError> {
        let commit = self.words[index].clone();
        if self.consumes.is_empty() {
            self.reading.clear();
        } else {
            let consumed = self.consumes.remove(0).min(self.reading.len());
            self.reading = self.reading.split_off(consumed);
        }
        self.caret = self.reading.len();
        Ok(EngineResult {
            handled: true,
            has_commit: true,
            commit,
            diagnostic: String::new(),
        })
    }
    fn finish(&mut self, index: usize) -> Result<EngineResult, RuntimeError> {
        self.select(index)
    }
    fn punctuation(&mut self, _value: u8) -> Result<EngineResult, RuntimeError> {
        Ok(empty_result(false))
    }
    fn select_edge(
        &mut self,
        index: usize,
        _edge: CandidateEdge,
    ) -> Result<EngineResult, RuntimeError> {
        self.select(index)
    }
}

// The reading is typed rather than seeded: taking focus cancels the composition, so an engine that
// started with one would lose it before the first key of the test.
fn phrase_runtime(reading: &str, consumes: Vec<usize>) -> Runtime<PhraseEngine> {
    let mut runtime = Runtime::new(PhraseEngine::new(consumes), 5).unwrap();
    runtime.set_phrase_preedit(true);
    runtime.focus(true).unwrap();
    for byte in reading.bytes() {
        runtime
            .dispatch(Action::Character {
                value: byte,
                shift: false,
            })
            .unwrap();
    }
    runtime
}

// A Ctrl+Backspace that empties the reading does not end the phrase: the chosen piece stays in the composition with its selection, as the reference's `keep_creating_word_after_empty_raw` keeps it (MSIME-Windows server/src/ipc/event_listener.cpp, pinned by `ShouldRetreatCreatingWordSelection(true, false, true, 0, 0, 1)` and `ShouldDropCreatingWordSegment(true, false, true, 0, 1)` in test_input_key_policy.cpp). Every follow-up key then acts on that phrase.
#[test]
fn a_segment_backspace_that_empties_the_reading_keeps_the_phrase() {
    let emptied = || {
        let mut runtime = phrase_runtime("haitanpaobu", vec![6]);
        let id = runtime.view().candidates[0].id;
        let held = runtime.dispatch(Action::Select(id)).unwrap();
        assert_eq!(held.view.phrase_prefix, "海滩");
        assert_eq!(held.view.editing_text, "paobu");
        let kept = runtime.dispatch(Action::SegmentBackspace).unwrap();
        assert_eq!(kept.commit, None);
        assert!(kept.handled);
        assert_eq!(kept.view.phrase_prefix, "海滩");
        assert!(kept.view.editing_text.is_empty());
        runtime
    };

    // Backspace takes the selection back: the reading it consumed returns.
    let mut runtime = emptied();
    let back = runtime
        .dispatch(Action::Command(Command::Backspace))
        .unwrap();
    assert_eq!(back.commit, None);
    assert!(back.view.phrase_prefix.is_empty());
    assert_eq!(back.view.editing_text, "haitan");

    // A second Ctrl+Backspace deletes the chosen piece, which ends the composition with nothing sent.
    let mut runtime = emptied();
    let dropped = runtime.dispatch(Action::SegmentBackspace).unwrap();
    assert_eq!(dropped.commit, None);
    assert!(dropped.handled);
    assert!(dropped.view.phrase_prefix.is_empty());
    assert!(dropped.view.editing_text.is_empty());

    // Enter sends the phrase, and the key does not also reach the application.
    let mut runtime = emptied();
    let committed = runtime
        .dispatch(Action::Command(Command::CommitRaw))
        .unwrap();
    assert_eq!(committed.commit.as_deref(), Some("海滩"));
    assert!(committed.handled);
    assert!(committed.view.phrase_prefix.is_empty());

    // Escape throws it away.
    let mut runtime = emptied();
    let cancelled = runtime.dispatch(Action::Command(Command::Cancel)).unwrap();
    assert_eq!(cancelled.commit, None);
    assert!(cancelled.handled);
    assert!(cancelled.view.phrase_prefix.is_empty());

    // Leaving the client still sends the phrase.
    let mut runtime = emptied();
    let blurred = runtime.focus(false).unwrap();
    assert_eq!(blurred.commit.as_deref(), Some("海滩"));
}

// Going back into a phrase that is half chosen.
//
// The reference has two rules for it, both in `input_key_policy.h`, and this host had neither: the
// piece the user picked could only be finished or thrown away whole. Picking the wrong word for the
// first half of a phrase is ordinary, and the way out of it was to cancel the composition and type
// the whole thing again.
#[test]
fn the_last_selection_of_a_phrase_can_be_taken_back() {
    // Backspace on the last character of the reading: the selection comes back instead of the
    // composition ending. The reading it consumed is what is on screen afterwards, so the user can
    // pick a different word for it.
    let mut runtime = phrase_runtime("haitanp", vec![6]);
    let id = runtime.view().candidates[0].id;
    let held = runtime.dispatch(Action::Select(id)).unwrap();
    assert_eq!(held.view.phrase_prefix, "海滩");
    assert_eq!(held.view.editing_text, "p");

    let back = runtime
        .dispatch(Action::Command(Command::Backspace))
        .unwrap();
    assert_eq!(back.commit, None);
    assert!(back.view.phrase_prefix.is_empty());
    assert_eq!(back.view.editing_text, "haitan");
    assert!(!back.view.candidates.is_empty());

    // And it is a stack: only the newest selection comes back, the ones before it stay.
    let mut runtime = phrase_runtime("haitanpaobux", vec![6, 5]);
    let first = runtime.view().candidates[0].id;
    runtime.dispatch(Action::Select(first)).unwrap();
    let second = runtime.view().candidates[1].id;
    let held = runtime.dispatch(Action::Select(second)).unwrap();
    assert_eq!(held.view.phrase_prefix, "海滩跑步");
    assert_eq!(held.view.editing_text, "x");
    let back = runtime
        .dispatch(Action::Command(Command::Backspace))
        .unwrap();
    assert_eq!(back.view.phrase_prefix, "海滩");
    assert_eq!(back.view.editing_text, "paobu");

    // With more than one character left the key is an ordinary Backspace: the user is editing the
    // reading, not leaving it.
    let mut runtime = phrase_runtime("haitanpa", vec![6]);
    let id = runtime.view().candidates[0].id;
    runtime.dispatch(Action::Select(id)).unwrap();
    let edited = runtime
        .dispatch(Action::Command(Command::Backspace))
        .unwrap();
    assert_eq!(edited.view.phrase_prefix, "海滩");
    assert_eq!(edited.view.editing_text, "p");

    // Nothing was ever selected, so there is nothing to go back to and Backspace stays Backspace.
    let mut runtime = phrase_runtime("p", Vec::new());
    let plain = runtime
        .dispatch(Action::Command(Command::Backspace))
        .unwrap();
    assert!(plain.view.editing_text.is_empty());
    assert!(plain.view.phrase_prefix.is_empty());
}

// Ctrl+Backspace with nothing before the caret deletes the selection itself, and unlike Backspace
// it does not hand the reading back: the user asked to remove that piece of the phrase, not to
// spell it again (the reference's PRD R3).
#[test]
fn a_segment_backspace_with_nothing_before_the_caret_drops_the_selection() {
    let mut runtime = phrase_runtime("haitanpaobu", vec![6]);
    let id = runtime.view().candidates[0].id;
    let held = runtime.dispatch(Action::Select(id)).unwrap();
    assert_eq!(held.view.phrase_prefix, "海滩");
    assert_eq!(held.view.editing_text, "paobu");

    runtime
        .dispatch(Action::Command(Command::MoveHome))
        .unwrap();
    let dropped = runtime.dispatch(Action::SegmentBackspace).unwrap();
    assert_eq!(dropped.commit, None);
    assert!(dropped.view.phrase_prefix.is_empty());
    // The reading it consumed is gone for good; what the user typed after it is untouched.
    assert_eq!(dropped.view.editing_text, "paobu");

    // With the caret anywhere else the key is the ordinary segment Backspace and reaches the
    // Engine, which owns the unit boundaries.
    let mut runtime = phrase_runtime("haitanpaobu", vec![6]);
    let id = runtime.view().candidates[0].id;
    runtime.dispatch(Action::Select(id)).unwrap();
    let edited = runtime.dispatch(Action::SegmentBackspace).unwrap();
    assert_eq!(edited.view.phrase_prefix, "海滩");
}

fn type_key(runtime: &mut Runtime<Fixture>) -> Transition {
    runtime
        .dispatch(Action::Character {
            value: b'a',
            shift: false,
        })
        .unwrap()
}

#[test]
fn candidate_codes_follow_candidates_in_page_and_complete_snapshots() {
    let mut runtime = Runtime::new(
        Fixture {
            scheme: 2,
            dedicated_english: false,
            nine_key: false,
            nine_key_spellings: Vec::new(),
            local_mode: "none".into(),
            words: vec!["甲".into(), "乙".into()],
            codes: vec!["ab".into(), "ac".into()],
            text: String::new(),
            snapshot_fails: false,
            balanced_openings: Vec::new(),
            cache_resets: 0,
            withheld: Vec::new(),
            sources: Vec::new(),
            remaining_after_select: None,
            positions: Vec::new(),
            reading: String::new(),
        },
        2,
    )
    .unwrap();
    runtime.focus(true).unwrap();
    let page = type_key(&mut runtime).view;
    assert_eq!(page.candidates[0].code, "ab");
    assert_eq!(page.candidates[1].code, "ac");
    let snapshot = runtime.all_candidates();
    assert_eq!(snapshot.candidates[0].code, "ab");
    assert_eq!(snapshot.candidates[1].code, "ac");
    assert!(snapshot.reading.is_empty());
    let serialized = serde_json::to_value(snapshot).unwrap();
    assert_eq!(serialized["candidates"][1]["code"], "ac");
    assert_eq!(serialized["reading"], "");
}

#[test]
fn online_provider_worker_is_bounded_and_filters_invalid_results() {
    let query = OnlineQuery {
        scheme: 0,
        generation: 4,
        identity: "identity".into(),
        query_text: "nihao".into(),
        cache_key: "cache".into(),
        pinyin_segments: vec!["ni".into(), "hao".into()],
        cloud_eligible: true,
        ai_eligible: true,
        cloud_candidates: true,
        session_id: 9,
        ai_context: String::new(),
        ai_assistant: None,
        ai_cache_only: false,
    };
    let worker = OnlineProviderWorker::spawn(1, |query| {
        if query.query_text == "nihao" {
            Some(("你好".into(), 0))
        } else {
            Some((String::new(), 7))
        }
    })
    .unwrap();
    assert!(worker.submit(query.clone()));
    let mut result = None;
    for _ in 0..100 {
        result = worker.try_recv();
        if result.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let result = result.expect("provider result");
    assert_eq!(result.query, query);
    assert_eq!(result.text, "你好");
    assert_eq!(result.source, 0);
    worker.shutdown();
}

#[test]
fn online_provider_worker_keeps_only_the_latest_completed_result() {
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = std::sync::Arc::clone(&calls);
    let worker = OnlineProviderWorker::spawn(1, move |query| {
        observed.fetch_add(1, Ordering::SeqCst);
        Some((query.query_text, 0))
    })
    .unwrap();
    let query = |text: &str| OnlineQuery {
        scheme: 0,
        generation: 1,
        identity: "identity".into(),
        query_text: text.into(),
        cache_key: text.into(),
        pinyin_segments: vec![],
        cloud_eligible: true,
        ai_eligible: false,
        cloud_candidates: true,
        session_id: 9,
        ai_context: String::new(),
        ai_assistant: None,
        ai_cache_only: false,
    };
    assert!(worker.submit(query("first")));
    for _ in 0..100 {
        if calls.load(Ordering::SeqCst) >= 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(worker.submit(query("second")));
    for _ in 0..100 {
        if calls.load(Ordering::SeqCst) >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        worker.try_recv().map(|result| result.text),
        Some("second".into())
    );
    assert!(worker.try_recv().is_none());
    worker.shutdown();
}

#[test]
fn cloud_request_requires_eligible_query() {
    assert_eq!(
        WINDOWS_CLOUD_DEBOUNCE,
        std::time::Duration::from_millis(500)
    );
    let mut query = OnlineQuery {
        scheme: 0,
        generation: 1,
        identity: "x".into(),
        query_text: "ni".into(),
        cache_key: "x".into(),
        pinyin_segments: vec![],
        cloud_eligible: false,
        ai_eligible: false,
        cloud_candidates: true,
        session_id: 1,
        ai_context: String::new(),
        ai_assistant: None,
        ai_cache_only: false,
    };
    assert!(cloud_request_url(&query).is_none());
    query.cloud_eligible = true;
    assert!(cloud_request_url(&query)
        .unwrap()
        .contains("inputtools.google.com"));
    let response = serde_json::json!(["SUCCESS", [["ni", ["你"]]]]).to_string();
    let result = cloud_candidate_from_response(query, response.as_bytes()).unwrap();
    assert_eq!(result.text, "你");
    assert_eq!(result.source, 0);
}

#[test]
fn online_provider_worker_rejects_zero_capacity_and_shutdowns_idle() {
    assert!(OnlineProviderWorker::spawn(0, |_| None).is_err());
    let worker = OnlineProviderWorker::spawn(1, |_| None).unwrap();
    worker.shutdown();
}
#[test]
fn replacement_requires_verified_idle_and_preserves_session_focus() {
    let mut active = runtime();
    active.focus(true).unwrap();
    let old = type_key(&mut active).view;
    assert!(matches!(
        active.replace_engine(runtime().engine, 2),
        Err(RuntimeError::CompositionActive)
    ));
    assert_eq!(active.view().editing_text, old.editing_text);
    active.dispatch(Action::Command(Command::Cancel)).unwrap();
    active.replace_engine(runtime().engine, 2).unwrap();
    let updated = type_key(&mut active).view;
    assert_eq!(updated.session, old.session);
    assert!(updated.focused && updated.generation > old.generation);
    assert_eq!(updated.candidates.len(), 2);
    assert!(matches!(
        active.dispatch(Action::Select(old.candidates[0].id)),
        Err(RuntimeError::StaleCandidate)
    ));
    active.engine.snapshot_fails = true;
    assert!(active.refresh().is_err());
    assert!(active.view().editing_text.is_empty());
    assert!(
        !active.is_idle(),
        "missing snapshot is not proof of idle Engine"
    );
    assert!(matches!(
        active.replace_engine(runtime().engine, 2),
        Err(RuntimeError::CompositionActive)
    ));
}

#[test]
fn touch_layout_changes_atomically_with_engine_replacement() {
    let mut active = runtime();
    assert_eq!(
        active.view().touch_keyboard_layout,
        TouchKeyboardLayout::TwentySixKey
    );
    active.focus(true).unwrap();
    type_key(&mut active);
    assert!(matches!(
        active.replace_engine_with_touch_layout(runtime().engine, 2, TouchKeyboardLayout::NineKey),
        Err(RuntimeError::CompositionActive)
    ));
    assert_eq!(
        active.view().touch_keyboard_layout,
        TouchKeyboardLayout::TwentySixKey
    );
    active.dispatch(Action::Command(Command::Cancel)).unwrap();
    active
        .replace_engine_with_touch_layout(runtime().engine, 2, TouchKeyboardLayout::NineKey)
        .unwrap();
    assert_eq!(
        active.view().touch_keyboard_layout,
        TouchKeyboardLayout::NineKey
    );
    active
        .replace_engine_with_touch_layout(runtime().engine, 2, TouchKeyboardLayout::Handwriting)
        .unwrap();
    assert_eq!(
        active.view().touch_keyboard_layout,
        TouchKeyboardLayout::Handwriting
    );
}

#[test]
fn translations_are_generation_scoped_and_exposed_on_candidates() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    let view = type_key(&mut runtime).view;
    assert!(
        !runtime.apply_translations(view.generation - 1, [("candidate-0".into(), "old".into())])
    );
    assert!(runtime.apply_translations(
        view.generation,
        [("candidate-0".into(), "translated".into())]
    ));
    assert_eq!(
        runtime.view().candidates[0].translation.as_deref(),
        Some("translated")
    );
    runtime.dispatch(Action::Command(Command::Cancel)).unwrap();
    assert!(runtime
        .view()
        .candidates
        .iter()
        .all(|candidate| candidate.translation.is_none()));
}

#[test]
fn replacement_snapshot_failure_keeps_the_original_engine() {
    let mut active = runtime();
    active.focus(true).unwrap();
    let generation = active.view().generation;
    let mut replacement = runtime().engine;
    replacement.snapshot_fails = true;
    assert!(active.replace_engine(replacement, 2).is_err());
    assert_eq!(active.view().generation, generation);
    assert_eq!(type_key(&mut active).view.candidates.len(), 5);
}

#[test]
fn paging_and_selection_use_global_engine_indices() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    let page = runtime.dispatch(Action::NextPage).unwrap().view;
    assert_eq!(page.page, 1);
    assert_eq!(page.page_count, 3);
    assert_eq!(page.candidates[0].annotation, "(5)");
    assert_eq!(page.candidates[0].text, "candidate-5");
    let result = runtime
        .dispatch(Action::Select(page.candidates[2].id))
        .unwrap();
    assert_eq!(result.commit.as_deref(), Some("candidate-7"));
    assert!(result.view.candidates.is_empty());
}

#[test]
fn complete_candidate_snapshot_is_on_demand_and_preserves_global_identity() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    let page = type_key(&mut runtime).view;
    assert_eq!(page.candidates.len(), 5);
    assert!(runtime.apply_translations(
        page.generation,
        [("candidate-10".into(), "translated".into())]
    ));

    let snapshot = runtime.all_candidates();
    assert_eq!(snapshot.session, page.session);
    assert_eq!(snapshot.generation, page.generation);
    assert_eq!(snapshot.preedit, "a");
    assert_eq!(snapshot.candidates.len(), 12);
    assert_eq!(snapshot.candidates[10].id.index, 10);
    assert_eq!(snapshot.candidates[10].annotation, "(10)");
    assert_eq!(snapshot.candidates[10].source, 0);
    assert_eq!(snapshot.candidates[10].fixed_position, 0);
    assert_eq!(
        snapshot.candidates[10].translation.as_deref(),
        Some("translated")
    );
    assert!(snapshot.candidates[0].highlighted);
}

#[test]
fn expanded_panel_selection_accepts_only_any_candidate_from_current_generation() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    let page = type_key(&mut runtime).view;
    let outside_page = runtime.all_candidates().candidates[10].id;
    let generation = page.generation;

    assert!(matches!(
        runtime.dispatch(Action::Select(outside_page)),
        Err(RuntimeError::StaleCandidate)
    ));
    for invalid in [
        CandidateId {
            session: outside_page.session + 1,
            ..outside_page
        },
        CandidateId {
            generation: outside_page.generation + 1,
            ..outside_page
        },
        CandidateId {
            index: 12,
            ..outside_page
        },
    ] {
        assert!(matches!(
            runtime.dispatch(Action::SelectAnyCandidate(invalid)),
            Err(RuntimeError::StaleCandidate)
        ));
        assert_eq!(runtime.view().generation, generation);
    }

    let selected = runtime
        .dispatch(Action::SelectAnyCandidate(outside_page))
        .unwrap();
    assert_eq!(selected.commit.as_deref(), Some("candidate-10"));
    assert!(selected.view.candidates.is_empty());
}

#[test]
fn candidate_list_edges_reach_the_ends_of_the_whole_list() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    let highlighted = |view: &View| {
        view.candidates
            .iter()
            .find(|candidate| candidate.highlighted)
            .unwrap()
            .text
            .clone()
    };

    // From the second page, Home goes back to the very first candidate and takes the page with it -
    // the reference answers its Home with SetSelection(0), which readjusts the page. Stopping at the
    // top of the page the user is already looking at is a keystroke that changes almost nothing.
    runtime.dispatch(Action::NextPage).unwrap();
    let first = runtime.dispatch(Action::FirstCandidate).unwrap().view;
    assert_eq!(highlighted(&first), "candidate-0");
    assert_eq!(first.page, 0);

    // End reaches the last candidate there is, page and all. The fixture holds twelve at a page of
    // five, so that is the third page rather than the end of the first.
    let last = runtime.dispatch(Action::LastCandidate).unwrap().view;
    assert_eq!(highlighted(&last), "candidate-11");
    assert_eq!(last.page, 2);
    assert_eq!(last.page_count, 3);

    // Pressing it again stays put rather than walking further.
    let again = runtime.dispatch(Action::LastCandidate).unwrap().view;
    assert_eq!(highlighted(&again), "candidate-11");
}

// The Engine caps what it returns to a short query and hands the rest over when asked. End has to
// ask, or it lands on the last candidate that happened to be cached - and a second press would then
// move further, which is not what an End key does.
#[test]
fn the_last_candidate_is_the_last_one_the_engine_has() {
    let mut runtime = withholding_runtime(5, 4, 5);
    runtime.focus(true).unwrap();
    let page = type_key(&mut runtime).view;
    assert_eq!(page.page_count, 1);

    let last = runtime.dispatch(Action::LastCandidate).unwrap().view;
    assert_eq!(last.page_count, 2);
    assert_eq!(
        last.candidates
            .iter()
            .find(|candidate| candidate.highlighted)
            .unwrap()
            .text,
        "candidate-8"
    );
}

#[test]
fn edge_selection_checks_identity_and_routes_global_index() {
    for edge in [CandidateEdge::FirstHan, CandidateEdge::LastHan] {
        let mut active = runtime();
        active.focus(true).unwrap();
        let first = type_key(&mut active).view.candidates[0].id;
        let page = active.dispatch(Action::NextPage).unwrap().view;
        let id = page.candidates[1].id;
        assert_eq!(id.index, 6);
        let generation = active.view().generation;
        for invalid in [
            first,
            CandidateId {
                session: id.session + 1,
                ..id
            },
            CandidateId { index: 0, ..id },
            CandidateId { index: 10, ..id },
        ] {
            assert!(matches!(
                active.dispatch(Action::SelectEdge(invalid, edge)),
                Err(RuntimeError::StaleCandidate)
            ));
            assert_eq!(active.view().generation, generation);
        }
        let selected = active.dispatch(Action::SelectEdge(id, edge)).unwrap();
        assert_eq!(
            selected.commit.as_deref(),
            Some(match edge {
                CandidateEdge::FirstHan => "candidate-6-first",
                CandidateEdge::LastHan => "candidate-6-last",
            })
        );
        assert!(selected.view.editing_text.is_empty());
    }
}

#[test]
fn punctuation_finishes_highlighted_candidate_and_remaining_segments() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    runtime.dispatch(Action::NextPage).unwrap();
    let result = runtime
        .dispatch(Action::Character {
            value: b',',
            shift: false,
        })
        .unwrap();
    assert_eq!(
        result.commit.as_deref(),
        Some("candidate-5-remaining-segments，")
    );
    assert!(result.handled && result.view.editing_text.is_empty());
}

#[test]
fn ascii_punctuation_finishes_highlighted_candidate_for_keypad_marks() {
    for mark in *b".-+/*" {
        let mut runtime = runtime();
        runtime.focus(true).unwrap();
        type_key(&mut runtime);
        let result = runtime.dispatch(Action::PunctuationAscii(mark)).unwrap();
        let expected = format!("candidate-0-remaining-segments{}", mark as char);
        assert_eq!(result.commit.as_deref(), Some(expected.as_str()));
        assert!(result.handled && result.view.editing_text.is_empty());
    }
}

#[test]
fn unsupported_punctuation_is_appended_only_after_a_composition() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    let idle = runtime
        .dispatch(Action::Character {
            value: b'@',
            shift: false,
        })
        .unwrap();
    assert!(!idle.handled && idle.commit.is_none());
    type_key(&mut runtime);
    let result = runtime
        .dispatch(Action::Character {
            value: b'@',
            shift: false,
        })
        .unwrap();
    assert_eq!(
        result.commit.as_deref(),
        Some("candidate-0-remaining-segments@")
    );
}

#[test]
fn punctuation_failure_does_not_lose_an_already_finished_commit() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    let result = runtime
        .dispatch(Action::Character {
            value: b'!',
            shift: false,
        })
        .unwrap();
    assert_eq!(
        result.commit.as_deref(),
        Some("candidate-0-remaining-segments!")
    );
    assert!(result
        .diagnostic
        .unwrap()
        .contains("injected punctuation failure"));
}

#[test]
fn number_keys_select_the_visible_page_and_pass_through_when_idle() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    assert!(
        !runtime
            .dispatch(Action::Character {
                value: b'2',
                shift: false
            })
            .unwrap()
            .handled
    );
    type_key(&mut runtime);
    runtime.dispatch(Action::NextPage).unwrap();
    let result = runtime
        .dispatch(Action::Character {
            value: b'2',
            shift: false,
        })
        .unwrap();
    assert_eq!(result.commit.as_deref(), Some("candidate-6"));
}

#[test]
fn nine_key_mode_owns_digits_and_spelling_choices_are_generation_scoped() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    let original_generation = runtime.view().generation;
    runtime.set_nine_key_enabled(true).unwrap();
    assert!(runtime.view().nine_key && runtime.view().generation > original_generation);
    let typed = runtime
        .dispatch(Action::Character {
            value: b'6',
            shift: false,
        })
        .unwrap();
    assert!(typed.handled && typed.commit.is_none());
    assert_eq!(
        typed.view.nine_key_spellings,
        vec!["ni".to_owned(), "mi".to_owned()]
    );
    let invalid_digit = runtime
        .dispatch(Action::Character {
            value: b'1',
            shift: false,
        })
        .unwrap();
    assert!(!invalid_digit.handled && invalid_digit.commit.is_none());
    let separator = runtime
        .dispatch(Action::Character {
            value: b'\'',
            shift: false,
        })
        .unwrap();
    assert!(!separator.handled && separator.commit.is_none());
    let generation = separator.view.generation;
    let stale = NineKeySpellingId {
        session: separator.view.session,
        generation: generation - 1,
        index: 0,
    };
    assert!(matches!(
        runtime.dispatch(Action::ChooseNineKeySpelling(stale)),
        Err(RuntimeError::StaleNineKeySpelling)
    ));
    let invalid = NineKeySpellingId {
        session: separator.view.session,
        generation,
        index: 2,
    };
    assert!(matches!(
        runtime.dispatch(Action::ChooseNineKeySpelling(invalid)),
        Err(RuntimeError::StaleNineKeySpelling)
    ));
    let selected = runtime
        .dispatch(Action::ChooseNineKeySpelling(NineKeySpellingId {
            session: separator.view.session,
            generation,
            index: 1,
        }))
        .unwrap();
    assert!(selected.handled && selected.view.editing_text == "mi");
    assert!(matches!(
        runtime.set_nine_key_enabled(false),
        Err(RuntimeError::CompositionActive)
    ));
    runtime.dispatch(Action::Command(Command::Cancel)).unwrap();
    runtime.set_nine_key_enabled(false).unwrap();
    assert!(!runtime.view().nine_key);
    runtime.engine.scheme = 1;
    runtime.refresh().unwrap();
    assert!(matches!(
        runtime.set_nine_key_enabled(true),
        Err(RuntimeError::InvalidNineKeyScheme)
    ));
}

#[test]
fn unavailable_numeric_slot_does_not_jump_back_to_first_page() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    runtime.dispatch(Action::NextPage).unwrap();
    runtime.dispatch(Action::NextPage).unwrap();
    let result = runtime
        .dispatch(Action::Character {
            value: b'9',
            shift: false,
        })
        .unwrap();
    assert!(result.handled && result.commit.is_none());
    assert_eq!(result.view.page, 2);
}

#[test]
fn engine_mode_is_authoritative_and_resets_old_highlight() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    runtime.dispatch(Action::NextPage).unwrap();
    runtime.engine.local_mode = "unicode".into();
    let result = runtime
        .dispatch(Action::Character {
            value: b'0',
            shift: false,
        })
        .unwrap();
    assert_eq!(result.view.local_mode, "unicode");
    assert_eq!(result.view.page, 0);
    assert!(!result.view.editing_text.starts_with('U'));
}

#[test]
fn dedicated_english_state_resets_highlight_without_guessing_from_text() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    runtime.dispatch(Action::NextPage).unwrap();
    let text = runtime.view().editing_text;
    assert!(!runtime.view().dedicated_english);
    assert_eq!(runtime.view().page, 1);
    runtime.engine.dedicated_english = true;
    runtime.refresh().unwrap();
    assert!(runtime.view().dedicated_english);
    assert_eq!(runtime.view().page, 0);
    assert_eq!(runtime.view().editing_text, text);
    assert_eq!(runtime.view().local_mode, "none");
    runtime.engine.dedicated_english = false;
    runtime.refresh().unwrap();
    assert!(!runtime.view().dedicated_english);
}

#[test]
fn switching_the_language_drops_the_composition_being_spelled() {
    // The source pairs `SetEnglishInputMode` with `ClearState`, and the engine does the same inside
    // `set_dedicated_english_mode`: letters spelled for Chinese are not what the user wants sitting
    // in an English composition. The hosts reach this through `msime_client_set_english_mode`, which
    // is what every mode-switch chord ends up calling -- Shift, a Ctrl tap, Ctrl+Alt+Space and
    // Ctrl+Shift+E alike.
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    assert!(!runtime.view().editing_text.is_empty());
    runtime.set_dedicated_english(true).unwrap();
    assert!(runtime.view().dedicated_english);
    assert_eq!(runtime.view().editing_text, "");
    // Setting the same mode again is not a change, so there is nothing to reset and nothing to lose.
    type_key(&mut runtime);
    let spelled = runtime.view().editing_text.clone();
    assert!(!spelled.is_empty());
    runtime.set_dedicated_english(true).unwrap();
    assert_eq!(runtime.view().editing_text, spelled);
}

#[test]
fn commit_context_precedes_mode_reset_for_every_selection_route() {
    for route in 0..5 {
        let mut runtime = runtime();
        runtime.focus(true).unwrap();
        runtime.engine.local_mode = "unicode".into();
        let view = type_key(&mut runtime).view;
        let id = view.candidates[0].id;
        let action = match route {
            0 => Action::Select(id),
            1 => Action::SelectEdge(id, CandidateEdge::FirstHan),
            2 => Action::SelectHighlighted,
            3 => Action::Finish,
            _ => Action::Character {
                value: b'1',
                shift: false,
            },
        };
        let committed = runtime.dispatch(action).unwrap();
        assert!(committed.commit.is_some());
        assert_eq!(committed.commit_context.unwrap().local_mode, "unicode");
        assert_eq!(committed.view.local_mode, "none");
    }
}

#[test]
fn finish_preserves_engine_completion_of_remaining_segments() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    runtime.dispatch(Action::NextPage).unwrap();
    let result = runtime.dispatch(Action::Finish).unwrap();
    assert_eq!(
        result.commit.as_deref(),
        Some("candidate-5-remaining-segments")
    );
    assert!(result.view.preedit.is_empty());
}
#[test]
fn stale_views_and_other_sessions_cannot_select() {
    let mut a = runtime();
    let mut b = runtime();
    a.focus(true).unwrap();
    b.focus(true).unwrap();
    let id = type_key(&mut a).view.candidates[0].id;
    type_key(&mut b);
    assert!(matches!(
        b.dispatch(Action::Select(id)),
        Err(RuntimeError::StaleCandidate)
    ));
    a.dispatch(Action::NextCandidate).unwrap();
    assert!(matches!(
        a.dispatch(Action::Select(id)),
        Err(RuntimeError::StaleCandidate)
    ));
}
#[test]
fn cache_maintenance_reaches_engine_without_acquiring_focus() {
    let mut runtime = runtime();
    let idle = runtime.dispatch(Action::ResetCache).unwrap();
    assert!(idle.handled);
    assert!(idle.commit.is_none());
    assert_eq!(runtime.engine.cache_resets, 1);
    assert!(!runtime.focused);
    assert!(!type_key(&mut runtime).handled);

    runtime.focus(true).unwrap();
    let composed = type_key(&mut runtime);
    let refreshed = runtime.dispatch(Action::ResetCache).unwrap();
    assert_eq!(runtime.engine.cache_resets, 2);
    assert_eq!(refreshed.view.preedit, composed.view.preedit);
    assert!(refreshed.commit.is_none());

    runtime.focus(false).unwrap();
    assert!(runtime.dispatch(Action::ResetCache).unwrap().handled);
    assert_eq!(runtime.engine.cache_resets, 3);
    assert!(!runtime.focused);
    assert!(!type_key(&mut runtime).handled);
}
#[test]
fn unfocused_keys_pass_through_and_blur_cancels_composition() {
    let mut runtime = runtime();
    assert!(!type_key(&mut runtime).handled);
    runtime.focus(true).unwrap();
    let id = type_key(&mut runtime).view.candidates[0].id;
    assert!(runtime.focus(false).unwrap().view.preedit.is_empty());
    assert!(!type_key(&mut runtime).handled);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    assert!(matches!(
        runtime.dispatch(Action::Select(id)),
        Err(RuntimeError::StaleCandidate)
    ));
}
#[test]
fn character_width_conversion_preserves_non_ascii_and_roundtrips_ascii() {
    let full = crate::character_width::to_fullwidth("A 1!");
    assert_eq!(full, "Ａ　１！");
    assert_eq!(crate::character_width::to_halfwidth(&full), "A 1!");
    assert_eq!(crate::character_width::to_fullwidth("中文"), "中文");
}

/// `move_to_back` is the whole of the demotion rule that can be tested without an engine, and
/// the version this replaced shipped with no test at all — which is how it reached `develop`
/// dropping Japanese katakana and, separately, the model's own runner-up choices.
#[test]
fn demotion_moves_flagged_items_to_the_end_and_keeps_both_orders() {
    let mut items = vec!["a", "b", "c", "d", "e"];
    crate::move_to_back(&mut items, &[false, true, false, true, false]);
    assert_eq!(items, vec!["a", "c", "e", "b", "d"]);
}

#[test]
fn demotion_loses_nothing() {
    // The point of moving rather than removing: every candidate is still reachable by paging.
    let mut items: Vec<u32> = (0..9).collect();
    crate::move_to_back(
        &mut items,
        &[false, true, true, false, true, false, false, true, true],
    );
    let mut sorted = items.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..9).collect::<Vec<u32>>());
    assert_eq!(items.len(), 9);
}

#[test]
fn demotion_with_no_flags_is_identity() {
    let mut items = vec![1, 2, 3];
    crate::move_to_back(&mut items, &[false, false, false]);
    assert_eq!(items, vec![1, 2, 3]);
}

#[test]
fn a_short_flag_list_leaves_the_tail_in_place() {
    // Defensive: the parallel arrays are length-checked before this runs, but a mismatch must
    // not reorder anything it was not told about.
    let mut items = vec![1, 2, 3, 4];
    crate::move_to_back(&mut items, &[true]);
    assert_eq!(items, vec![2, 3, 4, 1]);
}

/// Only `CandidateSource::Generated` names alternative readings of one key. Every other source
/// is plural by design — English words, emoji, kaomoji, quick phrases, AI suggestions — and an
/// earlier version of this rule kept one of each and dropped the rest.
#[test]
fn only_the_lattice_source_is_treated_as_alternative_readings() {
    assert_eq!(crate::LATTICE_SOURCE, 8);
    for plural in [2u8, 3, 4, 5, 6, 7] {
        assert_ne!(crate::LATTICE_SOURCE, plural);
    }
}

// The real Engine, not the fixture. Unicode mode is the one place where a bare
// digit is input rather than a candidate index, and the two layers decide that
// separately: the Engine reports the digit as handled, and the runtime only
// falls through to selection for a digit the Engine refused. A regression in
// either one silently turns "U4e2d" into a candidate pick, and the Windows and
// macOS suites that would notice both need their own host to run.
fn real_engine_options(root: &std::path::Path) -> msime_engine_bridge::EngineOptions {
    let path = |name: &str| {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path.to_str().unwrap().to_owned()
    };
    msime_engine_bridge::EngineOptions {
        resources: path("resources"),
        user_data: path("user"),
        cache: path("cache"),
        dictionaries: path("dictionaries"),
        scheme: 0,
        shuangpin_profile: 0,
        shuangpin_preedit_uses_raw: true,
        learning: false,
        autocorrect_transposition: true,
        autocorrect_neighbor: true,
        fuzzy_pinyin_rules: 0,
        wubi_mixed_pinyin: false,
        helpcode: false,
        show_helpcode: true,
        helpcode_schema: "ziranma".into(),
        chinese_punctuation: true,
        paired_punctuation: true,
        punctuation_lock: 0,
        frequency_mode: "promote".into(),
        frequency_trigger_count: 1,
        frequency_linear_step: 1,
        mixed_english: true,
        english_minimum_prefix: 2,
        mixed_emoji: false,
        mixed_kaomoji: false,
        local_unicode: true,
        local_date_time: true,
        local_quick_phrase: true,
        local_emoji: true,
        local_kaomoji: true,
        local_super_jianpin: true,
        local_temporary_english: true,
        local_temporary_japanese: true,
        sentence_alternatives: true,
    }
}

#[test]
fn unicode_mode_digits_compose_a_code_point_rather_than_picking_a_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let session =
        msime_engine_bridge::Session::new(&real_engine_options(directory.path())).unwrap();
    let mut runtime = Runtime::new(session, 5).unwrap();
    runtime.focus(true).unwrap();

    let shift_u = runtime
        .dispatch(Action::Character {
            value: b'U',
            shift: true,
        })
        .unwrap();
    assert_eq!(shift_u.view.local_mode, "unicode");

    for value in *b"4e2d" {
        let transition = runtime
            .dispatch(Action::Character {
                value,
                shift: false,
            })
            .unwrap();
        // A digit read as a candidate index would commit here and leave the mode.
        assert!(
            transition.commit.is_none(),
            "{} committed instead of extending the code point",
            value as char
        );
        assert_eq!(transition.view.local_mode, "unicode");
    }
    assert_eq!(runtime.view().editing_text, "U4e2d");
    assert!(runtime
        .view()
        .candidates
        .iter()
        .any(|candidate| candidate.text == "中"));
}

/// Without a settled model attached, the settle call is inert.
///
/// This is the shape every installation that ships one model is in, and the one where a mistake
/// would be invisible: a settle pass that quietly reordered candidates using the fast model would
/// look like the candidate window moving on its own after the user stopped typing.
#[test]
fn settling_without_a_second_model_changes_nothing() {
    let mut runtime = runtime();
    runtime.focus(true).expect("focus");
    for byte in b"nihao" {
        runtime
            .dispatch(Action::Character {
                value: *byte,
                shift: false,
            })
            .expect("type");
    }
    let before: Vec<String> = runtime
        .view()
        .candidates
        .iter()
        .map(|candidate| candidate.text.clone())
        .collect();
    assert!(!runtime.rerank_settled(), "no settled model, nothing to do");
    let after: Vec<String> = runtime
        .view()
        .candidates
        .iter()
        .map(|candidate| candidate.text.clone())
        .collect();
    assert_eq!(after, before);
}

/// An idle session has no candidates to settle on, and asking is not an error.
#[test]
fn settling_while_idle_is_inert() {
    let mut runtime = runtime();
    runtime.focus(true).expect("focus");
    assert!(!runtime.rerank_settled());
}

fn withholding_runtime(offered: usize, withheld: usize, page_size: u8) -> Runtime<Fixture> {
    Runtime::new(
        Fixture {
            scheme: 0,
            dedicated_english: false,
            nine_key: false,
            nine_key_spellings: Vec::new(),
            local_mode: "none".into(),
            words: (0..offered).map(|n| format!("candidate-{n}")).collect(),
            codes: Vec::new(),
            text: String::new(),
            snapshot_fails: false,
            balanced_openings: Vec::new(),
            cache_resets: 0,
            withheld: (offered..offered + withheld)
                .map(|n| format!("candidate-{n}"))
                .collect(),
            sources: Vec::new(),
            remaining_after_select: None,
            positions: Vec::new(),
            reading: String::new(),
        },
        page_size,
    )
    .unwrap()
}

#[test]
fn paging_reaches_candidates_the_engine_withheld() {
    // Twelve offered at five a page is two full pages and a short third. Without the expansion the
    // third page is the end of the road, and the eight held back are unreachable by any key.
    let mut runtime = withholding_runtime(12, 8, 5);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    assert_eq!(runtime.view().page_count, 3);
    let second = runtime.dispatch(Action::NextPage).unwrap().view;
    assert_eq!(second.page, 1);
    // Entering the short last page is where the expansion belongs: the page is filled before it is
    // shown, rather than appearing short and then growing under the user.
    let third = runtime.dispatch(Action::NextPage).unwrap().view;
    assert_eq!(third.page, 2);
    assert_eq!(third.page_count, 4);
    assert_eq!(third.candidates.len(), 5);
    let fourth = runtime.dispatch(Action::NextPage).unwrap().view;
    assert_eq!(fourth.page, 3);
    assert_eq!(
        fourth.candidates.first().map(|c| c.text.as_str()),
        Some("candidate-15")
    );
}

#[test]
fn the_full_list_holds_what_paging_would_have_reached() {
    // Twelve offered and eight held back. Paging to the last page releases the eight; a host that
    // opens the whole list instead never paged, so it used to see only the first twelve and the
    // panel that promises everything was short by the tail of the answer.
    let mut runtime = withholding_runtime(12, 8, 5);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    assert_eq!(runtime.all_candidates().candidates.len(), 20);
    // Asking again is stable: there is nothing left to release and the list does not shift.
    let repeated = runtime.all_candidates();
    assert_eq!(repeated.candidates.len(), 20);
    assert_eq!(
        repeated.candidates.last().map(|c| c.text.as_str()),
        Some("candidate-19")
    );
    // And it agrees with what paging reaches, which is the behaviour it is standing in for.
    let mut paged = withholding_runtime(12, 8, 5);
    paged.focus(true).unwrap();
    type_key(&mut paged);
    for _ in 0..3 {
        paged.dispatch(Action::NextPage).unwrap();
    }
    assert_eq!(paged.all_candidates().candidates.len(), 20);
}

#[test]
fn expansion_that_fills_the_current_page_does_not_advance_past_it() {
    // Three offered is a single short page. Asking for the next one has nowhere to go, so the
    // arrivals fill this page instead - advancing would step straight over them.
    let mut runtime = withholding_runtime(3, 4, 5);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    let filled = runtime.dispatch(Action::NextPage).unwrap().view;
    assert_eq!(filled.page, 0);
    assert_eq!(filled.page_count, 2);
    assert_eq!(filled.candidates.len(), 5);
    assert_eq!(
        filled.candidates.first().map(|c| c.text.as_str()),
        Some("candidate-0")
    );
    // The page is no longer short, so the next request moves on as usual.
    assert_eq!(runtime.dispatch(Action::NextPage).unwrap().view.page, 1);
}

#[test]
fn walking_the_highlight_off_the_end_reaches_candidates_the_engine_withheld() {
    // Ten offered at five a page is two full pages, so the partial-last-page rule below never fires
    // and this exercises the end of the list on its own. Walking down one candidate at a time - an
    // arrow key, a wheel notch - used to stop dead on the tenth, while page-down on the same query
    // walked past it. The cap is the Engine's single-letter limit, and selection has to release it
    // for the same reason paging does.
    let mut runtime = withholding_runtime(10, 8, 5);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    for _ in 0..9 {
        runtime.dispatch(Action::NextCandidate).unwrap();
    }
    let last_offered = runtime.view();
    assert_eq!(last_offered.page_count, 2);
    let expanded = runtime.dispatch(Action::NextCandidate).unwrap().view;
    assert_eq!(expanded.page, 2);
    assert_eq!(expanded.page_count, 4);
    assert_eq!(
        expanded
            .candidates
            .iter()
            .find(|candidate| candidate.highlighted)
            .map(|candidate| candidate.text.as_str()),
        Some("candidate-10")
    );
}

#[test]
fn stepping_into_the_partial_last_page_fills_it_first() {
    // The page-down path already fills the short last page before entering it. Arriving at the same
    // page by selection has to look the same, or the page appears short and then grows under a
    // highlight that is already sitting in it.
    let mut runtime = withholding_runtime(12, 8, 5);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    for _ in 0..9 {
        runtime.dispatch(Action::NextCandidate).unwrap();
    }
    let entered = runtime.dispatch(Action::NextCandidate).unwrap().view;
    assert_eq!(entered.page, 2);
    assert_eq!(entered.candidates.len(), 5);
    assert_eq!(
        entered.candidates.first().map(|c| c.text.as_str()),
        Some("candidate-10")
    );
}

#[test]
fn the_highlight_stops_at_the_last_candidate_once_nothing_is_withheld() {
    // Expansion is not a wrap: when the Engine has nothing left, the selection stays where it is
    // rather than moving or reordering the list under it.
    let mut runtime = withholding_runtime(3, 0, 5);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    for _ in 0..2 {
        runtime.dispatch(Action::NextCandidate).unwrap();
    }
    let end = runtime.dispatch(Action::NextCandidate).unwrap().view;
    assert_eq!(end.candidates.len(), 3);
    assert_eq!(
        end.candidates
            .iter()
            .position(|candidate| candidate.highlighted),
        Some(2)
    );
}

#[test]
fn walking_the_highlight_backwards_never_asks_for_more() {
    // Only forward motion runs into the cap. Asking the Engine to expand while moving up would
    // reorder the list the user is reading back through.
    let mut runtime = withholding_runtime(10, 8, 5);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    for _ in 0..8 {
        runtime.dispatch(Action::NextCandidate).unwrap();
    }
    runtime.dispatch(Action::PreviousCandidate).unwrap();
    assert_eq!(runtime.view().page_count, 2);
}

#[test]
fn an_engine_withholding_nothing_pages_exactly_as_before() {
    let mut runtime = withholding_runtime(12, 0, 5);
    runtime.focus(true).unwrap();
    type_key(&mut runtime);
    for expected in [1, 2, 2, 2] {
        assert_eq!(
            runtime.dispatch(Action::NextPage).unwrap().view.page,
            expected
        );
    }
    assert_eq!(runtime.view().page_count, 3);
}

// The runtime seats online candidates and reranks the Engine's list, so the page a host renders is not in the Engine's order. A selection names a seat on that page; the Engine has to be asked for the candidate sitting there, not for whatever it holds at the same number. fcitx5-native-ai caught this as an AI candidate shown in slot 1 committing the Engine's own second candidate.
#[test]
fn selecting_a_reseated_candidate_commits_that_candidate() {
    let mut runtime = Runtime::new(
        Fixture {
            local_mode: "none".into(),
            words: vec!["本地一".into(), "本地二".into(), "AI".into()],
            codes: (0..3).map(|n| format!("code-{n}")).collect(),
            sources: vec![0, 0, 3],
            ..Fixture::default()
        },
        9,
    )
    .unwrap();
    runtime.focus(true).unwrap();
    let page = type_key(&mut runtime).view.candidates;
    assert_eq!(
        page.iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>(),
        vec!["本地一", "AI", "本地二"]
    );

    let done = runtime.dispatch(Action::Select(page[1].id)).unwrap();
    assert_eq!(done.commit.as_deref(), Some("AI"));
}

// A phrase held over an emptied reading is still a composition: swapping the engine under it would
// retype the old scheme's reading into the new one on the next Backspace.
#[test]
fn a_held_phrase_over_an_emptied_reading_is_not_idle() {
    let mut runtime = phrase_runtime("haitanpaobu", vec![6]);
    let id = runtime.view().candidates[0].id;
    runtime.dispatch(Action::Select(id)).unwrap();
    let kept = runtime.dispatch(Action::SegmentBackspace).unwrap();
    assert_eq!(kept.view.phrase_prefix, "海滩");
    assert!(kept.view.editing_text.is_empty());
    assert!(!runtime.is_idle());
    let generation = runtime.view().generation;
    assert!(matches!(
        runtime.replace_engine(PhraseEngine::new(vec![6]), 5),
        Err(RuntimeError::CompositionActive)
    ));
    assert!(matches!(
        runtime.set_page_size(7),
        Err(RuntimeError::CompositionActive)
    ));
    assert!(matches!(
        runtime.set_nine_key_enabled(false),
        Err(RuntimeError::CompositionActive)
    ));
    assert_eq!(runtime.view().generation, generation);
    // Dropping the held piece ends the composition.
    runtime.dispatch(Action::SegmentBackspace).unwrap();
    assert!(runtime.is_idle());
}

/// Settling that moves nothing keeps the generation, so a host that redraws on a new generation
/// does not flicker on every pause.
#[test]
fn settling_that_moves_nothing_keeps_the_generation() {
    let mut runtime = runtime();
    runtime.focus(true).unwrap();
    let page = type_key(&mut runtime).view;
    assert!(!runtime.rerank_settled());
    assert_eq!(runtime.view().generation, page.generation);
}

#[test]
fn the_full_list_seats_online_candidates_as_paging_does_and_retires_page_ids() {
    // The released tail carries a cloud candidate. Paging seats it second; the panel has to agree,
    // and the page drawn before the release must not select by the reordered seats.
    let with_cloud = || {
        let mut runtime = withholding_runtime(12, 8, 5);
        runtime.engine.sources = (0..20).map(|n| if n == 15 { 2 } else { 0 }).collect();
        runtime.engine.codes = vec![String::new(); 20];
        runtime.focus(true).unwrap();
        runtime
    };
    let mut runtime = with_cloud();
    let page = type_key(&mut runtime).view;
    let panel = runtime.all_candidates();
    assert_eq!(panel.candidates.len(), 20);
    assert_eq!(panel.candidates[1].text, "candidate-15");
    assert_eq!(panel.generation, page.generation + 1);
    assert!(matches!(
        runtime.dispatch(Action::Select(page.candidates[1].id)),
        Err(RuntimeError::StaleCandidate)
    ));

    let mut paged = with_cloud();
    type_key(&mut paged);
    for _ in 0..3 {
        paged.dispatch(Action::NextPage).unwrap();
    }
    let texts = |snapshot: CandidateSnapshot| -> Vec<String> {
        snapshot.candidates.into_iter().map(|c| c.text).collect()
    };
    assert_eq!(texts(paged.all_candidates()), texts(panel));
}

/// An engine whose snapshot fails once a candidate has been committed.
struct FailsAfterCommit {
    inner: Fixture,
    committed: bool,
}

impl InputEngine for FailsAfterCommit {
    fn snapshot(&self) -> Result<EngineSnapshot, RuntimeError> {
        if self.committed {
            return Err(RuntimeError::Engine("injected snapshot failure".into()));
        }
        self.inner.snapshot()
    }
    fn character(&mut self, value: u8, shift: bool) -> Result<EngineResult, RuntimeError> {
        self.inner.character(value, shift)
    }
    fn command(&mut self, command: Command) -> Result<EngineResult, RuntimeError> {
        self.inner.command(command)
    }
    fn select(&mut self, index: usize) -> Result<EngineResult, RuntimeError> {
        self.committed = true;
        self.inner.select(index)
    }
    fn select_edge(
        &mut self,
        index: usize,
        edge: CandidateEdge,
    ) -> Result<EngineResult, RuntimeError> {
        self.committed = true;
        self.inner.select_edge(index, edge)
    }
    fn finish(&mut self, index: usize) -> Result<EngineResult, RuntimeError> {
        self.inner.finish(index)
    }
    fn punctuation(&mut self, value: u8) -> Result<EngineResult, RuntimeError> {
        self.inner.punctuation(value)
    }
}

#[test]
fn a_wubi_auto_commit_survives_a_failed_refresh() {
    let mut runtime = Runtime::new(
        FailsAfterCommit {
            inner: Fixture {
                scheme: 2,
                words: vec!["合成候选".into()],
                ..Fixture::default()
            },
            committed: false,
        },
        5,
    )
    .unwrap();
    runtime.focus(true).unwrap();
    let mut last = None;
    for value in b"wqaa" {
        last = Some(
            runtime
                .dispatch(Action::Character {
                    value: *value,
                    shift: false,
                })
                .unwrap(),
        );
    }
    let last = last.unwrap();
    assert_eq!(last.commit.as_deref(), Some("合成候选"));
    assert!(last
        .diagnostic
        .as_deref()
        .is_some_and(|diagnostic| diagnostic.starts_with("Candidate refresh failed")));
}

#[test]
fn a_busy_provider_keeps_only_the_newest_completed_result() {
    let query = |text: &str| OnlineQuery {
        scheme: 0,
        generation: 1,
        identity: text.into(),
        query_text: text.into(),
        cache_key: text.into(),
        pinyin_segments: vec![],
        cloud_eligible: true,
        ai_eligible: false,
        cloud_candidates: true,
        session_id: 1,
        ai_context: String::new(),
        ai_assistant: None,
        ai_cache_only: false,
    };
    let (started, first_running) = std::sync::mpsc::channel::<()>();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(gate);
    let worker = OnlineProviderWorker::spawn(1, move |query: OnlineQuery| {
        if query.query_text == "ni" {
            started.send(()).unwrap();
            gate.lock().unwrap().recv().unwrap();
        }
        Some((query.query_text.clone(), 0))
    })
    .unwrap();
    assert!(worker.submit(query("ni")));
    first_running
        .recv_timeout(Duration::from_secs(5))
        .expect("first query running");
    for text in ["nih", "niha", "nihao"] {
        assert!(worker.submit(query(text)));
    }
    release.send(()).unwrap();
    let mut answered = None;
    for _ in 0..500 {
        if let Some(result) = worker.try_recv() {
            answered = Some(result.text);
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(answered, Some("nihao".to_owned()));
    assert!(worker.try_recv().is_none());
    worker.shutdown();
}
