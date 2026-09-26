//! Unit tests for the parent module, in their own file because the module
//! is large enough that mixing them with the implementation obscured both.
//! Same `mod tests` as before, so `use super::*` still names the parent.

use super::*;

#[test]
fn selection_statistics_use_the_candidate_id_absolute_index() {
    let action = Action::Select(CandidateId {
        session: 7,
        generation: 11,
        index: 9,
    });
    assert_eq!(selected_position(&action), Some(10));
    let action = Action::SelectAnyCandidate(CandidateId {
        session: 7,
        generation: 11,
        index: 14,
    });
    assert_eq!(selected_position(&action), Some(15));
}

#[test]
fn default_sentence_model_uses_verified_resources_not_prepared_dictionaries() {
    let root = tempfile::tempdir().unwrap();
    let resources = root.path().join("resources");
    let dictionaries = root.path().join("user/dictionaries/generation");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::create_dir_all(&dictionaries).unwrap();
    std::fs::write(resources.join("sentence-model.safetensors"), b"synthetic").unwrap();

    let actual = ffi::sentence_model_path(resources.to_str().unwrap(), None);
    assert_eq!(actual, resources.join("sentence-model.safetensors"));
    assert!(actual.is_file());
    assert!(!dictionaries.join("sentence-model.safetensors").exists());

    let explicit = root.path().join("custom-model.safetensors");
    assert_eq!(
        ffi::sentence_model_path(resources.to_str().unwrap(), explicit.to_str()),
        explicit
    );
}

#[test]
fn windows_legacy_mixed_input_is_imported_without_leaking_other_config() {
    let mut preferences = Preferences::default();
    assert!(apply_windows_legacy_mixed_input(
        "[general]\ncn_en_mixed_input = false\ncn_en_mixed_input_min_chars = 5\nemoji_mixed_input = true\nkaomoji_mixed_input = true\ndiagnostic_log = true\n",
        &mut preferences,
    ));
    assert!(!preferences.mixed_input.english);
    assert_eq!(preferences.mixed_input.minimum_prefix, 5);
    assert!(preferences.mixed_input.emoji);
    assert!(preferences.mixed_input.kaomoji);
    assert!(!preferences.diagnostic_log.server);
    assert!(!preferences.diagnostic_log.tsf);
}

#[test]
fn windows_legacy_mixed_input_ignores_invalid_values_and_documents() {
    let mut preferences = Preferences::default();
    assert!(!apply_windows_legacy_mixed_input(
        "[general]\ncn_en_mixed_input_min_chars = 9\n",
        &mut preferences,
    ));
    assert_eq!(preferences.mixed_input.minimum_prefix, 5);
    assert!(!apply_windows_legacy_mixed_input(
        "not toml",
        &mut preferences
    ));
}

#[test]
fn local_mode_resource_gates_preserve_unrelated_modes() {
    let root = tempfile::tempdir().unwrap();
    for name in ["others.db", "english.db", "dict_japanese.dat"] {
        std::fs::write(root.path().join(name), b"fixture").unwrap();
    }
    let mut options = EngineOptions {
        resources: root.path().to_string_lossy().into_owned(),
        user_data: root.path().to_string_lossy().into_owned(),
        cache: root.path().to_string_lossy().into_owned(),
        dictionaries: root.path().to_string_lossy().into_owned(),
        scheme: 0,
        shuangpin_profile: 0,
        shuangpin_preedit_uses_raw: true,
        learning: false,
        autocorrect_transposition: false,
        autocorrect_neighbor: false,
        fuzzy_pinyin_rules: 0,
        wubi_mixed_pinyin: false,
        helpcode: false,
        show_helpcode: false,
        helpcode_schema: "ziranma".into(),
        chinese_punctuation: true,
        paired_punctuation: true,
        punctuation_lock: 0,
        frequency_mode: "disabled".into(),
        frequency_trigger_count: 1,
        frequency_linear_step: 1,
        mixed_english: false,
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
    };
    apply_local_mode_resource_gates(&mut options);
    assert!(options.local_unicode);
    assert!(options.local_date_time);
    assert!(options.local_quick_phrase);
    assert!(options.local_super_jianpin);
    assert!(options.local_emoji);
    assert!(options.local_kaomoji);
    assert!(options.local_temporary_english);
    assert!(options.local_temporary_japanese);

    std::fs::remove_file(root.path().join("others.db")).unwrap();
    std::fs::remove_file(root.path().join("dict_japanese.dat")).unwrap();
    apply_local_mode_resource_gates(&mut options);
    assert!(!options.local_emoji);
    assert!(!options.local_kaomoji);
    assert!(!options.local_temporary_japanese);
    assert!(options.local_temporary_english);
    assert!(options.local_unicode);
    assert!(options.local_date_time);
    assert!(options.local_quick_phrase);
    assert!(options.local_super_jianpin);
}

#[test]
fn doubao_frame_codec_is_available_through_c_abi() {
    let request = msime_client_core::voice::doubao_frame::encode_json_frame(
        9,
        0,
        1,
        br#"{"result":{"text":"fixture"}}"#,
    );
    let mut response = request[..4].to_vec();
    response.extend_from_slice(&request[8..12]);
    response.extend_from_slice(&request[12..]);
    let decoded =
        read(unsafe { msime_client_doubao_decode_frame(response.as_ptr(), response.len()) });
    assert_eq!(decoded["ok"], true);
    assert_eq!(decoded["value"]["last"], false);
    assert_eq!(
        decoded["value"]["payload"],
        r#"{"result":{"text":"fixture"}}"#
    );

    let error = [0x11, 0xf0, 0x11, 0, 0, 0, 0, 7, 0, 0, 0, 42];
    let decoded_error =
        read(unsafe { msime_client_doubao_decode_frame(error.as_ptr(), error.len()) });
    assert_eq!(decoded_error["value"]["error_code"], 7);

    let mut start = vec![0u8; 4096];
    let mut written = 0usize;
    assert!(unsafe {
        msime_client_doubao_start_frame(
            true,
            false,
            true,
            b"table".as_ptr(),
            5,
            start.as_mut_ptr(),
            start.len(),
            &mut written,
        )
    });
    assert_eq!(&start[..4], &[0x11, 0x11, 0x11, 0]);
    assert!(written > 12);

    // Mobile bindings size their Java/ArkTS result from a null, zero-capacity probe.
    let mut required = 0usize;
    assert!(!unsafe {
        msime_client_doubao_start_frame(
            true,
            false,
            true,
            b"table".as_ptr(),
            5,
            std::ptr::null_mut(),
            0,
            &mut required,
        )
    });
    assert_eq!(required, written);

    let mut audio = vec![0u8; 1024];
    let mut audio_written = 0usize;
    assert!(unsafe {
        msime_client_doubao_audio_frame(
            2,
            [0u8, 1, 2, 3].as_ptr(),
            4,
            true,
            audio.as_mut_ptr(),
            audio.len(),
            &mut audio_written,
        )
    });
    assert_eq!(&audio[..4], &[0x11, 0x23, 0x11, 0]);

    let mut audio_required = 0usize;
    assert!(!unsafe {
        msime_client_doubao_audio_frame(
            2,
            [0u8, 1, 2, 3].as_ptr(),
            4,
            true,
            std::ptr::null_mut(),
            0,
            &mut audio_required,
        )
    });
    assert_eq!(audio_required, audio_written);
}

#[test]
fn surface_route_boundary_resolves_panels_and_rejects_bad_buffers() {
    let parse = |value: &str| {
        // SAFETY: the slice outlives the call.
        read(unsafe { msime_client_parse_surface_route(value.as_ptr(), value.len()) })
    };

    let keyboard = parse("keyboard");
    assert_eq!(keyboard["ok"], true);
    assert_eq!(keyboard["value"]["route"], "keyboard");
    assert_eq!(keyboard["value"]["panel"]["label"], "keyboard-panel");
    assert_eq!(keyboard["value"]["panel"]["width"], 1100);

    // The local clipboard panel the desktop host ships must stay reachable.
    let clipboard = parse("clipboard");
    assert_eq!(clipboard["value"]["panel"]["label"], "clipboard-panel");
    assert_eq!(clipboard["value"]["panel"]["height"], 620);

    let deep_link = parse("settings:voice");
    assert_eq!(deep_link["ok"], true);
    assert_eq!(deep_link["value"]["route"], "settings:voice");
    // Settings is the main window, so it carries no panel geometry.
    assert!(deep_link["value"].get("panel").is_none());

    for rejected in ["", "account", "settings:unknown", "Settings"] {
        assert_eq!(parse(rejected)["ok"], false, "accepted {rejected:?}");
    }

    // A null buffer and an oversized length are refused, not dereferenced.
    assert_eq!(
        read(unsafe { msime_client_parse_surface_route(std::ptr::null(), 8) })["ok"],
        false
    );
    let value = "settings";
    assert_eq!(
        read(unsafe { msime_client_parse_surface_route(value.as_ptr(), 4096) })["ok"],
        false
    );
    let invalid = [0xff_u8, 0xfe];
    assert_eq!(
        read(unsafe { msime_client_parse_surface_route(invalid.as_ptr(), invalid.len()) })["ok"],
        false
    );
}

#[test]
fn smart_punctuation_gesture_boundary_arms_and_decides_from_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host_preferences(
        dir.path(),
        Preferences {
            smart_punctuation: true,
            smart_punctuation_repeat: true,
            smart_punctuation_space_convert: true,
            ..chinese_preferences()
        },
    );
    let arm = |body: serde_json::Value| {
        let text = body.to_string();
        // SAFETY: the buffer outlives the call.
        read(unsafe { msime_client_smart_punctuation_arm(handle, text.as_ptr(), text.len()) })
    };
    let decide = |body: serde_json::Value| {
        let text = body.to_string();
        // SAFETY: the buffer outlives the call.
        read(unsafe { msime_client_smart_punctuation_decide(handle, text.as_ptr(), text.len()) })
    };

    // An ASCII mark the host committed arms the repeat gesture and nothing else.
    let armed = arm(json!({
        "ascii": 44, "commit": ",", "timestamp_ms": 1_000,
        "editor_generation": 3, "auto_closed_pair": false
    }));
    assert_eq!(armed["ok"], true);
    assert_eq!(armed["value"]["repeat"]["committed"], ",");
    assert!(
        armed["value"]["space"].is_null(),
        "a comma is not a Chinese mark"
    );

    let repeat = armed["value"]["repeat"].clone();
    let replaced = decide(json!({
        "character": 44, "preceding": ",", "timestamp_ms": 1_500,
        "editor_generation": 3, "repeat": repeat, "space": null
    }));
    assert_eq!(replaced["value"]["replace_with"], "，");
    assert!(replaced["value"]["space_ascii"].is_null());

    // Past the window, in another editor, or over a character that is no longer there: nothing.
    for (timestamp, generation, preceding) in [(4_000, 3, ","), (1_500, 9, ","), (1_500, 3, "好")]
    {
        let answer = decide(json!({
            "character": 44, "preceding": preceding, "timestamp_ms": timestamp,
            "editor_generation": generation, "repeat": armed["value"]["repeat"], "space": null
        }));
        assert!(
            answer["value"]["replace_with"].is_null(),
            "{timestamp} {generation} {preceding} still replaced"
        );
    }

    // A committed Chinese mark arms the space conversion instead.
    let armed = arm(json!({
        "ascii": 46, "commit": "。", "timestamp_ms": 2_000,
        "editor_generation": 3, "auto_closed_pair": false
    }));
    assert!(
        armed["value"]["repeat"].is_null(),
        "the ASCII press never landed"
    );
    assert_eq!(armed["value"]["space"]["ascii"], 46);

    let space = armed["value"]["space"].clone();
    let converted = decide(json!({
        "character": 32, "preceding": "。", "timestamp_ms": 2_100,
        "editor_generation": 3, "repeat": null, "space": space
    }));
    assert_eq!(converted["value"]["space_ascii"], 46);
    // An auto-closed pair never arms: the caret sits between the two marks.
    let paired = arm(json!({
        "ascii": 40, "commit": "（", "timestamp_ms": 2_000,
        "editor_generation": 3, "auto_closed_pair": true
    }));
    assert!(paired["value"]["space"].is_null());

    // Malformed buffers are refused, not dereferenced.
    assert_eq!(
        read(unsafe { msime_client_smart_punctuation_arm(handle, std::ptr::null(), 8) })["ok"],
        false
    );
    let body = json!({
        "character": 32, "preceding": "ab", "timestamp_ms": 1,
        "editor_generation": 3, "repeat": null, "space": null
    })
    .to_string();
    assert_eq!(
        read(unsafe { msime_client_smart_punctuation_decide(handle, body.as_ptr(), body.len()) })
            ["ok"],
        false,
        "preceding must be a single scalar"
    );
}

#[test]
fn smart_punctuation_gestures_follow_their_own_switches() {
    let dir = tempfile::tempdir().unwrap();
    let arm = |handle: u64, body: serde_json::Value| {
        let text = body.to_string();
        // SAFETY: the buffer outlives the call.
        read(unsafe { msime_client_smart_punctuation_arm(handle, text.as_ptr(), text.len()) })
    };
    let comma = json!({
        "ascii": 44, "commit": ",", "timestamp_ms": 0,
        "editor_generation": 1, "auto_closed_pair": false
    });
    let period = json!({
        "ascii": 46, "commit": "。", "timestamp_ms": 0,
        "editor_generation": 1, "auto_closed_pair": false
    });

    // The shipped defaults are not the same for the two: repeat follows smart punctuation, which is off on Windows and macOS as the source ships it and on elsewhere, while space conversion is off everywhere until asked for, because it rewrites a character the user already watched land.
    let shipped = test_host_preferences(dir.path(), chinese_preferences());
    let repeat_shipped = !cfg!(any(windows, target_os = "macos"));
    assert_eq!(
        arm(shipped, comma.clone())["value"]["repeat"]["committed"] == ",",
        repeat_shipped
    );
    assert!(
        arm(shipped, period.clone())["value"]["space"].is_null(),
        "space conversion is off until it is asked for"
    );

    // Each switch answers only for its own gesture.
    let repeat_off = test_host_preferences(
        dir.path(),
        Preferences {
            smart_punctuation: true,
            smart_punctuation_repeat: false,
            smart_punctuation_space_convert: true,
            ..chinese_preferences()
        },
    );
    assert!(arm(repeat_off, comma.clone())["value"]["repeat"].is_null());
    assert_eq!(
        arm(repeat_off, period.clone())["value"]["space"]["ascii"],
        46
    );

    // Smart punctuation off is the whole family off, whatever the sub-switches say.
    let all_off = test_host_preferences(
        dir.path(),
        Preferences {
            smart_punctuation: false,
            smart_punctuation_repeat: true,
            smart_punctuation_space_convert: true,
            ..chinese_preferences()
        },
    );
    assert!(arm(all_off, comma)["value"]["repeat"].is_null());
    assert!(arm(all_off, period)["value"]["space"].is_null());
}

#[test]
fn shuangpin_key_hint_boundary_publishes_the_engine_face() {
    let hints = |value: &str| {
        // SAFETY: the slice outlives the call.
        read(unsafe { msime_client_shuangpin_key_hints(value.as_ptr(), value.len()) })
    };

    let xiaohe = hints("xiaohe");
    assert_eq!(xiaohe["ok"], true);
    // Both units the key carries, not just the first one.
    assert_eq!(xiaohe["value"]["K"], "ing uai");
    // Initials and finals stay on their own side of the separator.
    assert_eq!(xiaohe["value"]["V"], "zh / ui ü");
    assert!(xiaohe["value"].as_object().unwrap().len() >= 26);

    // Each profile answers for itself rather than for the Engine's default.
    assert_ne!(hints("microsoft")["value"], xiaohe["value"]);
    assert_eq!(hints("microsoft")["value"][";"], "ing");

    // An unknown name yields nothing instead of mislabelling the keys.
    for unknown in ["", "quanpin", "xiaohe-v2"] {
        assert_eq!(
            hints(unknown)["value"].as_object().unwrap().len(),
            0,
            "{unknown:?} produced hints"
        );
    }

    // A null buffer and an oversized length are refused, not dereferenced.
    assert_eq!(
        read(unsafe { msime_client_shuangpin_key_hints(std::ptr::null(), 6) })["ok"],
        false
    );
    let value = "xiaohe";
    assert_eq!(
        read(unsafe { msime_client_shuangpin_key_hints(value.as_ptr(), 4096) })["ok"],
        false
    );
}

#[test]
fn host_capability_boundary_describes_each_platform() {
    let capabilities = |value: &str| {
        // SAFETY: the slice outlives the call.
        read(unsafe { msime_client_host_capabilities(value.as_ptr(), value.len()) })
    };

    let linux = capabilities("linux");
    assert_eq!(linux["ok"], true);
    assert_eq!(linux["value"]["platform"], "linux");
    assert_eq!(linux["value"]["restart_input_method"], true);
    assert_eq!(linux["value"]["ime_mode_scope"], true);
    // Every host reads capabilities through this boundary, so the border colour reaches the Linux page here.
    assert_eq!(linux["value"]["candidate_border_color"], true);
    assert_eq!(linux["value"]["candidate_selection_appearance"], false);

    let windows = capabilities("windows");
    assert_eq!(windows["value"]["restart_input_method"], true);
    assert_eq!(windows["value"]["panel_windows"], true);
    // Typing statistics used to be gated on an Android user-agent match.
    assert_eq!(windows["value"]["typing_statistics"], true);

    let macos = capabilities("macos");
    assert_eq!(macos["value"]["restart_input_method"], true);
    assert_eq!(macos["value"]["candidate_follow_cursor"], true);

    let android = capabilities("android");
    assert_eq!(android["value"]["panel_windows"], false);
    assert_eq!(android["value"]["window_chrome"], false);

    assert_eq!(capabilities("bsd")["ok"], false);
    assert_eq!(capabilities("")["ok"], false);
    assert_eq!(
        read(unsafe { msime_client_host_capabilities(std::ptr::null(), 5) })["ok"],
        false
    );
}

#[test]
fn traditional_conversion_boundary_returns_text_or_null() {
    let convert = |bytes: &[u8]| {
        // SAFETY: the slice outlives the call.
        let raw = unsafe { msime_client_simplified_to_traditional(bytes.as_ptr(), bytes.len()) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: a non-null result is an owned NUL-terminated string from this library.
        let text = unsafe { std::ffi::CStr::from_ptr(raw) }
            .to_str()
            .unwrap()
            .to_owned();
        // SAFETY: released exactly once.
        unsafe { msime_client_string_free(raw) };
        Some(text)
    };
    assert_eq!(convert("头发".as_bytes()).as_deref(), Some("頭髮"));
    assert_eq!(convert(b"").as_deref(), Some(""));
    assert_eq!(convert(b"\xff\xfe"), None);
    assert_eq!(convert(b"a\0b"), None);
    // SAFETY: null is part of the documented contract.
    assert!(unsafe { msime_client_simplified_to_traditional(std::ptr::null(), 4) }.is_null());
    // A native length is bounded before the borrowed slice is formed, so a malformed or stale
    // length cannot make the ABI scan beyond the caller's intended text buffer.
    let fixture = b"fixture";
    assert!(
        unsafe { msime_client_simplified_to_traditional(fixture.as_ptr(), (1 << 20) + 1) }
            .is_null()
    );
}

#[test]
fn abi_version_reports_the_surface_route_revision() {
    assert_eq!(msime_client_abi_version(), 2);
}
#[test]
fn native_preference_save_clears_history_only_after_successful_disable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    let store = PreferencesStore::new(directory.path());
    let mut enabled = store
        .save(
            0,
            Preferences {
                clipboard_history: true,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert!(store
        .capture_clipboard_text("synthetic saved history".into())
        .unwrap());
    let history = directory.path().join("clipboard_history.json");
    let original = std::fs::read(&history).unwrap();
    let save = |revision, snapshot: &PreferencesSnapshot| {
        let bytes = serde_json::to_vec(snapshot).unwrap();
        read(unsafe {
            msime_client_save_preferences(
                path.as_ptr(),
                path.len(),
                revision,
                bytes.as_ptr(),
                bytes.len(),
            )
        })
    };
    enabled.preferences.clipboard_history = false;
    assert_eq!(save(0, &enabled)["ok"], false);
    assert_eq!(std::fs::read(&history).unwrap(), original);
    let result = save(enabled.revision, &enabled);
    assert_eq!(result["ok"], true);
    assert_eq!(result["value"]["revision"], enabled.revision + 1);
    assert!(
        !history.exists(),
        "successful native disable retained clipboard history"
    );
    assert!(!store
        .capture_clipboard_text("synthetic stopped capture".into())
        .unwrap());
    let mut restored = store.load().unwrap();
    restored.preferences.clipboard_history = true;
    assert_eq!(save(restored.revision, &restored)["ok"], true);
    assert!(store
        .capture_clipboard_text("synthetic new capture".into())
        .unwrap());
    let before = std::fs::read(&history).unwrap();
    let current = store.load().unwrap();
    assert_eq!(save(current.revision, &current)["ok"], true);
    assert_eq!(std::fs::read(&history).unwrap(), before);
    std::fs::remove_file(&history).unwrap();
    std::fs::create_dir(&history).unwrap();
    let mut disabled = store.load().unwrap();
    disabled.preferences.clipboard_history = false;
    assert_eq!(save(disabled.revision, &disabled)["ok"], false);
    assert!(!store.load().unwrap().preferences.clipboard_history);
    assert_eq!(store.load().unwrap().revision, disabled.revision + 1);
    assert!(history.is_dir());
}

#[test]
fn mixed_input_changes_defer_until_composition_ends() {
    use msime_client_core::preferences::MixedInputPreferences;
    let dir = tempfile::tempdir().unwrap();
    // Start with every mixed-input switch opposite to the update below, whatever the platform's factory default is, so the deferral is observable for each one.
    let mut initial = chinese_preferences();
    initial.mixed_input.emoji = false;
    initial.mixed_input.kaomoji = false;
    let handle = test_host_preferences(dir.path(), initial);
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'U', true));
    let before = read(msime_client_view(handle));
    let mut preferences = Preferences {
        mixed_input: MixedInputPreferences {
            english: false,
            minimum_prefix: 8,
            emoji: true,
            kaomoji: true,
        },
        ..Preferences::default()
    };
    assert_eq!(update(handle, 1, &preferences)["value"]["deferred"], true);
    assert_eq!(read(msime_client_view(handle)), before);
    SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        let options = &sessions[&handle].options;
        assert!(options.mixed_english);
        assert!(!options.mixed_emoji);
        assert!(!options.mixed_kaomoji);
    });
    read(msime_client_command(handle, 3));
    SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        let options = &sessions[&handle].options;
        assert!(!options.mixed_english);
        assert_eq!(options.english_minimum_prefix, 8);
        assert!(options.mixed_emoji && options.mixed_kaomoji);
    });
    preferences.mixed_input.minimum_prefix = 9;
    assert_eq!(update(handle, 2, &preferences)["ok"], false);
    read(msime_client_destroy(handle));
}
#[test]
fn frequency_changes_wait_for_composition_and_reject_invalid_updates() {
    use msime_client_core::preferences::{FrequencyMode, FrequencyPreferences};
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'U', true));
    let before = read(msime_client_view(handle));
    let mut preferences = Preferences {
        frequency: FrequencyPreferences {
            mode: FrequencyMode::Linear,
            trigger_count: 3,
            linear_step: 2,
        },
        ..Preferences::default()
    };
    assert_eq!(update(handle, 1, &preferences)["value"]["deferred"], true);
    assert_eq!(read(msime_client_view(handle)), before);
    SESSIONS
        .with(|sessions| assert_eq!(sessions.borrow()[&handle].options.frequency_mode, "promote"));
    read(msime_client_command(handle, 3));
    SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        assert_eq!(sessions[&handle].options.frequency_mode, "linear");
        assert_eq!(sessions[&handle].options.frequency_trigger_count, 3);
        assert_eq!(sessions[&handle].options.frequency_linear_step, 2);
    });
    preferences.frequency.trigger_count = 0;
    assert_eq!(update(handle, 2, &preferences)["ok"], false);
    read(msime_client_destroy(handle));
}

#[test]
fn fuzzy_pinyin_changes_wait_for_composition_and_disabled_rules_are_retained() {
    use msime_client_core::preferences::{FuzzyPinyinPreferences, FuzzyPinyinRule};
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'z', false));
    let mut preferences = Preferences {
        fuzzy_pinyin: FuzzyPinyinPreferences {
            enabled: true,
            rules: [FuzzyPinyinRule::ZZh].into_iter().collect(),
            seeded: false,
        },
        ..Preferences::default()
    };
    let queued = update(handle, 1, &preferences);
    assert_eq!(queued["value"]["deferred"], true);
    SESSIONS.with(|sessions| assert_eq!(sessions.borrow()[&handle].options.fuzzy_pinyin_rules, 0));
    read(msime_client_command(handle, 3));
    SESSIONS.with(|sessions| assert_eq!(sessions.borrow()[&handle].options.fuzzy_pinyin_rules, 1));

    preferences.fuzzy_pinyin.enabled = false;
    let disabled = update(handle, 2, &preferences);
    assert_eq!(disabled["value"]["deferred"], false);
    SESSIONS.with(|sessions| {
        let session = &sessions.borrow()[&handle];
        assert_eq!(session.options.fuzzy_pinyin_rules, 0);
        assert!(!session.applied.fuzzy_pinyin.enabled);
        assert!(session
            .applied
            .fuzzy_pinyin
            .rules
            .contains(&FuzzyPinyinRule::ZZh));
    });
    read(msime_client_destroy(handle));
}

#[test]
fn wubi_mixed_pinyin_reaches_engine_and_applies_after_composition() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'a', false));
    let mut preferences = Preferences {
        scheme: InputScheme::Wubi,
        wubi_mixed_pinyin: true,
        ..Preferences::default()
    };
    let queued = update(handle, 1, &preferences);
    assert_eq!(queued["value"]["deferred"], true);
    SESSIONS.with(|sessions| {
        assert!(!sessions.borrow()[&handle].options.wubi_mixed_pinyin);
    });

    read(msime_client_command(handle, 3));
    SESSIONS.with(|sessions| {
        let session = &sessions.borrow()[&handle];
        assert!(session.options.wubi_mixed_pinyin);
        assert!(session.applied.wubi_mixed_pinyin);
    });

    preferences.wubi_mixed_pinyin = false;
    let disabled = update(handle, 2, &preferences);
    assert_eq!(disabled["value"]["deferred"], false);
    SESSIONS.with(|sessions| {
        assert!(!sessions.borrow()[&handle].options.wubi_mixed_pinyin);
    });
    read(msime_client_destroy(handle));
}

#[test]
fn japanese_mode_switch_defers_and_restores_chinese_profile() {
    use msime_client_core::preferences::ChineseScheme;
    let dir = tempfile::tempdir().unwrap();
    let chinese = Preferences {
        scheme: InputScheme::Shuangpin,
        shuangpin_profile: ShuangpinProfile::Microsoft,
        last_chinese_scheme: Some(ChineseScheme::Shuangpin),
        ..chinese_preferences()
    };
    let handle = test_host_preferences(dir.path(), chinese.clone());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'b', false));
    read(msime_client_character(handle, b';', false));
    let japanese = Preferences {
        scheme: InputScheme::Japanese,
        touch_keyboard_layout: TouchKeyboardLayout::NineKey,
        ..chinese.clone()
    };
    let queued = update(handle, 1, &japanese);
    assert_eq!(queued["value"]["deferred"], true);
    assert_eq!(
        queued["value"]["view"]["touch_keyboard_layout"],
        "twenty_six_key"
    );
    let committed = read(msime_client_command(handle, 2));
    assert_eq!(committed["value"]["commit"], "b;");
    assert_eq!(committed["value"]["commit_context"]["scheme"], 1);
    assert_eq!(committed["value"]["view"]["scheme"], 3);
    assert_eq!(committed["value"]["view"]["nine_key"], false);
    assert_eq!(
        committed["value"]["view"]["touch_keyboard_layout"],
        "nine_key"
    );
    let kana = read(msime_client_character(handle, b'a', false));
    assert_eq!(kana["ok"], true);
    assert_eq!(kana["value"]["view"]["preedit"], "a");
    assert_eq!(kana["value"]["view"]["reading"], "あ");
    assert_eq!(kana["value"]["view"]["scheme"], 3);
    assert_eq!(kana["value"]["view"]["candidates"][0]["text"], "あ");
    assert_eq!(kana["value"]["view"]["candidates"][1]["text"], "ア");
    let small_kana = read(msime_client_command(handle, 10));
    assert_eq!(small_kana["value"]["handled"], true);
    assert_eq!(small_kana["value"]["view"]["reading"], "ぁ");
    let committed_small_kana = read(msime_client_command(handle, 11));
    assert_eq!(committed_small_kana["value"]["commit"], "ぁ");
    assert_eq!(committed_small_kana["value"]["view"]["reading"], "");
    read(msime_client_character(handle, b'a', false));
    let committed_kana = read(msime_client_command(handle, 11));
    assert_eq!(committed_kana["value"]["commit"], "あ");
    assert_eq!(committed_kana["value"]["view"]["reading"], "");
    read(msime_client_command(handle, 3));
    read(msime_client_character(handle, b'n', false));
    let syllable_separator = read(msime_client_character(handle, b'\'', false));
    assert_eq!(syllable_separator["ok"], true);
    assert_eq!(
        syllable_separator["value"]["view"]["candidates"][0]["text"],
        "ん"
    );
    assert_eq!(update(handle, 2, &chinese)["value"]["deferred"], true);
    read(msime_client_command(handle, 3));
    assert_eq!(
        read(msime_client_view(handle))["value"]["touch_keyboard_layout"],
        "twenty_six_key"
    );
    read(msime_client_character(handle, b'b', false));
    assert_eq!(
        read(msime_client_character(handle, b';', false))["value"]["view"]["editing_text"],
        "b;"
    );
    read(msime_client_destroy(handle));
}

#[test]
fn raw_commit_without_learning_crosses_the_host_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    for character in b"xq" {
        read(msime_client_character(handle, *character, false));
    }
    let committed = read(msime_client_command(handle, 15));
    assert_eq!(committed["ok"], true);
    assert_eq!(committed["value"]["commit"], "xq");
    assert_eq!(committed["value"]["view"]["editing_text"], "");
    read(msime_client_destroy(handle));
}

#[test]
fn japanese_commands_are_unhandled_for_non_japanese_schemes() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    let typed = read(msime_client_character(handle, b'a', false));
    assert_eq!(typed["value"]["view"]["reading"], "");

    let variant = read(msime_client_command(handle, 10));
    assert_eq!(variant["value"]["handled"], false);
    assert_eq!(variant["value"]["view"]["editing_text"], "a");
    assert_eq!(variant["value"]["view"]["reading"], "");

    let reading = read(msime_client_command(handle, 11));
    assert_eq!(reading["value"]["handled"], false);
    assert!(reading["value"]["commit"].is_null());
    assert_eq!(reading["value"]["view"]["reading"], "");
    read(msime_client_destroy(handle));
}

#[test]
fn japanese_commands_apply_to_the_twenty_six_key_scheme() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host_preferences(
        dir.path(),
        Preferences {
            scheme: InputScheme::Japanese,
            touch_keyboard_layout: TouchKeyboardLayout::TwentySixKey,
            ..chinese_preferences()
        },
    );
    read(msime_client_focus(handle, true));
    let typed = read(msime_client_character(handle, b'a', false));
    assert_eq!(
        typed["value"]["view"]["touch_keyboard_layout"],
        "twenty_six_key"
    );
    assert_eq!(typed["value"]["view"]["reading"], "あ");

    let variant = read(msime_client_command(handle, 10));
    assert_eq!(variant["value"]["handled"], true);
    assert_eq!(variant["value"]["view"]["reading"], "ぁ");
    let committed = read(msime_client_command(handle, 11));
    assert_eq!(committed["value"]["commit"], "ぁ");
    assert_eq!(committed["value"]["view"]["reading"], "");
    read(msime_client_destroy(handle));
}

#[test]
fn handwriting_layout_is_exposed_only_after_pending_composition_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'n', false));
    read(msime_client_character(handle, b'i', false));
    let handwriting = Preferences {
        touch_keyboard_layout: TouchKeyboardLayout::Handwriting,
        ..Preferences::default()
    };
    let queued = update(handle, 1, &handwriting);
    assert_eq!(queued["value"]["deferred"], true);
    assert_eq!(
        queued["value"]["view"]["touch_keyboard_layout"],
        "twenty_six_key"
    );
    let finished = read(msime_client_command(handle, 9));
    assert_eq!(finished["value"]["commit"], "ni");
    assert_eq!(
        finished["value"]["view"]["touch_keyboard_layout"],
        "handwriting"
    );
    assert_eq!(finished["value"]["view"]["nine_key"], false);
    read(msime_client_destroy(handle));
}

#[test]
fn nine_key_mode_and_spelling_identity_cross_the_host_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    let enabled = read(msime_client_set_nine_key_mode(handle, true));
    assert_eq!(enabled["value"]["nine_key"], true);
    let typed = read(msime_client_character(handle, b'6', false));
    assert_eq!(typed["value"]["handled"], true);
    let view = &typed["value"]["view"];
    let generation = view["generation"].as_u64().unwrap();
    assert!(!view["nine_key_spellings"].as_array().unwrap().is_empty());
    assert_eq!(
        read(msime_client_choose_nine_key_spelling(
            handle,
            generation - 1,
            0
        ))["ok"],
        false
    );
    let selected = read(msime_client_choose_nine_key_spelling(handle, generation, 0));
    assert_eq!(selected["value"]["handled"], true);
    assert_eq!(
        read(msime_client_set_nine_key_mode(handle, false))["ok"],
        false
    );
    read(msime_client_command(handle, 3));
    assert_eq!(
        read(msime_client_set_nine_key_mode(handle, false))["value"]["nine_key"],
        false
    );

    let mut preferences = Preferences {
        candidate_page_size: 4,
        touch_keyboard_layout: TouchKeyboardLayout::NineKey,
        ..Preferences::default()
    };
    assert_eq!(
        update(handle, 1, &preferences)["value"]["view"]["nine_key"],
        true
    );
    preferences.learning = false;
    assert_eq!(
        update(handle, 2, &preferences)["value"]["view"]["nine_key"],
        true
    );
    preferences.touch_keyboard_layout = TouchKeyboardLayout::TwentySixKey;
    assert_eq!(
        update(handle, 3, &preferences)["value"]["view"]["nine_key"],
        false
    );
    preferences.touch_keyboard_layout = TouchKeyboardLayout::Handwriting;
    assert_eq!(
        update(handle, 4, &preferences)["value"]["view"]["touch_keyboard_layout"],
        "handwriting"
    );
    assert_eq!(read(msime_client_view(handle))["value"]["nine_key"], false);
    assert_eq!(
        read(msime_client_set_nine_key_mode(handle, true))["value"]["nine_key"],
        true
    );
    preferences.candidate_page_size = 3;
    assert_eq!(
        update(handle, 5, &preferences)["value"]["view"]["nine_key"],
        true
    );
    preferences.scheme = InputScheme::Wubi;
    assert_eq!(
        update(handle, 6, &preferences)["value"]["view"]["nine_key"],
        false
    );
    assert_eq!(
        read(msime_client_set_nine_key_mode(handle, true))["ok"],
        false
    );
    read(msime_client_destroy(handle));

    let persisted_dir = tempfile::tempdir().unwrap();
    let persisted = test_host_preferences(
        persisted_dir.path(),
        Preferences {
            touch_keyboard_layout: TouchKeyboardLayout::NineKey,
            ..Preferences::default()
        },
    );
    assert_eq!(
        read(msime_client_view(persisted))["value"]["nine_key"],
        true
    );
    read(msime_client_destroy(persisted));
}

#[test]
fn nine_key_digits_offer_ranked_english_across_the_host_boundary() {
    use msime_client_core::preferences::MixedInputPreferences;

    let dir = tempfile::tempdir().unwrap();
    let dictionaries = dir.path().join("dictionaries");
    std::fs::create_dir_all(&dictionaries).unwrap();
    let db = rusqlite::Connection::open(dictionaries.join("english.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);
         INSERT INTO english_words VALUES('ok','ok',900);
         INSERT INTO english_words VALUES('old','old',1000);
         INSERT INTO english_words VALUES('older','older',800);",
    )
    .unwrap();
    drop(db);

    let handle = test_host_preferences(
        dir.path(),
        Preferences {
            candidate_page_size: 2,
            learning: false,
            touch_keyboard_layout: TouchKeyboardLayout::NineKey,
            mixed_input: MixedInputPreferences {
                english: true,
                minimum_prefix: 2,
                emoji: false,
                kaomoji: false,
            },
            ..chinese_preferences()
        },
    );
    read(msime_client_focus(handle, true));
    assert_eq!(
        read(msime_client_character(handle, b'6', false))["value"]["handled"],
        true
    );
    let typed = read(msime_client_character(handle, b'5', false));
    assert_eq!(typed["value"]["view"]["nine_key"], true);

    let complete = read(msime_client_all_candidates(handle));
    let candidates = complete["value"]["candidates"].as_array().unwrap();
    let position = |text: &str| {
        candidates
            .iter()
            .position(|candidate| candidate["text"] == text)
            .unwrap_or_else(|| panic!("missing synthetic candidate {text}"))
    };
    let ok = position("ok");
    assert!(ok < position("old"));
    assert!(ok < position("older"));

    let generation = complete["value"]["generation"].as_u64().unwrap();
    let index = candidates[ok]["id"]["index"].as_u64().unwrap() as usize;
    let selected = read(msime_client_select_any_candidate(handle, generation, index));
    assert_eq!(selected["value"]["handled"], true);
    assert_eq!(selected["value"]["commit"], "ok");
    assert_eq!(read(msime_client_destroy(handle))["ok"], true);
}

#[test]
fn helpcode_defaults_follow_windows_for_each_pinyin_scheme() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    SESSIONS.with(|sessions| {
        let session = &sessions.borrow()[&handle];
        assert!(session.options.helpcode);
        assert_eq!(session.options.helpcode_schema, "ziranma");
        assert!(!session.options.show_helpcode);
    });

    let shuangpin = Preferences {
        scheme: InputScheme::Shuangpin,
        ..chinese_preferences()
    };
    assert_eq!(update(handle, 1, &shuangpin)["value"]["deferred"], false);
    SESSIONS.with(|sessions| {
        let session = &sessions.borrow()[&handle];
        assert!(session.options.helpcode);
        assert_eq!(session.options.helpcode_schema, "lantian");
        assert!(session.options.show_helpcode);
    });
    read(msime_client_destroy(handle));
}

#[test]
fn english_completion_boundary_is_read_only_and_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    let dictionaries = dir.path().join("dictionaries");
    std::fs::create_dir_all(&dictionaries).unwrap();
    let db = rusqlite::Connection::open(dictionaries.join("english.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE english_words(word TEXT,display TEXT,weight INTEGER);
         INSERT INTO english_words VALUES('hello','hello',1000);
         INSERT INTO english_words VALUES('help','help',900);
         INSERT INTO english_words VALUES('helium','helium',800);",
    )
    .unwrap();
    drop(db);
    let handle = test_host(dir.path());
    let before = read(msime_client_view(handle))["value"].clone();
    let prefix = b"He";
    let result =
        read(unsafe { msime_client_english_completions(handle, prefix.as_ptr(), prefix.len(), 2) });
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["value"]["completions"], json!(["hello", "help"]));
    assert_eq!(read(msime_client_view(handle))["value"], before);
    let invalid = read(unsafe { msime_client_english_completions(handle, b"he!".as_ptr(), 3, 2) });
    assert_eq!(invalid["ok"], false);
    read(msime_client_destroy(handle));
}

#[test]
fn helpcode_settings_switch_independently_after_composition() {
    use msime_client_core::preferences::{HelpcodePreferences, HelpcodeSchema};
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'U', true));
    let before = read(msime_client_view(handle))["value"].clone();
    let mut preferences = Preferences {
        quanpin_helpcode: HelpcodePreferences {
            enabled: false,
            schema: HelpcodeSchema::Xiaohe,
            show_in_candidate_window: false,
        },
        shuangpin_helpcode: HelpcodePreferences {
            enabled: true,
            schema: HelpcodeSchema::Shouyou2,
            show_in_candidate_window: true,
        },
        ..Preferences::default()
    };
    assert_eq!(update(handle, 1, &preferences)["value"]["deferred"], true);
    assert_eq!(read(msime_client_view(handle))["value"], before);
    SESSIONS.with(|sessions| assert!(sessions.borrow()[&handle].options.helpcode));
    read(msime_client_command(handle, 3));
    SESSIONS.with(|sessions| {
        assert!(!sessions.borrow()[&handle].options.helpcode);
        assert_eq!(sessions.borrow()[&handle].options.helpcode_schema, "xiaohe");
        assert!(!sessions.borrow()[&handle].options.show_helpcode);
    });
    preferences.scheme = InputScheme::Shuangpin;
    assert_eq!(update(handle, 2, &preferences)["value"]["deferred"], false);
    SESSIONS.with(|sessions| {
        assert!(sessions.borrow()[&handle].options.helpcode);
        assert!(sessions.borrow()[&handle].options.show_helpcode);
        assert_eq!(
            sessions.borrow()[&handle].options.helpcode_schema,
            "shouyou2_0"
        );
    });
    preferences.scheme = InputScheme::Quanpin;
    update(handle, 3, &preferences);
    SESSIONS.with(|sessions| assert!(!sessions.borrow()[&handle].options.helpcode));
    read(msime_client_destroy(handle));
}

#[test]
fn autocorrect_update_waits_for_composition_end() {
    let dir = tempfile::tempdir().unwrap();
    let disabled_preferences = Preferences {
        quanpin: msime_client_core::preferences::QuanpinPreferences {
            autocorrect_transposition: Some(false),
            autocorrect_neighbor: Some(false),
        },
        ..chinese_preferences()
    };
    let handle = test_host_preferences(dir.path(), disabled_preferences.clone());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'U', true));
    let before = read(msime_client_view(handle))["value"].clone();
    let preferences = Preferences {
        quanpin: msime_client_core::preferences::QuanpinPreferences {
            autocorrect_transposition: Some(true),
            autocorrect_neighbor: Some(true),
        },
        ..Preferences::default()
    };
    assert_eq!(update(handle, 1, &preferences)["value"]["deferred"], true);
    assert_eq!(read(msime_client_view(handle))["value"], before);
    SESSIONS.with(|sessions| {
        assert!(!sessions.borrow()[&handle].options.autocorrect_transposition);
        assert!(!sessions.borrow()[&handle].options.autocorrect_neighbor);
    });
    read(msime_client_command(handle, 3));
    SESSIONS.with(|sessions| {
        assert!(sessions.borrow()[&handle].options.autocorrect_transposition);
        assert!(sessions.borrow()[&handle].options.autocorrect_neighbor);
    });
    assert_eq!(
        update(handle, 2, &disabled_preferences)["value"]["deferred"],
        false
    );
    SESSIONS.with(|sessions| {
        assert!(!sessions.borrow()[&handle].options.autocorrect_transposition);
        assert!(!sessions.borrow()[&handle].options.autocorrect_neighbor);
    });
    read(msime_client_destroy(handle));
}

#[test]
#[cfg(not(target_os = "android"))]
fn skin_catalog_reaches_native_presenters_without_the_settings_shell() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("skins");
    let scan = |path: &str| read(unsafe { msime_client_skin_catalog(path.as_ptr(), path.len()) });
    let path = root.to_str().unwrap().to_owned();
    // An absent root is an empty catalog, not a failure the presenter shows.
    assert_eq!(
        scan(&path),
        json!({"ok": true, "value": {"packages": [], "issues": []}})
    );
    std::fs::create_dir_all(root.join("sample")).unwrap();
    std::fs::write(
        root.join("sample/skin.toml"),
        "schema_version = 1\nid = 'sample'\nname = 'Sample'\nversion = '1.0'\n\
         base = 'fluent'\n[supports]\nlayouts = ['vertical']\nthemes = ['light']\n\
         [candidate_window]\nmin_width_dip = 10\n[candidate_window.decoration]\n\
         top_inset_dip = 0\nwidth_dip = 0\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("broken")).unwrap();
    std::fs::write(root.join("broken/skin.toml"), "not a manifest").unwrap();
    let catalog = scan(&path);
    assert_eq!(catalog["ok"], true);
    assert_eq!(catalog["value"]["packages"][0]["id"], "sample");
    assert_eq!(catalog["value"]["packages"][0]["minWidthDip"], 10.0);
    assert_eq!(catalog["value"]["packages"][0]["layouts"][0], "vertical");
    assert_eq!(catalog["value"]["packages"].as_array().unwrap().len(), 1);
    // A package that fails validation is reported, never offered for rendering.
    assert_eq!(catalog["value"]["issues"][0]["folder"], "broken");
    assert_eq!(
        catalog["value"],
        serde_json::to_value(msime_client_core::skin::catalog::scan(&root)).unwrap()
    );
    let relative = "skins";
    assert_eq!(scan(relative)["ok"], false);
    assert_eq!(
        read(unsafe { msime_client_skin_catalog(std::ptr::null(), 0) })["ok"],
        false
    );
}

#[test]
fn a_picked_skin_folder_is_copied_in_through_the_c_abi() {
    let files = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let root = state.path().join("skins");
    let source = files.path().join("sakura");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("skin.toml"), "id = 'sakura'").unwrap();
    let call = |request: String| {
        read(unsafe { msime_client_skin_import(request.as_ptr(), request.len()) })
    };
    let imported = call(json!({"source": source, "directory": root}).to_string());
    assert_eq!(imported, json!({"ok": true, "value": {"id": "sakura"}}));
    assert!(root.join("sakura/skin.toml").is_file());
    let bare = files.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    assert_eq!(
        call(json!({"source": bare, "directory": root}).to_string())["error"],
        "skin_manifest"
    );
    assert_eq!(
        call(json!({"source": "sakura", "directory": root}).to_string())["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_skin_import(std::ptr::null(), 0) })["ok"],
        false
    );
}

#[test]
#[cfg(not(target_os = "android"))]
fn skin_package_resolves_one_manifest_with_the_catalog_loader() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("skins");
    let call =
        |request: &str| read(unsafe { msime_client_skin_package(request.as_ptr(), request.len()) });
    let request = |id: &str| json!({"directory": root, "id": id}).to_string();
    assert_eq!(
        read(unsafe { msime_client_skin_package(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(call("not json")["ok"], false);
    assert_eq!(
        call(&json!({"directory": "skins", "id": "sample"}).to_string())["ok"],
        false
    );
    assert_eq!(call(&request("sample"))["ok"], false);
    std::fs::create_dir_all(root.join("sample")).unwrap();
    // Literal strings, a multi-line array and an inline table: full TOML, as the settings page and Windows toml++ read it.
    std::fs::write(
        root.join("sample/skin.toml"),
        "schema_version = 1\nid = 'sample'\nname = 'Sample'\nversion = '1.0'\n\
         base = 'fluent'\n[supports]\nlayouts = [\n  'vertical',\n]\nthemes = ['light']\n\
         [candidate_window]\nmin_width_dip = 1_0\n\
         decoration = { top_inset_dip = 0, width_dip = 0 }\n[candidate.light]\naccent = '#123456'\n",
    )
    .unwrap();
    let package = call(&request("sample"));
    assert_eq!(package["ok"], true, "{package}");
    assert_eq!(package["value"]["minWidthDip"], 10.0);
    assert_eq!(package["value"]["candidate"]["light"]["accent"], "#123456");
    let catalog = read(unsafe {
        let path = root.to_str().unwrap();
        msime_client_skin_catalog(path.as_ptr(), path.len())
    });
    assert_eq!(package["value"], catalog["value"]["packages"][0]);
    assert_eq!(call(&request("fluent"))["error"], "invalid skin id");
    std::fs::write(root.join("sample/skin.toml"), "schema_version = '1'\n").unwrap();
    assert_eq!(
        call(&request("sample"))["error"],
        "unsupported schema_version"
    );
}

#[test]
#[cfg(not(target_os = "android"))]
fn custom_skin_library_reaches_a_c_abi_host_without_a_second_store() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_str().unwrap().to_owned();
    let call = |request: String| {
        read(unsafe { msime_client_custom_skin_library(request.as_ptr(), request.len()) })
    };
    let read_library = || call(json!({"directory": root}).to_string());
    // An untouched library is an empty list rather than a failure to show.
    assert_eq!(read_library(), json!({"ok": true, "value": []}));

    let design =
        serde_json::to_value(msime_client_core::preferences::TouchKeyboardSkinDesign::default())
            .unwrap();
    let created = call(
        json!({
            "directory": root,
            "action": {"operation": "create", "name": "晨雾", "design": design},
        })
        .to_string(),
    );
    assert_eq!(created["ok"], true);
    assert_eq!(created["value"][0]["name"], "晨雾");
    // A mutation answers with the whole library, so the page redraws from one reply.
    assert_eq!(created["value"], read_library()["value"]);
    let id = created["value"][0]["id"].as_str().unwrap().to_owned();

    // The codes are the ones the shared community pages have wording for; a host forwarding the
    // Display text would put an English log sentence into a Chinese dialog.
    let duplicate = call(
        json!({
            "directory": root,
            "action": {"operation": "create", "name": "晨雾", "design": design},
        })
        .to_string(),
    );
    assert_eq!(duplicate["ok"], false);
    assert_eq!(duplicate["error"], "community_skin_duplicate_name");
    let missing = call(
        json!({
            "directory": root,
            "action": {"operation": "delete", "id": "10000000-0000-4000-8000-000000000009"},
        })
        .to_string(),
    );
    assert_eq!(missing["error"], "community_not_found");
    let blank = call(
        json!({
            "directory": root,
            "action": {"operation": "rename", "id": id, "name": "   "},
        })
        .to_string(),
    );
    assert_eq!(blank["error"], "community_skin_invalid_name");

    let renamed = call(
        json!({
            "directory": root,
            "action": {"operation": "rename", "id": id, "name": "竹影"},
        })
        .to_string(),
    );
    assert_eq!(renamed["value"][0]["name"], "竹影");
    assert_eq!(
        call(json!({"directory": root, "action": {"operation": "delete", "id": id}}).to_string()),
        json!({"ok": true, "value": []})
    );

    // A relative root and a null buffer are refused rather than resolved against whatever the
    // process happens to have as its working directory.
    assert_eq!(call(json!({"directory": "skins"}).to_string())["ok"], false);
    assert_eq!(
        read(unsafe { msime_client_custom_skin_library(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(call("not json".to_owned())["error"], "community_invalid");
}

#[test]
#[cfg(not(target_os = "android"))]
fn installing_a_community_skin_is_one_step_so_a_failed_import_ends_its_trial() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_str().unwrap().to_owned();
    let install = |request: String| {
        read(unsafe { msime_client_community_skin_install(request.as_ptr(), request.len()) })
    };
    let trial = |request: String| {
        read(unsafe { msime_client_keyboard_skin_trial(request.as_ptr(), request.len()) })
    };
    let preferences = msime_client_core::preferences::PreferencesStore::new(directory.path());
    let design = serde_json::to_value(msime_client_core::preferences::TouchKeyboardSkinDesign {
        background: 0x102030,
        ..Default::default()
    })
    .unwrap();
    let id = "10000000-0000-4000-8000-000000000001";

    let installed =
        install(json!({"directory": root, "id": id, "name": "晨雾", "design": design}).to_string());
    assert_eq!(installed["ok"], true);
    assert_eq!(installed["value"]["skin"]["id"], id);
    assert_eq!(installed["value"]["skin"]["name"], "晨雾");
    assert_eq!(installed["value"]["trial"]["name"], "晨雾");
    // The design is on the keyboard, not merely in the library: a gallery that saved a skin
    // without wearing it would make "试用" mean nothing.
    let applied = preferences.load().unwrap();
    assert_eq!(
        applied.preferences.touch_keyboard_skin,
        msime_client_core::preferences::TouchKeyboardSkin::Custom
    );
    assert_eq!(
        applied.preferences.custom_touch_keyboard_skin.background,
        0x102030
    );

    // Declining puts the previous skin back, which is the whole reason the trial exists.
    let trial_id = installed["value"]["trial"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let restored = trial(
        json!({
            "directory": root,
            "action": {"operation": "finish", "id": trial_id, "keep": false},
        })
        .to_string(),
    );
    assert_eq!(restored["ok"], true);
    let reverted = preferences.load().unwrap();
    assert_ne!(
        reverted.preferences.touch_keyboard_skin,
        msime_client_core::preferences::TouchKeyboardSkin::Custom
    );
    // The library keeps it: declining the trial is declining to wear it now, not to own it.
    let library = json!({"directory": root}).to_string();
    let saved = read(unsafe { msime_client_custom_skin_library(library.as_ptr(), library.len()) });
    assert_eq!(saved["value"][0]["id"], id);

    // Recovery is safe with nothing pending, because that is exactly when it runs: at startup,
    // before anyone knows whether the last session ended mid-trial.
    assert_eq!(
        trial(json!({"directory": root, "action": {"operation": "restore_pending"}}).to_string())
            ["ok"],
        true
    );

    assert_eq!(
        install(
            json!({"directory": root, "id": "not-a-uuid", "name": "x", "design": design})
                .to_string()
        )["error"],
        "community_invalid"
    );
    assert_eq!(
        install(json!({"directory": "skins", "id": id, "name": "x", "design": design}).to_string())
            ["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_community_skin_install(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_keyboard_skin_trial(std::ptr::null(), 0) })["ok"],
        false
    );
}

#[test]
#[cfg(not(target_os = "android"))]
fn the_reply_library_a_keyboard_rereads_is_written_through_its_own_store() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory
        .path()
        .join("CommunityLibrary.json")
        .to_str()
        .unwrap()
        .to_owned();
    let call = |request: String| {
        read(unsafe { msime_client_community_resource_library(request.as_ptr(), request.len()) })
    };
    let template = |id: &str, prompt: &str| {
        json!({
            "id": id,
            "kind": "reply",
            "name": "高情商",
            "description": "",
            "author": "作者",
            "content": {"prompt": prompt},
            "revision": 1,
            "saves": 0,
            "saved": true,
            "owned": false,
            "rating_count": 0,
            "rating_average": 0.0,
            "my_rating": 0,
        })
    };
    let id = "10000000-0000-4000-8000-000000000001";

    // An untouched library is an empty list, which is what a first run looks like.
    assert_eq!(
        call(json!({"file": file, "action": {"operation": "load"}}).to_string()),
        json!({"ok": true, "value": []})
    );
    let saved = call(
        json!({
            "file": file,
            "action": {"operation": "save_reply", "item": template(id, "换个说法")},
        })
        .to_string(),
    );
    assert_eq!(saved["ok"], true);
    assert_eq!(saved["value"][0]["id"], id);
    assert_eq!(saved["value"][0]["content"]["prompt"], "换个说法");

    // Keeping the same template twice replaces it rather than growing the list: the id is the
    // publication, and a second copy would show up twice in the keyboard's menu.
    let replaced = call(
        json!({
            "file": file,
            "action": {"operation": "save_reply", "item": template(id, "更客气一点")},
        })
        .to_string(),
    );
    assert_eq!(replaced["value"].as_array().unwrap().len(), 1);
    assert_eq!(replaced["value"][0]["content"]["prompt"], "更客气一点");

    // A dictionary is not a reply template. The keyboard's parser skips what it does not recognise,
    // so a host that wrote one here would produce a library that silently lost an entry.
    let wrong_kind = json!({
        "file": file,
        "action": {
            "operation": "save_reply",
            "item": {
                "id": "10000000-0000-4000-8000-000000000002",
                "kind": "dictionary",
                "name": "词库",
                "description": "",
                "author": "作者",
                "content": {"entries": [{"kind": "pinyin", "code": "ni", "word": "你", "weight": 1}]},
                "revision": 1,
                "saves": 0,
                "saved": true,
                "owned": false,
                "rating_count": 0,
                "rating_average": 0.0,
                "my_rating": 0,
            },
        },
    });
    assert_eq!(
        call(wrong_kind.to_string())["error"],
        "community_resource_library_format"
    );

    assert_eq!(
        call(json!({"file": file, "action": {"operation": "remove", "id": id}}).to_string()),
        json!({"ok": true, "value": []})
    );
    // Forgetting something that is not there is not a failure; the page may have been showing a
    // library another process already changed.
    assert_eq!(
        call(json!({"file": file, "action": {"operation": "remove", "id": id}}).to_string())["ok"],
        true
    );
    assert_eq!(
        call(json!({"file": file, "action": {"operation": "remove", "id": "nope"}}).to_string())
            ["error"],
        "community_invalid"
    );
    assert_eq!(
        call(json!({"file": "CommunityLibrary.json", "action": {"operation": "load"}}).to_string())
            ["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_community_resource_library(std::ptr::null(), 0) })["ok"],
        false
    );
}

#[test]
#[cfg(not(target_os = "android"))]
fn ai_skin_planning_keeps_the_instruction_and_the_parser_together() {
    let call = |request: String| {
        read(unsafe { msime_client_ai_skin_plan(request.as_ptr(), request.len()) })
    };

    // The host does not write the instruction. It names the exact document the parser accepts, so
    // a host composing its own would be asking for something the parser was not written against.
    let composed = call(
        json!({"operation": "compose", "prompt": "晨雾里的竹林", "model": "fast"}).to_string(),
    );
    assert_eq!(composed["ok"], true);
    assert_eq!(composed["value"]["path"], "/v1/chat/completions");
    assert_eq!(composed["value"]["body"]["model"], "fast");
    assert_eq!(composed["value"]["body"]["stream"], false);
    assert_eq!(composed["value"]["body"]["messages"][0]["role"], "system");
    assert_eq!(
        composed["value"]["body"]["messages"][0]["content"],
        msime_client_core::skin::ai::AI_SKIN_SYSTEM_PROMPT
    );
    assert_eq!(
        composed["value"]["body"]["messages"][1]["content"],
        "晨雾里的竹林"
    );

    // The bounds are the ones `generate` applies before it spends anything: a prompt refused here
    // is one the service would have refused after four requests.
    assert_eq!(
        call(json!({"operation": "compose", "prompt": "", "model": "fast"}).to_string())["error"],
        "ai_skin_invalid"
    );
    assert_eq!(
        call(json!({"operation": "compose", "prompt": "线\u{7}索", "model": "fast"}).to_string())
            ["error"],
        "ai_skin_invalid"
    );
    assert_eq!(
        call(
            json!({"operation": "compose", "prompt": "x".repeat(501), "model": "fast"}).to_string()
        )["error"],
        "ai_skin_invalid"
    );

    let design = |shape: &str, material: &str, accent: &str, prompt: &str| {
        json!({
            "name": "晨雾",
            "description": "竹林里的薄雾",
            "artworkPrompt": prompt,
            "background": "#E8F0EB",
            "keyBackground": "#FFFFFF",
            "keyForeground": "#17251D",
            "accent": accent,
            "actionBackground": accent,
            "gradientEnd": null,
            "gradientHorizontal": false,
            "keyShape": shape,
            "keyMaterial": material,
            "cornerRadius": 8,
            "borderWidth": 0,
            "shadow": 0.1,
            "pattern": 0,
            "monospaced": false,
        })
    };
    let scene = |suffix: &str| {
        format!(
            "{}{suffix}",
            "远山薄雾中的竹林与流水，主体靠近画面边缘，中央留白".repeat(2)
        )
    };
    let answer = json!({
        "skins": [
            design("pebble", "raised", "#185C47", &scene("甲")),
            design("capsule", "glass", "#1C3F6E", &scene("乙")),
            design("ticket", "paper", "#6E2C1C", &scene("丙")),
        ]
    })
    .to_string();
    let parsed = call(json!({"operation": "parse", "text": answer}).to_string());
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["value"].as_array().unwrap().len(), 3);
    assert_eq!(parsed["value"][0]["artworkPrompt"], scene("甲"));
    assert_eq!(parsed["value"][1]["design"]["keyShape"], "capsule");

    // Three proposals that share a shape are not three skins to choose between, which is what the
    // instruction asked the model for.
    let same_shape = json!({
        "skins": [
            design("pebble", "raised", "#185C47", &scene("甲")),
            design("pebble", "glass", "#1C3F6E", &scene("乙")),
            design("ticket", "paper", "#6E2C1C", &scene("丙")),
        ]
    })
    .to_string();
    assert_eq!(
        call(json!({"operation": "parse", "text": same_shape}).to_string())["error"],
        "ai_skin_response"
    );
    assert_eq!(
        call(json!({"operation": "parse", "text": "not json"}).to_string())["error"],
        "ai_skin_response"
    );

    // A returned picture is checked by the shared client, header and all: a host that trusted the
    // declared type would render whatever arrived under it.
    // base64 of b"\x89PNG\r\n\x1A\nrest" and b"\xFF\xD8\xFFrest", written out so this crate does
    // not take a base64 dependency for two fixtures.
    let png = "iVBORw0KGgpyZXN0";
    assert_eq!(
        call(
            json!({
                "operation": "artwork",
                "artwork": {"b64_json": png, "mime_type": "image/png", "width": 512, "height": 512},
            })
            .to_string()
        )["ok"],
        true
    );
    let jpeg_header_on_a_png_claim = "/9j/cmVzdA==";
    assert_eq!(
        call(
            json!({
                "operation": "artwork",
                "artwork": {
                    "b64_json": jpeg_header_on_a_png_claim,
                    "mime_type": "image/png",
                    "width": 512,
                    "height": 512,
                },
            })
            .to_string()
        )["error"],
        "ai_skin_response"
    );
    assert_eq!(
        read(unsafe { msime_client_ai_skin_plan(std::ptr::null(), 0) })["ok"],
        false
    );
}

#[test]
#[cfg(not(target_os = "android"))]
fn skin_resource_bridge_revalidates_kind_and_package_containment() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("skins");
    let skin = root.join("sample");
    std::fs::create_dir_all(skin.join("images")).unwrap();
    std::fs::write(
        skin.join("skin.toml"),
        "schema_version = 1\nid = 'sample'\nname = 'Sample'\nversion = '1.0'\n\
         base = 'fluent'\ntoolbar_stylesheet = 'toolbar.css'\npreview = 'images/preview.png'\n\
         [supports]\nlayouts = ['vertical']\nthemes = ['dark']\n\
         [candidate_window]\nmin_width_dip = 10\n\
         [candidate_window.decoration]\ntop_inset_dip = 1\nwidth_dip = 10\n",
    )
    .unwrap();
    std::fs::write(skin.join("images/preview.png"), [1_u8, 2, 3]).unwrap();
    std::fs::write(skin.join("font.woff2"), [4_u8, 5, 6]).unwrap();
    std::fs::write(skin.join("toolbar.css"), ".sample { color: red; }").unwrap();
    let directory = root.to_str().unwrap().to_owned();
    let call = |request: Value| {
        let document = request.to_string();
        read(unsafe { msime_client_skin_resource(document.as_ptr(), document.len()) })
    };
    let image = call(json!({
        "directory": directory,
        "id": "sample",
        "relative": "images/preview.png",
        "kind": "image"
    }));
    assert_eq!(image["ok"], true);
    assert_eq!(image["value"]["contentType"], "image/png");
    assert_eq!(image["value"]["bytes"], json!([1, 2, 3]));
    let font = call(json!({
        "directory": root.to_str().unwrap(),
        "id": "sample",
        "relative": "font.woff2",
        "kind": "font"
    }));
    assert_eq!(font["ok"], true);
    assert_eq!(font["value"]["contentType"], "font/woff2");
    let mismatch = call(json!({
        "directory": root.to_str().unwrap(),
        "id": "sample",
        "relative": "toolbar.css",
        "kind": "image"
    }));
    assert_eq!(mismatch["ok"], false);
    let escaped = call(json!({
        "directory": root.to_str().unwrap(),
        "id": "sample",
        "relative": "../toolbar.css",
        "kind": "image"
    }));
    assert_eq!(escaped["ok"], false);
    let stylesheet_request = json!({
        "directory": root.to_str().unwrap(),
        "id": "sample"
    });
    let stylesheet_document = stylesheet_request.to_string();
    let stylesheet = read(unsafe {
        msime_client_skin_toolbar_stylesheet(
            stylesheet_document.as_ptr(),
            stylesheet_document.len(),
        )
    });
    assert_eq!(
        stylesheet,
        json!({"ok": true, "value": ".sample { color: red; }"})
    );
    let invalid = read(unsafe { msime_client_skin_resource(std::ptr::null(), 0) });
    assert_eq!(invalid["ok"], false);
}
#[test]
#[cfg(not(target_os = "android"))]
fn try_preferences_reader_reports_contention_without_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    let load = || read(unsafe { msime_client_try_load_preferences(path.as_ptr(), path.len()) });
    let initial = load();
    assert_eq!(initial["ok"], true);
    assert_eq!(initial["value"]["revision"], 0);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("preferences.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert_eq!(load(), json!({"ok": true, "value": null}));
    drop(lock);
    assert_eq!(load(), initial);
    std::fs::write(directory.path().join("preferences.json"), "broken").unwrap();
    assert_eq!(load()["ok"], false);
    assert_eq!(
        read(unsafe { msime_client_try_load_preferences(std::ptr::null(), 0) })["ok"],
        false
    );
}

#[test]
#[cfg(not(target_os = "android"))]
fn recover_preferences_backs_up_malformed_documents_only() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    let recover = || read(unsafe { msime_client_recover_preferences(path.as_ptr(), path.len()) });
    let document = directory.path().join("preferences.json");

    // Missing: nothing is written.
    let missing = recover();
    assert_eq!(missing["ok"], true);
    assert_eq!(missing["value"]["recovered"], false);
    assert_eq!(missing["value"]["snapshot"]["revision"], 0);
    assert!(!document.exists());

    // Malformed: backed up verbatim, then replaced by a document load accepts.
    std::fs::write(&document, "{\"format_version\":1,").unwrap();
    let repaired = recover();
    assert_eq!(repaired["ok"], true, "{repaired}");
    let value = &repaired["value"];
    assert_eq!(value["recovered"], true);
    assert_eq!(value["salvaged"], false);
    let backup = std::path::PathBuf::from(value["backup_path"].as_str().unwrap());
    assert_eq!(backup.parent().unwrap(), directory.path());
    assert_eq!(
        value["backup_name"].as_str().unwrap(),
        backup.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(std::fs::read(&backup).unwrap(), b"{\"format_version\":1,");
    let loaded = read(unsafe { msime_client_load_preferences(path.as_ptr(), path.len()) });
    assert_eq!(loaded["value"], value["snapshot"]);

    // Valid now: a second call is a no-op.
    let again = recover();
    assert_eq!(again["value"]["recovered"], false);
    assert_eq!(again["value"]["snapshot"], value["snapshot"]);

    // Well-formed but from a newer build: refused and left as it is.
    let future = json!({"format_version": 2, "revision": 3, "preferences": {}}).to_string();
    std::fs::write(&document, &future).unwrap();
    assert_eq!(recover()["ok"], false);
    assert_eq!(std::fs::read_to_string(&document).unwrap(), future);

    assert_eq!(
        read(unsafe { msime_client_recover_preferences(std::ptr::null(), 0) })["ok"],
        false
    );
    let relative = "relative";
    assert_eq!(
        read(unsafe { msime_client_recover_preferences(relative.as_ptr(), relative.len()) })["ok"],
        false
    );
}

#[test]
#[cfg(not(target_os = "android"))]
fn save_preferences_uses_compare_and_swap_and_rejects_invalid_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_string_lossy().into_owned();
    let snapshot = serde_json::to_string(&PreferencesSnapshot::default()).unwrap();
    let save = |revision, document: &str| {
        read(unsafe {
            msime_client_save_preferences(
                path.as_ptr(),
                path.len(),
                revision,
                document.as_ptr(),
                document.len(),
            )
        })
    };

    let saved = save(0, &snapshot);
    assert_eq!(saved["ok"], true);
    assert_eq!(saved["value"]["revision"], 1);
    let file = directory.path().join("preferences.json");
    let original = std::fs::read_to_string(&file).unwrap();

    let conflict = save(0, &snapshot);
    assert_eq!(conflict["ok"], false);
    assert!(conflict["error"].as_str().unwrap().contains("changed"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);

    let mut invalid = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    invalid["format_version"] = json!(2);
    let invalid = invalid.to_string();
    let rejected = save(1, &invalid);
    assert_eq!(rejected["ok"], false);
    assert!(rejected["error"].as_str().unwrap().contains("unsupported"));
    assert_eq!(std::fs::read_to_string(file).unwrap(), original);
}
#[test]
#[cfg(not(target_os = "android"))]
fn a_document_holding_a_custom_skin_photo_can_still_be_saved_and_applied() {
    // base64 of a PNG signature followed by zero bytes: about 40 KiB, well past the 16 KiB other buffers are held to.
    let photo = format!("iVBORw0KGgoA{}", "AAAA".repeat(10_000));
    let mut preferences = Preferences::default();
    preferences.custom_touch_keyboard_skin.photo = Some(photo.clone());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_string_lossy().into_owned();
    let snapshot = serde_json::to_string(&PreferencesSnapshot {
        format_version: 1,
        revision: 0,
        preferences: preferences.clone(),
    })
    .unwrap();
    assert!(snapshot.len() > 16_384);
    let saved = read(unsafe {
        msime_client_save_preferences(
            path.as_ptr(),
            path.len(),
            0,
            snapshot.as_ptr(),
            snapshot.len(),
        )
    });
    assert_eq!(saved["ok"], true, "{saved}");
    let loaded = PreferencesStore::new(directory.path()).load().unwrap();
    assert_eq!(
        loaded.preferences.custom_touch_keyboard_skin.photo,
        Some(photo)
    );

    let handle = test_host(&directory.path().join("host"));
    assert_eq!(update(handle, 1, &preferences)["ok"], true);
    read(msime_client_destroy(handle));

    // HostOptions carries the same preference object. Session creation must
    // accept the saved design too; the old 16 KiB boundary rejected this
    // otherwise valid configuration before the Engine was even opened.
    let oversized_options = serde_json::to_string(&json!({
        "api_version": 1,
        "resources": directory.path().join("oversized-host/resources"),
        "user_data": directory.path().join("oversized-host/user"),
        "cache": directory.path().join("oversized-host/cache"),
        "dictionaries": directory.path().join("oversized-host/dictionaries"),
        "preferences": preferences,
    }))
    .unwrap();
    for name in ["resources", "user", "cache", "dictionaries"] {
        std::fs::create_dir_all(directory.path().join("oversized-host").join(name)).unwrap();
    }
    assert!(oversized_options.len() > 16_384);
    let oversized_handle =
        read(unsafe { msime_client_create(oversized_options.as_ptr(), oversized_options.len()) });
    assert_eq!(oversized_handle["ok"], true, "{oversized_handle}");
    let oversized_handle = oversized_handle["value"]["session"].as_u64().unwrap();
    read(msime_client_destroy(oversized_handle));
}
#[test]
fn background_preferences_reader_uses_shared_store_and_preserves_bad_files() {
    let directory = tempfile::tempdir().unwrap();
    let saved = PreferencesStore::new(directory.path())
        .save(0, Preferences::default())
        .unwrap();
    let path = directory.path().to_str().unwrap().to_owned();
    let load = |path: String| {
        std::thread::spawn(move || {
            read(unsafe { msime_client_load_preferences(path.as_ptr(), path.len()) })
        })
        .join()
        .unwrap()
    };
    assert_eq!(
        load(path.clone())["value"],
        serde_json::to_value(saved).unwrap()
    );
    let file = directory.path().join("preferences.json");
    std::fs::write(&file, "broken").unwrap();
    assert_eq!(load(path)["ok"], false);
    assert_eq!(std::fs::read_to_string(file).unwrap(), "broken");
    assert_eq!(load("relative".into())["ok"], false);
    assert_eq!(
        read(unsafe { msime_client_load_preferences(std::ptr::null(), 0) })["ok"],
        false
    );
}
#[test]
fn typing_statistics_boundary_persists_only_aggregate_counts() {
    let directory = tempfile::tempdir().unwrap();
    let call = |action: Value| {
        let request = serde_json::to_vec(&json!({
            "directory": directory.path(),
            "action": action,
        }))
        .unwrap();
        read(unsafe { msime_client_typing_statistics(request.as_ptr(), request.len()) })
    };
    // Statistics ship off, so a fresh directory records nothing until asked. That is the
    // boundary's behaviour too, and it is asserted before turning them on.
    assert_eq!(
        call(json!({"operation": "load"}))["value"]["enabled"],
        false
    );
    assert_eq!(
        call(json!({
            "operation": "record",
            "text": "ignored",
            "source": "handwriting",
            "day": "2026-09-12",
        }))["value"]["recorded"],
        0
    );
    assert_eq!(
        call(json!({"operation": "set_enabled", "enabled": true}))["value"]["enabled"],
        true
    );
    let recorded = call(json!({
        "operation": "record",
        "text": "synthetic 🌲",
        "source": "handwriting",
        "day": "2026-09-12",
    }));
    assert_eq!(recorded["value"]["recorded"], 10);
    let loaded = call(json!({"operation": "load"}));
    assert_eq!(loaded["value"]["total"], 10);
    assert_eq!(loaded["value"]["detail"]["characters"]["latin"], 9);
    assert_eq!(loaded["value"]["detail"]["characters"]["emoji"], 1);
    assert_eq!(loaded["value"]["detail"]["sources"]["handwriting"], 10);
    let persisted =
        std::fs::read_to_string(directory.path().join("typing-statistics.json")).unwrap();
    assert!(!persisted.contains("synthetic"));
    assert_eq!(
        call(json!({"operation": "set_enabled", "enabled": false}))["value"]["enabled"],
        false
    );
    assert_eq!(
        call(json!({
            "operation": "record",
            "text": "ignored",
            "source": "english",
            "day": "2026-09-12",
        }))["value"]["recorded"],
        0
    );
    let reset = call(json!({"operation": "reset"}));
    assert_eq!(reset["value"]["total"], 0);
    assert_eq!(reset["value"]["enabled"], false);
    assert_eq!(
        read(unsafe { msime_client_typing_statistics(std::ptr::null(), 0) })["ok"],
        false
    );
}
/// A session over the two-candidate `nihao` fixture, focused, with typing statistics switched as asked, and the store it writes to.
fn selection_statistics_host(
    root: &std::path::Path,
    enabled: bool,
) -> (u64, TypingStatisticsStore) {
    let store = TypingStatisticsStore::new(root.join("user"));
    store.set_enabled(enabled).unwrap();
    let handle = test_host_with_pinyin_fixture(root, chinese_preferences());
    assert_eq!(read(msime_client_focus(handle, true))["ok"], true);
    (handle, store)
}
/// Type `nihao` and commit the candidate at `index` by position, as a click or a tap does.
fn commit_candidate_by_position(handle: u64, index: usize) {
    let mut view = Value::Null;
    for byte in b"nihao" {
        view = read(msime_client_character(handle, *byte, false))["value"]["view"].clone();
    }
    let generation = view["generation"].as_u64().unwrap();
    let selected = read(msime_client_select(handle, generation, index));
    assert!(selected["value"]["commit"].is_string(), "{selected}");
}
#[test]
fn selection_statistics_reach_the_store_at_focus_out() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, store) = selection_statistics_host(dir.path(), true);
    for index in [0, 1, 1] {
        commit_candidate_by_position(handle, index);
    }
    // Held in the session until the field ends, which is the point: no document cycle per selection.
    assert_eq!(store.load().unwrap().selections.total(), 0);
    assert_eq!(read(msime_client_focus(handle, false))["ok"], true);
    let selections = store.load().unwrap().selections;
    assert_eq!(selections.ranks[0], 1);
    assert_eq!(selections.ranks[1], 2);
    assert_eq!(selections.total(), 3);
    // A second focus-out has nothing left to write and must not count anything twice.
    assert_eq!(read(msime_client_focus(handle, true))["ok"], true);
    assert_eq!(read(msime_client_focus(handle, false))["ok"], true);
    assert_eq!(store.load().unwrap().selections.total(), 3);
    read(msime_client_destroy(handle));
    assert_eq!(store.load().unwrap().selections.total(), 3);
}
#[test]
fn selection_statistics_reach_the_store_when_the_session_is_destroyed() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, store) = selection_statistics_host(dir.path(), true);
    commit_candidate_by_position(handle, 1);
    commit_candidate_by_position(handle, 0);
    assert_eq!(store.load().unwrap().selections.total(), 0);
    // No focus-out first: a host tearing the session down directly still keeps what it counted.
    assert_eq!(read(msime_client_destroy(handle))["ok"], true);
    let selections = store.load().unwrap().selections;
    assert_eq!(selections.ranks[0], 1);
    assert_eq!(selections.ranks[1], 1);
    assert_eq!(selections.total(), 2);
}
#[test]
fn selection_statistics_are_written_once_a_batch_fills() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, store) = selection_statistics_host(dir.path(), true);
    let batch = SELECTION_BATCH as usize;
    for round in 0..batch - 1 {
        commit_candidate_by_position(handle, round % 2);
    }
    assert_eq!(store.load().unwrap().selections.total(), 0);
    // The selection that fills the batch writes it, with no focus-out, which bounds what a killed process loses.
    commit_candidate_by_position(handle, 0);
    assert_eq!(store.load().unwrap().selections.total(), SELECTION_BATCH);
    commit_candidate_by_position(handle, 1);
    assert_eq!(store.load().unwrap().selections.total(), SELECTION_BATCH);
    read(msime_client_focus(handle, false));
    let selections = store.load().unwrap().selections;
    assert_eq!(selections.total(), SELECTION_BATCH + 1);
    assert_eq!(selections.ranks[0], SELECTION_BATCH / 2 + 1);
    assert_eq!(selections.ranks[1], SELECTION_BATCH / 2);
    read(msime_client_destroy(handle));
}
#[test]
fn selection_statistics_write_nothing_while_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let (handle, store) = selection_statistics_host(dir.path(), false);
    let path = store.directory().join("typing-statistics.json");
    let before = std::fs::read(&path).unwrap();
    let written = std::fs::metadata(&path).unwrap().modified().unwrap();
    for round in 0..SELECTION_BATCH as usize + 3 {
        commit_candidate_by_position(handle, round % 2);
    }
    read(msime_client_focus(handle, false));
    read(msime_client_destroy(handle));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        written
    );
    assert_eq!(store.load().unwrap().selections.total(), 0);
}
#[test]
fn clipboard_reader_respects_preferences_and_preserves_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    let load = || read(unsafe { msime_client_load_clipboard_history(path.as_ptr(), path.len()) });
    let store = PreferencesStore::new(directory.path());
    let saved = store
        .save(
            0,
            Preferences {
                clipboard_history: true,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(load()["value"]["entries"], serde_json::json!([]));
    let file = directory.path().join("clipboard_history.json");
    let fixture = r#"["synthetic alpha","synthetic beta","synthetic alpha"]"#;
    std::fs::write(&file, fixture).unwrap();
    assert_eq!(
        load()["value"]["entries"],
        serde_json::json!(["synthetic alpha", "synthetic beta"])
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), fixture);
    std::fs::write(&file, "broken synthetic fixture").unwrap();
    assert_eq!(load()["error"], "clipboard history unavailable");
    let mut preferences = saved.preferences;
    preferences.clipboard_history = false;
    store.save(saved.revision, preferences).unwrap();
    assert_eq!(
        load()["value"],
        serde_json::json!({"enabled": false, "entries": []})
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "broken synthetic fixture"
    );
    std::fs::write(directory.path().join("preferences.json"), "broken").unwrap();
    assert_eq!(load()["error"], "history preferences unavailable");
    assert_eq!(
        read(unsafe { msime_client_load_clipboard_history(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_load_clipboard_history(b"relative".as_ptr(), 8) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_load_clipboard_history([255u8].as_ptr(), 1) })["ok"],
        false
    );
}

#[test]
fn mobile_clipboard_migrates_apple_history_and_uses_structured_actions() {
    let directory = tempfile::tempdir().unwrap();
    let call = |action: Value| {
        let request = serde_json::to_vec(&json!({
            "directory": directory.path(),
            "action": action,
        }))
        .unwrap();
        read(unsafe { msime_client_mobile_clipboard_history(request.as_ptr(), request.len()) })
    };
    let legacy = directory.path().join("Clipboard/history.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy,
        serde_json::to_vec(&json!([
            {
                "id": "00000000-0000-4000-8000-000000000001",
                "text": "synthetic older",
                "date": 721_692_800.0,
                "pinned": false
            },
            {
                "id": "00000000-0000-4000-8000-000000000002",
                "text": "synthetic pinned",
                "date": 721_692_900.0,
                "pinned": true
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let loaded = call(json!({"operation": "load"}));
    assert_eq!(loaded["value"]["migrated"], true);
    assert_eq!(loaded["value"]["entries"][0]["text"], "synthetic pinned");
    assert_eq!(
        loaded["value"]["entries"][0]["timestampMs"],
        1_700_000_100_000_u64
    );
    assert_eq!(loaded["value"]["entries"][1]["text"], "synthetic older");
    assert!(!legacy.exists());
    assert!(directory
        .path()
        .join("MSIME/clipboard_history.json")
        .exists());
    assert!(!directory.path().join("preferences.json").exists());
    assert_eq!(
        call(json!({"operation": "load"}))["value"]["migrated"],
        false
    );

    assert_eq!(
        call(json!({"operation": "capture", "text": "synthetic current"}))["value"]["captured"],
        true
    );
    assert_eq!(
        call(json!({
            "operation": "set_pinned",
            "text": "synthetic current",
            "pinned": true
        }))["value"]["updated"],
        true
    );
    let pinned = call(json!({"operation": "load"}));
    assert_eq!(pinned["value"]["entries"][0]["text"], "synthetic current");
    assert_eq!(pinned["value"]["entries"][1]["text"], "synthetic pinned");
    assert_eq!(
        call(json!({"operation": "remove", "text": "synthetic older"}))["value"]["removed"],
        true
    );
    assert_eq!(
        call(json!({"operation": "clear"}))["value"]["cleared"],
        true
    );
    assert_eq!(
        call(json!({"operation": "load"}))["value"]["entries"],
        json!([])
    );
}

#[test]
fn harmony_mobile_clipboard_migrates_once_and_preserves_corrupt_current_data() {
    let directory = tempfile::tempdir().unwrap();
    let call = |action: Value| {
        let request = serde_json::to_vec(&json!({
            "directory": directory.path(),
            "legacy": "harmony_state",
            "action": action,
        }))
        .unwrap();
        read(unsafe { msime_client_mobile_clipboard_history(request.as_ptr(), request.len()) })
    };
    let legacy = directory.path().join("state/clipboard-history.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy,
        serde_json::to_vec(&json!([
            {"text": "synthetic older", "at": 10, "pinned": false},
            {"text": "synthetic pinned", "at": 1, "pinned": true}
        ]))
        .unwrap(),
    )
    .unwrap();

    let loaded = call(json!({"operation": "load"}));
    assert_eq!(loaded["value"]["migrated"], true);
    assert_eq!(loaded["value"]["entries"][0]["text"], "synthetic pinned");
    assert!(!legacy.exists());

    let captured = call(json!({"operation": "capture", "text": "synthetic current"}));
    assert_eq!(captured["value"]["captured"], true);
    assert_eq!(captured["value"]["entries"][1]["text"], "synthetic current");
    let shared = directory.path().join("MSIME/clipboard_history.json");
    let corrupt = b"invalid synthetic current history";
    std::fs::write(&shared, corrupt).unwrap();
    let refused = call(json!({"operation": "capture", "text": "synthetic rejected"}));
    assert_eq!(refused["ok"], false);
    assert_eq!(std::fs::read(&shared).unwrap(), corrupt);
}

#[test]
fn harmony_mobile_clipboard_rejects_corrupt_legacy_without_replacing_it() {
    let directory = tempfile::tempdir().unwrap();
    let legacy = directory.path().join("state/clipboard-history.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let corrupt = b"invalid synthetic harmony history";
    std::fs::write(&legacy, corrupt).unwrap();
    let request = serde_json::to_vec(&json!({
        "directory": directory.path(),
        "legacy": "harmony_state",
        "action": {"operation": "load"}
    }))
    .unwrap();
    let response =
        read(unsafe { msime_client_mobile_clipboard_history(request.as_ptr(), request.len()) });
    assert_eq!(response["error"], "invalid legacy clipboard history");
    assert_eq!(std::fs::read(&legacy).unwrap(), corrupt);
    assert!(!directory
        .path()
        .join("MSIME/clipboard_history.json")
        .exists());
}

#[test]
fn mobile_clipboard_preserves_invalid_legacy_and_existing_shared_history() {
    let invalid = tempfile::tempdir().unwrap();
    let legacy = invalid.path().join("Clipboard/history.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let fixture = b"invalid synthetic legacy";
    std::fs::write(&legacy, fixture).unwrap();
    let request = serde_json::to_vec(&json!({
        "directory": invalid.path(),
        "action": {"operation": "load"}
    }))
    .unwrap();
    let response =
        read(unsafe { msime_client_mobile_clipboard_history(request.as_ptr(), request.len()) });
    assert_eq!(response["error"], "invalid legacy clipboard history");
    assert_eq!(std::fs::read(&legacy).unwrap(), fixture);
    assert!(!invalid.path().join("MSIME/clipboard_history.json").exists());

    let existing = tempfile::tempdir().unwrap();
    let call = |action: Value| {
        let request = serde_json::to_vec(&json!({
            "directory": existing.path(),
            "action": action,
        }))
        .unwrap();
        read(unsafe { msime_client_mobile_clipboard_history(request.as_ptr(), request.len()) })
    };
    assert_eq!(
        call(json!({"operation": "capture", "text": "synthetic shared"}))["value"]["captured"],
        true
    );
    let legacy = existing.path().join("Clipboard/history.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, b"invalid synthetic legacy").unwrap();
    let loaded = call(json!({"operation": "load"}));
    assert_eq!(loaded["value"]["entries"][0]["text"], "synthetic shared");
    assert!(legacy.exists());
    assert_eq!(
        call(json!({"operation": "clear"}))["value"]["cleared"],
        true
    );
    assert!(!legacy.exists());
    assert_eq!(
        call(json!({"operation": "load"}))["value"]["entries"],
        json!([])
    );

    let null = read(unsafe { msime_client_mobile_clipboard_history(std::ptr::null(), 0) });
    assert_eq!(null["ok"], false);
    let relative = serde_json::to_vec(&json!({
        "directory": "relative",
        "action": {"operation": "load"}
    }))
    .unwrap();
    assert_eq!(
        read(unsafe { msime_client_mobile_clipboard_history(relative.as_ptr(), relative.len()) })
            ["ok"],
        false
    );
}

#[test]
fn history_removal_is_exact_idempotent_and_respects_disabled_setting() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("clipboard_history.json");
    let store = PreferencesStore::new(directory.path());
    let saved = store
        .save(
            0,
            Preferences {
                clipboard_history: true,
                ..Preferences::default()
            },
        )
        .unwrap();
    let remove = |path: &std::path::Path, text: &str| {
        let request = serde_json::to_vec(&json!({"directory": path, "text": text})).unwrap();
        read(unsafe { msime_client_remove_clipboard_history(request.as_ptr(), request.len()) })
    };
    std::fs::write(&file, br#"["synthetic first","synthetic second"]"#).unwrap();
    assert_eq!(
        remove(directory.path(), "synthetic first")["value"]["removed"],
        true
    );
    let upgraded: Vec<msime_client_core::clipboard::ClipboardHistoryEntry> =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(upgraded.len(), 1);
    assert_eq!(upgraded[0].text, "synthetic second");
    assert_eq!(
        remove(directory.path(), "synthetic first")["value"]["removed"],
        false
    );
    assert_eq!(remove(directory.path(), "")["ok"], false);
    assert_eq!(
        remove(std::path::Path::new("relative"), "synthetic")["ok"],
        false
    );
    let maximum = msime_client_core::clipboard::MAX_TEXT_BYTES;
    assert_eq!(remove(directory.path(), &"x".repeat(maximum))["ok"], true);
    assert_eq!(
        remove(directory.path(), &"x".repeat(maximum + 1))["ok"],
        false
    );
    let mut preferences = saved.preferences;
    preferences.clipboard_history = false;
    store.save(saved.revision, preferences).unwrap();
    assert_eq!(
        remove(directory.path(), "synthetic second")["error"],
        "clipboard history disabled"
    );
    let preserved: Vec<msime_client_core::clipboard::ClipboardHistoryEntry> =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(preserved, upgraded);
    assert_eq!(
        read(unsafe { msime_client_remove_clipboard_history(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_remove_clipboard_history(b"{".as_ptr(), 1) })["ok"],
        false
    );
}

#[test]
fn history_capture_bridge_validates_and_returns_no_text() {
    let directory = tempfile::tempdir().unwrap();
    let capture = |request: Value| {
        let bytes = serde_json::to_vec(&request).unwrap();
        read(unsafe { msime_client_capture_clipboard_history(bytes.as_ptr(), bytes.len()) })
    };
    let request = json!({"directory": directory.path(), "text": "synthetic capture"});
    assert_eq!(
        capture(request.clone())["value"],
        json!({"captured": false})
    );
    assert!(!directory.path().join("clipboard_history.json").exists());
    PreferencesStore::new(directory.path())
        .save(
            0,
            Preferences {
                clipboard_history: true,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(capture(request.clone())["value"], json!({"captured": true}));
    assert_eq!(
        capture(json!({"directory": "relative", "text": "synthetic"}))["ok"],
        false
    );
    let maximum = msime_client_core::clipboard::MAX_TEXT_BYTES;
    assert_eq!(
        capture(json!({"directory": directory.path(), "text": "x".repeat(maximum)}))["ok"],
        true
    );
    assert_eq!(
        capture(json!({"directory": directory.path(), "text": "x".repeat(maximum + 1)}))["ok"],
        false
    );
    assert_eq!(capture(json!({"directory": directory.path()}))["ok"], false);
    assert_eq!(
        read(unsafe { msime_client_capture_clipboard_history(std::ptr::null(), 0) })["ok"],
        false
    );
    std::fs::write(directory.path().join("preferences.json"), "broken").unwrap();
    assert_eq!(
        capture(request)["error"],
        "clipboard history capture failed"
    );
}

fn test_host(root: &std::path::Path) -> u64 {
    test_host_preferences(root, chinese_preferences())
}
fn chinese_preferences() -> Preferences {
    Preferences {
        default_ime_mode: msime_client_core::preferences::DefaultImeMode::Chinese,
        ..Preferences::default()
    }
}
fn test_host_preferences(root: &std::path::Path, preferences: Preferences) -> u64 {
    let path = |name| {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let options = json!({ "api_version": 1, "resources": path("resources"), "user_data": path("user"), "cache": path("cache"), "dictionaries": path("dictionaries"), "preferences": preferences }).to_string();
    let created = read(unsafe { msime_client_create(options.as_ptr(), options.len()) });
    assert_eq!(created["ok"], true);
    created["value"]["session"].as_u64().unwrap()
}
fn test_host_with_pinyin_fixture(root: &std::path::Path, preferences: Preferences) -> u64 {
    let path = |name| {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let resources = path("resources");
    let dictionaries = path("dictionaries");
    let fixture = "CREATE TABLE tbl_2_n(key TEXT,jp TEXT,value TEXT,weight INTEGER);
                   INSERT INTO tbl_2_n VALUES('ni''hao','nh','本地',100),('ni''hao','nh','拟好',80);
                   CREATE TABLE wubi86(key TEXT,value TEXT,weight INTEGER);
                   CREATE TABLE quick_parases(key TEXT,value TEXT,weight INTEGER);
                   CREATE INDEX idx_quick_parases_key_weight ON quick_parases(key,weight DESC);";
    for directory in [&resources, &dictionaries] {
        rusqlite::Connection::open(directory.join("msime.db"))
            .unwrap()
            .execute_batch(fixture)
            .unwrap();
    }
    let options = json!({ "api_version": 1, "resources": resources, "user_data": path("user"), "cache": path("cache"), "dictionaries": dictionaries, "preferences": preferences }).to_string();
    let created = read(unsafe { msime_client_create(options.as_ptr(), options.len()) });
    assert_eq!(created["ok"], true);
    created["value"]["session"].as_u64().unwrap()
}
fn update(handle: u64, revision: u64, preferences: &Preferences) -> Value {
    let snapshot = json!({ "format_version": 1, "revision": revision, "preferences": preferences })
        .to_string();
    read(unsafe { msime_client_update_preferences(handle, snapshot.as_ptr(), snapshot.len()) })
}

#[test]
fn translation_queries_use_latest_preferences_without_resetting_composition() {
    let dir = tempfile::tempdir().unwrap();
    let mut preferences = Preferences {
        candidate_translations: false,
        ..Preferences::default()
    };
    let handle = test_host_preferences(dir.path(), preferences.clone());
    read(msime_client_focus(handle, true));
    let mut view = Value::Null;
    for byte in b"U4e2d" {
        view = read(msime_client_character(
            handle,
            *byte,
            byte.is_ascii_uppercase(),
        ))["value"]["view"]
            .clone();
    }
    assert!(!view["candidates"].as_array().unwrap().is_empty());
    assert_eq!(
        read(msime_client_translation_query(handle))["value"],
        Value::Null
    );
    preferences.candidate_translations = true;
    preferences.translation_target_language =
        msime_client_core::preferences::TranslationTargetLanguage::Fr;
    preferences.translation_secondary_language =
        Some(msime_client_core::preferences::TranslationTargetLanguage::Ja);
    preferences.custom_translation.enabled = true;
    preferences.custom_translation.endpoint = "https://translation.example.invalid".into();
    preferences.tencent_tmt.secret_id = "AKIDsynthetic".into();
    preferences.tencent_tmt.secret_key = "synthetic".into();
    let changed = update(handle, 1, &preferences);
    assert_eq!(changed["value"]["deferred"], true);
    assert_eq!(changed["value"]["view"]["generation"], view["generation"]);
    let query = read(msime_client_translation_query(handle));
    assert_eq!(query["value"]["generation"], view["generation"]);
    assert_eq!(query["value"]["target_language"], "fr");
    assert_eq!(query["value"]["target_languages"], json!(["fr", "ja"]));
    assert!(query["value"]["user_data"].is_null());
    assert_eq!(
        query["value"]["custom_translation"]["endpoint"],
        "https://translation.example.invalid"
    );
    assert!(!query["value"]["candidates"].as_array().unwrap().is_empty());
    assert!(query["value"]["tencent_tmt"].is_null());
    preferences.custom_translation.enabled = false;
    update(handle, 2, &preferences);
    let tencent_query = read(msime_client_translation_query(handle));
    assert_eq!(tencent_query["value"]["generation"], view["generation"]);
    assert_eq!(
        tencent_query["value"]["tencent_tmt"]["region"],
        "ap-guangzhou"
    );
    assert_eq!(
        tencent_query["value"]["tencent_tmt"]["secret_id"],
        "AKIDsynthetic"
    );
    preferences.tencent_tmt.enabled = false;
    update(handle, 3, &preferences);
    assert!(read(msime_client_translation_query(handle))["value"]["tencent_tmt"].is_null());
    preferences.tencent_tmt.enabled = true;
    preferences.tencent_tmt.secret_key.clear();
    update(handle, 4, &preferences);
    assert!(read(msime_client_translation_query(handle))["value"]["tencent_tmt"].is_null());
    preferences.candidate_translations = false;
    update(handle, 5, &preferences);
    assert_eq!(
        read(msime_client_translation_query(handle))["value"],
        Value::Null
    );

    // The offline gloss is a packaged dictionary lookup, so it has to be
    // reachable with every online provider off - which is the usual case,
    // and was why Windows could not offer the setting at all.
    preferences.candidate_english_gloss = true;
    preferences.translation_target_language =
        msime_client_core::preferences::TranslationTargetLanguage::En;
    update(handle, 6, &preferences);
    let gloss = read(msime_client_translation_query(handle));
    assert_eq!(gloss["value"]["generation"], view["generation"]);
    assert_eq!(gloss["value"]["english_gloss"], true);
    assert_eq!(gloss["value"]["target_language"], "en");
    // The dictionary paths ride along only because the lookup needs them.
    assert!(gloss["value"]["resources"].is_string());
    assert!(!gloss["value"]["candidates"].as_array().unwrap().is_empty());
    // Still no online provider: the gloss must not imply one.
    assert!(gloss["value"]["tencent_tmt"].is_null());
    assert!(gloss["value"]["custom_translation"].is_null());
    assert!(gloss["value"]["niutrans"].is_null());

    // The packaged gloss dictionary is English only, so another target
    // language is an online request or nothing - never a wrong-language
    // gloss.
    preferences.translation_target_language =
        msime_client_core::preferences::TranslationTargetLanguage::Ja;
    update(handle, 7, &preferences);
    assert_eq!(
        read(msime_client_translation_query(handle))["value"],
        Value::Null
    );

    // An English secondary language still enables the packaged offline
    // dictionary while preserving the user's primary target.
    preferences.translation_secondary_language =
        Some(msime_client_core::preferences::TranslationTargetLanguage::En);
    preferences.candidate_english_gloss = true;
    update(handle, 8, &preferences);
    let secondary_gloss = read(msime_client_translation_query(handle));
    assert_eq!(
        secondary_gloss["value"]["target_languages"],
        json!(["ja", "en"])
    );
    assert_eq!(secondary_gloss["value"]["english_gloss"], true);

    // With the gloss off, only the user path needed to persist successful
    // English-target provider results is carried. Packaged resources stay
    // private to offline lookup.
    preferences.candidate_english_gloss = false;
    preferences.translation_target_language =
        msime_client_core::preferences::TranslationTargetLanguage::En;
    preferences.candidate_translations = true;
    preferences.tencent_tmt.secret_key = "synthetic".into();
    update(handle, 9, &preferences);
    let online = read(msime_client_translation_query(handle));
    assert_eq!(online["value"]["english_gloss"], false);
    assert!(online["value"]["resources"].is_null());
    assert!(online["value"]["user_data"].is_string());
    read(msime_client_destroy(handle));
}

fn offline_gloss_fixture(path: &std::path::Path, language: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TABLE zh_glosses(chinese TEXT PRIMARY KEY, gloss TEXT NOT NULL, source TEXT NOT NULL) WITHOUT ROWID;
             CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
             INSERT INTO meta VALUES('target_language', '{language}');
             INSERT INTO zh_glosses VALUES('你好', 'bonjour, salut', 'hello');
             PRAGMA user_version = 1;"
        ))
        .unwrap();
}

/// A non-English offline dictionary is announced only when it is installed, and the query without one is the one hosts always received.
#[test]
fn translation_query_lists_installed_offline_gloss_languages() {
    let dir = tempfile::tempdir().unwrap();
    let mut preferences = Preferences {
        candidate_translations: false,
        candidate_english_gloss: true,
        translation_target_language: msime_client_core::preferences::TranslationTargetLanguage::Fr,
        ..Preferences::default()
    };
    preferences.tencent_tmt.enabled = false;
    let handle = test_host_preferences(dir.path(), preferences.clone());
    read(msime_client_focus(handle, true));
    for byte in b"U4e2d" {
        read(msime_client_character(
            handle,
            *byte,
            byte.is_ascii_uppercase(),
        ));
    }
    // Nothing installed: French with translation off has no gloss source, as before.
    assert_eq!(
        read(msime_client_translation_query(handle))["value"],
        Value::Null
    );
    preferences.candidate_translations = true;
    update(handle, 1, &preferences);
    let plain = read(msime_client_translation_query(handle));
    assert!(plain["value"].get("offline_gloss_languages").is_none());
    assert!(plain["value"]["resources"].is_null());

    offline_gloss_fixture(&dir.path().join("offline-glosses/zh-fr.db"), "fr");
    offline_gloss_fixture(&dir.path().join("offline-glosses/zh-ko.db"), "ko");
    preferences.translation_secondary_language =
        Some(msime_client_core::preferences::TranslationTargetLanguage::Ja);
    update(handle, 2, &preferences);
    let installed = read(msime_client_translation_query(handle));
    // Korean is installed but not a target; Japanese is a target but not installed.
    assert_eq!(installed["value"]["offline_gloss_languages"], json!(["fr"]));
    assert!(installed["value"]["resources"].is_string());
    assert_eq!(installed["value"]["english_gloss"], false);

    // The offline switch alone reaches it, with online translation off and no provider implied.
    preferences.candidate_translations = false;
    update(handle, 3, &preferences);
    let offline = read(msime_client_translation_query(handle));
    assert_eq!(offline["value"]["offline_gloss_languages"], json!(["fr"]));
    assert_eq!(offline["value"]["translation_account"], false);
    assert!(offline["value"]["tencent_tmt"].is_null());

    // Both switches off: nothing, installed or not.
    preferences.candidate_english_gloss = false;
    update(handle, 4, &preferences);
    assert_eq!(
        read(msime_client_translation_query(handle))["value"],
        Value::Null
    );
    read(msime_client_destroy(handle));
}

#[test]
fn candidate_gloss_request_reads_the_offline_dictionary_for_its_target_language() {
    let directory = tempfile::tempdir().unwrap();
    let resources = directory.path().join("resources");
    std::fs::create_dir_all(&resources).unwrap();
    let user = directory.path().join("user");
    std::fs::create_dir_all(&user).unwrap();
    let resources = resources.to_str().unwrap().as_bytes().to_vec();
    let call = |request: Value| {
        let request = serde_json::to_vec(&request).unwrap();
        read(unsafe {
            msime_client_candidate_gloss_request(
                request.as_ptr(),
                request.len(),
                resources.as_ptr(),
                resources.len(),
            )
        })
    };
    let candidates = json!([
        {"text":"你好","source":0},
        {"text":"Hello","source":4},
        {"text":"再见","source":0}
    ]);
    // Not installed is not an error: the host keeps whatever the online path brings.
    let missing = call(json!({"generation":7,"target_language":"fr","candidates":candidates}));
    assert_eq!(missing["ok"], true);
    assert_eq!(missing["value"], json!({"generation":7,"translations":[]}));

    offline_gloss_fixture(&directory.path().join("offline-glosses/zh-fr.db"), "fr");
    // The user directory holds English only, so a French request never reads it even when given.
    std::fs::write(user.join("custom_translations.txt"), "你好\thand written\n").unwrap();
    let french = call(json!({
        "generation":8,
        "target_language":"fr",
        "user_data":user.to_str().unwrap(),
        "candidates":candidates
    }));
    assert_eq!(french["ok"], true);
    assert_eq!(
        french["value"],
        json!({"generation":8,"translations":[{"text":"你好","translation":"bonjour, salut"}]})
    );

    // A file under the wrong name is refused rather than shown as another language.
    std::fs::copy(
        directory.path().join("offline-glosses/zh-fr.db"),
        directory.path().join("offline-glosses/zh-ja.db"),
    )
    .unwrap();
    let renamed = call(json!({"generation":9,"target_language":"ja","candidates":candidates}));
    assert_eq!(renamed["ok"], false);
    assert_eq!(renamed["error"], "candidate gloss dictionary unavailable");

    for language in ["xx", "EN", "../fr", ""] {
        let invalid =
            call(json!({"generation":10,"target_language":language,"candidates":candidates}));
        assert_eq!(
            invalid["error"], "invalid candidate gloss request",
            "{language}"
        );
    }
    // English, spelled out or implied, is still the packaged dictionary, which this directory lacks.
    for request in [
        json!({"generation":11,"target_language":"en","candidates":candidates}),
        json!({"generation":11,"candidates":candidates}),
    ] {
        assert_eq!(
            call(request)["error"],
            "candidate gloss dictionary unavailable"
        );
    }
}

/// Linux keeps the Tencent secret in the provider's own file, so the query's credential fields cannot say which service the user picked. The explicit choice has to survive to the socket even when that service is unusable, or the provider falls back to Tencent.
#[cfg(unix)]
#[test]
fn translation_query_names_the_selected_service_through_the_provider_socket() {
    use std::io::{BufRead, Write};
    let dir = tempfile::tempdir().unwrap();
    let mut preferences = Preferences {
        candidate_translations: true,
        ..Preferences::default()
    };
    let handle = test_host_preferences(dir.path(), preferences.clone());
    read(msime_client_focus(handle, true));
    for byte in b"U4e2d" {
        read(msime_client_character(
            handle,
            *byte,
            byte.is_ascii_uppercase(),
        ));
    }
    let sockets = tempfile::tempdir().unwrap();
    // The provider connection refuses a socket directory other users can reach.
    std::fs::set_permissions(
        sockets.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let socket = sockets.path().join("translation.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    // Send the query as the Linux hosts do and return the document the provider received, or None when nothing connected.
    let forward = |query: &Value| -> Option<Value> {
        let mut transport = query.clone();
        transport["candidates"] = Value::Array(
            query["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|candidate| candidate["text"].clone())
                .collect(),
        );
        let request = transport.to_string();
        let path = socket.to_str().unwrap();
        std::thread::scope(|scope| {
            let call = scope.spawn(|| {
                read(unsafe {
                    msime_client_translation_provider_request(
                        request.as_ptr(),
                        request.len(),
                        path.as_ptr(),
                        path.len(),
                    )
                })
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            // A call that connected cannot return before it is answered, so a finished call with nothing pending never connected.
            let received = loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut line = String::new();
                        std::io::BufReader::new(stream.try_clone().unwrap())
                            .read_line(&mut line)
                            .unwrap();
                        stream.write_all(b"{\"translations\":[]}\n").unwrap();
                        break Some(serde_json::from_str::<Value>(&line).unwrap());
                    }
                    Err(_) if call.is_finished() => break None,
                    Err(_) => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "provider request hung"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            };
            assert_eq!(call.join().unwrap()["value"], json!({"translations": []}));
            received
        })
    };

    let query = read(msime_client_translation_query(handle))["value"].clone();
    assert_eq!(query["provider"], "tencent");
    // The MSIME account endpoint is never chosen implicitly.
    assert_eq!(query["translation_account"], false);
    assert_eq!(forward(&query).unwrap()["query"]["provider"], "tencent");

    preferences.tencent_tmt.enabled = false;
    update(handle, 1, &preferences);
    let query = read(msime_client_translation_query(handle))["value"].clone();
    assert_eq!(query["provider"], "none");
    assert!(
        forward(&query).is_none(),
        "a switched-off query reached the provider"
    );

    preferences.niutrans.enabled = true;
    update(handle, 2, &preferences);
    let query = read(msime_client_translation_query(handle))["value"].clone();
    assert_eq!(query["provider"], "niutrans");
    assert!(query["niutrans"].is_null() && query["tencent_tmt"].is_null());
    let received = forward(&query).unwrap();
    assert_eq!(received["query"]["provider"], "niutrans");
    assert!(received["query"].get("niutrans").is_none());

    preferences.niutrans.enabled = false;
    preferences.custom_translation.enabled = true;
    update(handle, 3, &preferences);
    let query = read(msime_client_translation_query(handle))["value"].clone();
    assert_eq!(query["provider"], "custom");
    assert!(query["custom_translation"].is_null());
    assert_eq!(forward(&query).unwrap()["query"]["provider"], "custom");
    assert_eq!(query["translation_account"], false);

    // Choosing the account switches every other service off, and the Linux provider socket receives
    // the explicit account flag so it can create or reuse the anonymous translation identity.
    preferences.custom_translation.enabled = false;
    preferences.translation_account = true;
    update(handle, 4, &preferences);
    let query = read(msime_client_translation_query(handle))["value"].clone();
    assert_eq!(query["translation_account"], true);
    assert_eq!(query["provider"], "account");
    let received = forward(&query).expect("an account query should reach the provider");
    assert_eq!(received["query"]["provider"], "account");
    assert_eq!(received["query"]["translation_account"], true);

    // Tencent's default `enabled: true` without usable secrets is not a user choice and does not displace the account; usable secrets do.
    preferences.tencent_tmt.enabled = true;
    update(handle, 5, &preferences);
    let query = read(msime_client_translation_query(handle))["value"].clone();
    assert_eq!(query["translation_account"], true);
    preferences.tencent_tmt.secret_id = "id".into();
    preferences.tencent_tmt.secret_key = "key".into();
    update(handle, 6, &preferences);
    let query = read(msime_client_translation_query(handle))["value"].clone();
    assert_eq!(query["translation_account"], false);
    preferences.tencent_tmt.enabled = false;

    // The user's own service wins over the account.
    preferences.niutrans.enabled = true;
    update(handle, 7, &preferences);
    let query = read(msime_client_translation_query(handle))["value"].clone();
    assert_eq!(query["translation_account"], false);

    // The offline English gloss keeps the query alive with candidate translations off, and must not carry the account.
    preferences.niutrans.enabled = false;
    preferences.candidate_translations = false;
    preferences.candidate_english_gloss = true;
    preferences.translation_target_language =
        msime_client_core::preferences::TranslationTargetLanguage::En;
    update(handle, 8, &preferences);
    let query = read(msime_client_translation_query(handle))["value"].clone();
    assert_eq!(query["english_gloss"], true);
    assert_eq!(query["translation_account"], false);
    read(msime_client_destroy(handle));
}

#[test]
fn translation_results_reject_control_characters_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    let mut view = Value::Null;
    for byte in b"U4e2d" {
        view = read(msime_client_character(
            handle,
            *byte,
            byte.is_ascii_uppercase(),
        ))["value"]["view"]
            .clone();
    }
    let generation = view["generation"].as_u64().unwrap();
    let candidate = view["candidates"][0]["text"].as_str().unwrap().to_owned();
    let apply = |values: Value| {
        let encoded = serde_json::to_vec(&values).unwrap();
        read(unsafe {
            msime_client_apply_translations(handle, generation, encoded.as_ptr(), encoded.len())
        })
    };

    let applied = apply(json!([{"text":candidate,"translation":"合成释义"}]));
    assert_eq!(applied["ok"], true);
    assert_eq!(applied["value"]["applied"], true);
    assert_eq!(
        applied["value"]["view"]["candidates"][0]["translation"],
        "合成释义"
    );
    let before = applied["value"]["view"].clone();

    for codepoint in (0..=0x1f).chain(0x7f..=0x9f) {
        let control = char::from_u32(codepoint).unwrap();
        for invalid in [
            json!([{"text":format!("{candidate}{control}"),"translation":"safe"}]),
            json!([{"text":candidate,"translation":format!("before{control}after")}]),
        ] {
            let rejected = apply(invalid);
            assert_eq!(rejected["ok"], false);
            assert_eq!(rejected["error"], "translation entries exceed limits");
        }
    }
    assert_eq!(read(msime_client_view(handle))["value"], before);
    read(msime_client_destroy(handle));
}

#[test]
fn translation_queries_follow_active_japanese_mode() {
    for temporary in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        if temporary {
            // The host disables the temporary Japanese shortcut when its
            // model resource is absent.  This test supplies a bounded
            // placeholder so it exercises the shortcut's generated kana
            // path without depending on a packaged model.
            std::fs::create_dir_all(dir.path().join("resources")).unwrap();
            std::fs::write(dir.path().join("resources/dict_japanese.dat"), b"synthetic").unwrap();
        }
        let preferences = Preferences {
            scheme: if temporary {
                InputScheme::Quanpin
            } else {
                InputScheme::Japanese
            },
            candidate_translations: true,
            ..chinese_preferences()
        };
        let handle = test_host_preferences(dir.path(), preferences.clone());
        read(msime_client_focus(handle, true));
        if temporary {
            read(msime_client_character(handle, b'R', true));
        }
        let view = read(msime_client_character(handle, b'a', false))["value"]["view"].clone();
        assert!(!view["candidates"].as_array().unwrap().is_empty());
        assert_eq!(view["local_mode"] == "temporary_japanese", temporary);
        assert_eq!(
            read(msime_client_translation_query(handle))["value"],
            Value::Null
        );
        read(msime_client_command(handle, 3));
        update(
            handle,
            1,
            &Preferences {
                scheme: InputScheme::Quanpin,
                ..preferences
            },
        );
        for byte in b"U4e2d" {
            read(msime_client_character(
                handle,
                *byte,
                byte.is_ascii_uppercase(),
            ));
        }
        let query = read(msime_client_translation_query(handle));
        assert!(query["value"]["candidates"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        read(msime_client_destroy(handle));
    }
}

#[test]
fn shuangpin_preedit_mode_is_applied_after_composition() {
    let dir = tempfile::tempdir().unwrap();
    let raw = Preferences {
        scheme: InputScheme::Shuangpin,
        ..chinese_preferences()
    };
    let handle = test_host_preferences(dir.path(), raw.clone());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'h', false));
    let before = read(msime_client_character(handle, b'k', false))["value"]["view"].clone();
    let expanded = Preferences {
        shuangpin_preedit_uses_raw: false,
        ..raw.clone()
    };
    let queued = update(handle, 1, &expanded);
    assert_eq!(queued["value"]["deferred"], true);
    assert_eq!(queued["value"]["view"], before);
    read(msime_client_command(handle, 3));
    assert_eq!(update(handle, 1, &expanded)["value"]["deferred"], false);
    read(msime_client_character(handle, b'h', false));
    let after = read(msime_client_character(handle, b'k', false))["value"]["view"].clone();
    assert_eq!(after["editing_text"], before["editing_text"]);
    assert_ne!(after["preedit"], before["preedit"]);
    read(msime_client_command(handle, 3));
    update(handle, 2, &raw);
    read(msime_client_character(handle, b'h', false));
    let restored = read(msime_client_character(handle, b'k', false));
    assert_eq!(restored["value"]["view"]["preedit"], before["preedit"]);
    msime_client_destroy(handle);
}

#[test]
fn shuangpin_profile_creation_and_deferred_replacement_use_real_engine() {
    let dir = tempfile::tempdir().unwrap();
    let microsoft = Preferences {
        scheme: InputScheme::Shuangpin,
        shuangpin_profile: ShuangpinProfile::Microsoft,
        ..chinese_preferences()
    };
    let handle = test_host_preferences(dir.path(), microsoft.clone());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'b', false));
    let first = read(msime_client_character(handle, b';', false));
    assert_eq!(first["value"]["view"]["editing_text"], "b;");
    let xiaohe = Preferences {
        shuangpin_profile: ShuangpinProfile::Xiaohe,
        ..microsoft.clone()
    };
    let before = read(msime_client_view(handle))["value"].clone();
    let queued = update(handle, 1, &xiaohe);
    assert_eq!(before["microsoft_shuangpin"], true);
    assert_eq!(before["shuangpin_profile"], "microsoft");
    assert_eq!(queued["value"]["deferred"], true);
    assert_eq!(queued["value"]["view"], before);
    // The old composition completes under Microsoft before replacing Engine.
    assert_eq!(
        read(msime_client_command(handle, 2))["value"]["commit"],
        "b;"
    );
    assert_eq!(update(handle, 1, &xiaohe)["value"]["deferred"], false);
    assert_eq!(
        read(msime_client_view(handle))["value"]["shuangpin_profile"],
        "xiaohe"
    );
    assert_eq!(
        read(msime_client_view(handle))["value"]["microsoft_shuangpin"],
        false
    );
    read(msime_client_character(handle, b'b', false));
    let replaced = read(msime_client_character(handle, b';', false));
    assert_ne!(replaced["value"]["view"]["editing_text"], "b;");
    read(msime_client_command(handle, 3));
    assert_eq!(update(handle, 2, &microsoft)["value"]["deferred"], false);
    read(msime_client_character(handle, b'b', false));
    assert_eq!(
        read(msime_client_character(handle, b';', false))["value"]["view"]["editing_text"],
        "b;"
    );
    read(msime_client_destroy(handle));
}
/// A lock to Chinese carries the punctuation switch with it, as the Windows host turns the switch on whenever punctuation is locked; unlocking hands the switch back.
#[test]
fn chinese_punctuation_lock_overrides_a_switched_off_punctuation_mode() {
    let comma =
        |handle| read(msime_client_punctuation_with_context(handle, b',', 0))["value"].clone();
    let dir = tempfile::tempdir().unwrap();
    let locked = test_host_preferences(
        dir.path(),
        Preferences {
            chinese_punctuation: false,
            punctuation_lock: msime_client_core::preferences::PunctuationLock::Chinese,
            ..chinese_preferences()
        },
    );
    read(msime_client_focus(locked, true));
    assert_eq!(comma(locked)["commit"], "，");
    read(msime_client_set_chinese_punctuation(locked, false));
    assert_eq!(comma(locked)["commit"], "，");
    read(msime_client_set_punctuation_lock(locked, 0));
    assert_ne!(comma(locked)["commit"], "，");
    read(msime_client_set_punctuation_lock(locked, 1));
    assert_eq!(comma(locked)["commit"], "，");
    read(msime_client_destroy(locked));

    let dir = tempfile::tempdir().unwrap();
    let follow = test_host_preferences(dir.path(), chinese_preferences());
    read(msime_client_focus(follow, true));
    read(msime_client_set_chinese_punctuation(follow, false));
    assert_ne!(comma(follow)["commit"], "，");
    read(msime_client_destroy(follow));
}

/// Windows keeps punctuation Chinese in English mode while it is locked to Chinese (`ResolvePunctuationOpen`), so a host that hands English-mode punctuation over under the lock must get the Chinese mark back.
#[test]
fn chinese_punctuation_lock_holds_in_english_mode() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host_preferences(
        dir.path(),
        Preferences {
            punctuation_lock: msime_client_core::preferences::PunctuationLock::Chinese,
            ..chinese_preferences()
        },
    );
    read(msime_client_focus(handle, true));
    read(msime_client_set_english_mode(handle, true));
    assert_eq!(
        read(msime_client_punctuation_with_context(handle, b',', 0))["value"]["commit"],
        "，"
    );
    read(msime_client_destroy(handle));
}

#[test]
fn explicit_punctuation_finishes_unicode_and_rejects_invalid_bytes() {
    for enabled in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let handle = test_host(dir.path());
        assert_eq!(read(msime_client_focus(handle, true))["ok"], true);
        read(msime_client_set_chinese_punctuation(handle, enabled));
        read(msime_client_character(handle, b'U', true));
        for byte in b"4e2d" {
            read(msime_client_character(handle, *byte, false));
        }
        let before = read(msime_client_view(handle));
        for invalid in [b'a', b' ', 0, 128, 255] {
            assert_eq!(read(msime_client_punctuation(handle, invalid))["ok"], false);
            assert_eq!(read(msime_client_view(handle)), before);
        }
        assert_eq!(
            std::thread::spawn(move || read(msime_client_punctuation(handle, b','))["ok"].clone())
                .join()
                .unwrap(),
            false
        );
        assert_eq!(read(msime_client_view(handle)), before);
        let result = read(msime_client_punctuation(handle, b','));
        assert_eq!(result["ok"], true);
        assert_eq!(result["value"]["handled"], true);
        assert_eq!(
            result["value"]["commit"],
            if enabled { "中，" } else { "中," }
        );
        assert_eq!(result["value"]["view"]["editing_text"], "");
        read(msime_client_destroy(handle));
        assert_eq!(read(msime_client_punctuation(handle, b','))["ok"], false);
    }
}

#[test]
fn contextual_punctuation_respects_editor_context_preferences_and_composition() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host_preferences(
        dir.path(),
        Preferences {
            // Stated rather than inherited: the default is `!cfg!(any(windows, target_os = "macos"))`, because the source ships the whole family off. Leaving it to the default made this a test that quietly asserted the opposite thing on those hosts - and passed wherever else it was run, since the suite only runs for the host target.
            smart_punctuation: true,
            smart_punctuation_direct_digit: true,
            smart_punctuation_direct_letter: true,
            ..chinese_preferences()
        },
    );
    assert_eq!(read(msime_client_focus(handle, true))["ok"], true);

    for preceding in [u32::from('0'), u32::from('a'), u32::from('Z')] {
        for punctuation in *b",.:" {
            let result = read(msime_client_punctuation_with_context(
                handle,
                punctuation,
                preceding,
            ));
            assert_eq!(result["ok"], true);
            assert_eq!(result["value"]["handled"], false);
        }
    }
    for preceding in [0, u32::from('中'), u32::from(' ')] {
        let result = read(msime_client_punctuation_with_context(
            handle, b',', preceding,
        ));
        assert_eq!(result["value"]["commit"], "，");
    }
    assert_eq!(
        read(msime_client_punctuation_with_context(
            handle,
            b'?',
            u32::from('a')
        ))["value"]["commit"],
        "？"
    );

    // Turning the two halves off is how a user asks for none of this, and then a comma after a digit
    // or a letter stays Chinese. They default on where the parent does - the parent's own
    // description promises exactly this conversion - so the document says so rather than relying on
    // a default that now points the other way.
    let plain = test_host_preferences(
        dir.path(),
        Preferences {
            smart_punctuation: true,
            smart_punctuation_direct_digit: false,
            smart_punctuation_direct_letter: false,
            ..chinese_preferences()
        },
    );
    assert_eq!(read(msime_client_focus(plain, true))["ok"], true);
    for preceding in [u32::from('0'), u32::from('a')] {
        assert_eq!(
            read(msime_client_punctuation_with_context(
                plain, b',', preceding
            ))["value"]["commit"],
            "，"
        );
    }

    read(msime_client_set_punctuation_lock(handle, 1));
    assert_eq!(
        read(msime_client_punctuation_with_context(
            handle,
            b',',
            u32::from('a')
        ))["value"]["commit"],
        "，"
    );
    read(msime_client_set_punctuation_lock(handle, 2));
    assert_eq!(
        read(msime_client_punctuation_with_context(
            handle,
            b',',
            u32::from('中')
        ))["value"]["handled"],
        false
    );
    read(msime_client_set_punctuation_lock(handle, 0));

    read(msime_client_character(handle, b'n', false));
    read(msime_client_character(handle, b'i', false));
    let composed = read(msime_client_punctuation_with_context(
        handle,
        b',',
        u32::from('a'),
    ));
    assert!(composed["value"]["commit"]
        .as_str()
        .is_some_and(|value| value.ends_with('，')));

    let preferences = Preferences {
        smart_punctuation: false,
        ..Preferences::default()
    };
    assert_eq!(update(handle, 1, &preferences)["value"]["deferred"], false);
    assert_eq!(
        read(msime_client_punctuation_with_context(
            handle,
            b'.',
            u32::from('7')
        ))["value"]["commit"],
        "。"
    );
    assert_eq!(
        read(msime_client_punctuation_with_context(
            handle,
            b'a',
            u32::from('7')
        ))["ok"],
        false
    );
    assert_eq!(
        read(msime_client_punctuation_with_context(handle, b',', 0xd800))["ok"],
        false
    );
    read(msime_client_destroy(handle));
}

#[test]
fn paired_book_title_auto_close_balance_is_narrow_and_owned() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    assert_eq!(read(msime_client_focus(handle, true))["ok"], true);
    assert_eq!(
        read(msime_client_punctuation(handle, b'<'))["value"]["commit"],
        "《"
    );
    let before = read(msime_client_view(handle));
    for invalid in [b'(', b'>', b'a', b' ', 0, 128, 255] {
        assert_eq!(
            read(msime_client_balance_paired_punctuation_after_auto_close(
                handle, invalid
            ))["ok"],
            false
        );
        assert_eq!(read(msime_client_view(handle)), before);
    }
    let balanced = read(msime_client_balance_paired_punctuation_after_auto_close(
        handle, b'<',
    ));
    assert_eq!(balanced["ok"], true);
    assert_eq!(balanced["value"], before["value"]);
    assert_eq!(
        read(msime_client_punctuation(handle, b'<'))["value"]["commit"],
        "《"
    );
    read(msime_client_balance_paired_punctuation_after_auto_close(
        handle, b'<',
    ));
    read(msime_client_destroy(handle));
    assert_eq!(
        read(msime_client_balance_paired_punctuation_after_auto_close(
            handle, b'<',
        ))["ok"],
        false
    );
}

#[test]
fn unpaired_punctuation_keeps_quote_alternation_and_book_title_nesting() {
    // With paired completion off (or in an excluded host) nothing supplies the closing half, so the Engine's own alternation and nesting are the only way to type it - the reference's GetPunctuation does both regardless of the setting. See `scripts/apply_engine_punctuation_alternation.py`.
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    assert_eq!(read(msime_client_focus(handle, true))["ok"], true);
    assert_eq!(
        read(msime_client_set_paired_punctuation(handle, false))["ok"],
        true
    );
    let marks = |keys: &[u8]| -> Vec<String> {
        keys.iter()
            .map(|&key| {
                read(msime_client_punctuation(handle, key))["value"]["commit"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    };
    assert_eq!(marks(b"\"\"\"\""), ["“", "”", "“", "”"]);
    assert_eq!(marks(b"''"), ["‘", "’"]);
    assert_eq!(marks(b"<<>>"), ["《", "〈", "〉", "》"]);
    // An unmatched closing mark leaves the depth at zero, so the next pair opens with 《 again.
    assert_eq!(marks(b"><>"), ["》", "《", "》"]);

    // The state belongs to the session, not to the setting: turning pairing on mid-quote does not reset it, as the reference's toggle is never reset by the switch either. Paired-on output is unchanged by the overlay.
    assert_eq!(marks(b"\"<"), ["“", "《"]);
    assert_eq!(
        read(msime_client_set_paired_punctuation(handle, true))["ok"],
        true
    );
    assert_eq!(marks(b"\"<>>"), ["”", "〈", "〉", "》"]);
    read(msime_client_destroy(handle));
}

#[test]
fn candidate_edge_uses_engine_han_text_and_preserves_unsupported_composition() {
    for (code, first, last) in [("4e2d", "中", "中"), ("20000", "𠀀", "𠀀"), ("41", "", "")] {
        for (edge, expected) in [(0, first), (1, last)] {
            let dir = tempfile::tempdir().unwrap();
            let handle = test_host(dir.path());
            read(msime_client_focus(handle, true));
            read(msime_client_character(handle, b'U', true));
            for byte in code.bytes() {
                read(msime_client_character(handle, byte, false));
            }
            let before = read(msime_client_view(handle))["value"].clone();
            assert!(
                before["candidates"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty()),
                "Missing Unicode fixture candidate for {code}: {before}"
            );
            let id = &before["candidates"][0]["id"];
            let generation = id["generation"].as_u64().unwrap();
            let index = id["index"].as_u64().unwrap() as usize;
            for invalid in [2, 255] {
                assert_eq!(
                    read(msime_client_select_edge(handle, generation, index, invalid))["ok"],
                    false
                );
                assert_eq!(read(msime_client_view(handle))["value"], before);
            }
            assert_eq!(
                read(msime_client_select_edge(
                    handle,
                    generation - 1,
                    index,
                    edge
                ))["ok"],
                false
            );
            assert_eq!(
                read(msime_client_select_edge(
                    handle,
                    generation,
                    usize::MAX,
                    edge
                ))["ok"],
                false
            );
            assert_eq!(
                std::thread::spawn(move || read(msime_client_select_edge(
                    handle, generation, index, edge
                ))["ok"]
                    .clone())
                .join()
                .unwrap(),
                false
            );
            assert_eq!(read(msime_client_view(handle))["value"], before);
            let result = read(msime_client_select_edge(handle, generation, index, edge));
            assert_eq!(result["ok"], true);
            assert_eq!(result["value"]["handled"], !expected.is_empty());
            if expected.is_empty() {
                assert!(result["value"]["commit"].is_null());
                assert_eq!(
                    result["value"]["view"]["editing_text"],
                    before["editing_text"]
                );
                assert_eq!(
                    result["value"]["view"]["candidates"][0]["text"],
                    before["candidates"][0]["text"]
                );
            } else {
                assert_eq!(result["value"]["commit"], expected);
                assert_eq!(result["value"]["view"]["editing_text"], "");
                assert!(result["value"]["view"]["candidates"]
                    .as_array()
                    .unwrap()
                    .is_empty());
            }
            assert_eq!(
                read(msime_client_select_edge(handle, generation, index, edge))["ok"],
                false
            );
            read(msime_client_destroy(handle));
            assert_eq!(
                read(msime_client_select_edge(handle, generation, index, edge))["ok"],
                false
            );
        }
    }
}

#[test]
fn live_punctuation_preserves_composition_and_survives_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'U', true));
    for byte in b"4e2d" {
        read(msime_client_character(handle, *byte, false));
    }
    let before = read(msime_client_view(handle))["value"].clone();
    for _ in 0..2 {
        let toggled = read(msime_client_set_chinese_punctuation(handle, false));
        assert_eq!(toggled["value"], before);
    }
    let preferences = Preferences {
        candidate_page_size: 2,
        ..Preferences::default()
    };
    assert_eq!(update(handle, 1, &preferences)["value"]["deferred"], true);
    let committed = read(msime_client_command(handle, 9));
    assert_eq!(committed["value"]["commit"], "中");
    assert_eq!(update(handle, 1, &preferences)["value"]["deferred"], false);
    let ascii = read(msime_client_character(handle, b',', false));
    assert_eq!(ascii["value"]["handled"], false);
    assert!(ascii["value"]["commit"].is_null());
    assert_eq!(
        read(msime_client_set_chinese_punctuation(handle, true))["ok"],
        true
    );
    assert_eq!(
        read(msime_client_character(handle, b',', false))["value"]["commit"],
        "，"
    );
    assert_eq!(
        std::thread::spawn(
            move || read(msime_client_set_chinese_punctuation(handle, false))["ok"].clone()
        )
        .join()
        .unwrap(),
        false
    );
    read(msime_client_destroy(handle));
    assert_eq!(
        read(msime_client_set_chinese_punctuation(handle, true))["ok"],
        false
    );
}

#[test]
fn dedicated_english_mode_switches_through_host_api() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    let enabled = read(msime_client_set_english_mode(handle, true));
    assert_eq!(enabled["ok"], true);
    assert_eq!(enabled["value"]["focused"], true);
    assert_eq!(enabled["value"]["dedicated_english"], true);
    let typed = read(msime_client_character(handle, b'a', false));
    assert_eq!(typed["value"]["view"]["dedicated_english"], true);
    assert_eq!(typed["value"]["view"]["local_mode"], "none");
    let disabled = read(msime_client_set_english_mode(handle, false));
    assert_eq!(disabled["ok"], true);
    assert_eq!(disabled["value"]["dedicated_english"], false);
    read(msime_client_destroy(handle));
}

#[test]
fn enabling_dedicated_english_cancels_active_composition() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'n', false));
    let composing = read(msime_client_character(handle, b'i', false));
    assert!(!composing["value"]["view"]["editing_text"]
        .as_str()
        .unwrap()
        .is_empty());
    let enabled = read(msime_client_set_english_mode(handle, true));
    assert_eq!(enabled["value"]["dedicated_english"], true);
    assert_eq!(enabled["value"]["editing_text"], "");
    assert!(enabled["value"]["candidates"]
        .as_array()
        .unwrap()
        .is_empty());
    read(msime_client_destroy(handle));
}

// 默认输入状态 = 英文 is the host's passthrough state: the host keeps the
// letters and no session sees them. It must not put the session itself
// into dedicated English. A session that starts there answers the first
// key with English word candidates, and the host's own CN/EN toggle does
// not clear it - so the toggle flips between passthrough English and
// English candidates, and Chinese is unreachable.
#[test]
fn english_default_ime_mode_leaves_dedicated_english_off() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host_preferences(
        dir.path(),
        Preferences {
            default_ime_mode: msime_client_core::preferences::DefaultImeMode::English,
            ..Preferences::default()
        },
    );
    read(msime_client_focus(handle, true));
    assert_eq!(
        read(msime_client_view(handle))["value"]["dedicated_english"],
        false
    );
    let typed = read(msime_client_character(handle, b'n', false));
    assert_eq!(typed["value"]["view"]["dedicated_english"], false);
    // Still reachable - it just has to be asked for, by the menu row or
    // the hotkey that owns it.
    let enabled = read(msime_client_set_english_mode(handle, true));
    assert_eq!(enabled["value"]["dedicated_english"], true);
    read(msime_client_destroy(handle));
}

#[test]
fn preferences_wait_for_commit_keep_handle_and_reject_old_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'U', true));
    for byte in b"4e2d" {
        read(msime_client_character(handle, *byte, false));
    }
    let before = read(msime_client_view(handle))["value"].clone();
    let prefs = Preferences {
        chinese_punctuation: false,
        candidate_page_size: 2,
        ..Preferences::default()
    };
    let queued = update(handle, 1, &prefs);
    assert_eq!(queued["value"]["deferred"], true);
    assert_eq!(queued["value"]["view"], before);
    let committed = read(msime_client_command(handle, 1));
    assert_eq!(committed["value"]["commit"], "中");
    assert_eq!(committed["value"]["view"]["session"], handle);
    assert_eq!(committed["value"]["view"]["focused"], true);
    assert_eq!(update(handle, 1, &prefs)["value"]["deferred"], false);
    assert_eq!(
        read(msime_client_character(handle, b',', false))["value"]["handled"],
        false
    );
    assert_eq!(update(handle, 0, &prefs)["ok"], false);
    assert_eq!(update(handle, 1, &Preferences::default())["ok"], false);
    let generation = before["generation"].as_u64().unwrap();
    assert_eq!(
        read(msime_client_select(handle, generation, 0))["ok"],
        false
    );
    read(msime_client_destroy(handle));
}
#[test]
fn newest_pending_preferences_win_on_blur_and_invalid_values_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'U', true));
    let off = Preferences {
        chinese_punctuation: false,
        ..Preferences::default()
    };
    assert_eq!(update(handle, 1, &off)["value"]["deferred"], true);
    let invalid = Preferences {
        candidate_page_size: 0,
        ..off.clone()
    };
    assert_eq!(update(handle, 20, &invalid)["ok"], false);
    assert_eq!(update(handle, 2, &Preferences::default())["ok"], true);
    read(msime_client_focus(handle, false));
    read(msime_client_focus(handle, true));
    assert_eq!(
        read(msime_client_character(handle, b',', false))["value"]["commit"],
        "，"
    );
    let wrong = std::thread::spawn(move || update(handle, 3, &Preferences::default()))
        .join()
        .unwrap();
    assert_eq!(wrong["ok"], false);
    assert_eq!(
        read(unsafe { msime_client_update_preferences(handle, std::ptr::null(), 0) })["ok"],
        false
    );
    read(msime_client_destroy(handle));
}
#[test]
fn failed_rebuild_preserves_completed_input_and_retries_later() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'U', true));
    for byte in b"4e2d" {
        read(msime_client_character(handle, *byte, false));
    }
    let prefs = Preferences {
        chinese_punctuation: false,
        ..Preferences::default()
    };
    update(handle, 1, &prefs);
    // Inject invalid replacement options without touching the live Engine or disk.
    let original = SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let host = sessions.get_mut(&handle).unwrap();
        std::mem::replace(&mut host.options.resources, "relative".into())
    });
    let committed = read(msime_client_command(handle, 1));
    assert_eq!(committed["value"]["commit"], "中");
    assert!(committed["value"]["diagnostic"]
        .as_str()
        .unwrap()
        .contains("Preferences update deferred"));
    SESSIONS.with(|sessions| {
        sessions
            .borrow_mut()
            .get_mut(&handle)
            .unwrap()
            .options
            .resources = original
    });
    assert_eq!(update(handle, 1, &prefs)["value"]["deferred"], false);
    assert_eq!(
        read(msime_client_character(handle, b',', false))["value"]["handled"],
        false
    );
    read(msime_client_destroy(handle));
}
/// The contract the input hosts' dictionary-maintenance release relies on: a live session is what keeps maintenance out, destroying it is all it takes to let maintenance in, and a session asked for while maintenance runs is refused rather than queued.
#[test]
fn a_session_and_dictionary_maintenance_exclude_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let path = |name| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let (user, dictionaries) = (path("user"), path("dictionaries"));
    let options = json!({ "api_version": 1, "resources": path("resources"), "user_data": user, "cache": path("cache"), "dictionaries": dictionaries, "preferences": { "scheme": "quanpin", "default_ime_mode": "chinese", "candidate_page_size": 5, "learning": false, "chinese_punctuation": true } }).to_string();
    let create = || read(unsafe { msime_client_create(options.as_ptr(), options.len()) });

    let maintenance = DictionaryAccess::try_maintenance(&user, &dictionaries)
        .unwrap()
        .unwrap();
    let refused = create();
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"], "dictionary maintenance busy");
    drop(maintenance);

    let created = create();
    assert_eq!(created["ok"], true, "{created}");
    let handle = created["value"]["session"].as_u64().unwrap();
    assert!(DictionaryAccess::try_maintenance(&user, &dictionaries)
        .unwrap()
        .is_none());
    assert_eq!(read(msime_client_destroy(handle))["ok"], true);
    // Other tests spawn processes concurrently, and a fork can briefly inherit the released lock before close-on-exec runs.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while DictionaryAccess::try_maintenance(&user, &dictionaries)
        .unwrap()
        .is_none()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "a destroyed session kept dictionary maintenance out"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
pub(super) fn read(pointer: *mut c_char) -> Value {
    // SAFETY: all callers pass a fresh response allocation.
    let string = unsafe { CString::from_raw(pointer) };
    serde_json::from_slice(string.as_bytes()).unwrap()
}
#[test]
fn native_boundary_drives_real_engine_and_rejects_wrong_thread() {
    let dir = tempfile::tempdir().unwrap();
    let path = |name| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let options = json!({ "api_version": 1, "resources": path("resources"), "user_data": path("user"), "cache": path("cache"), "dictionaries": path("dictionaries"), "preferences": { "scheme": "quanpin", "default_ime_mode": "chinese", "candidate_page_size": 5, "learning": false, "chinese_punctuation": true } }).to_string();
    let created = read(unsafe { msime_client_create(options.as_ptr(), options.len()) });
    assert_eq!(created["ok"], true, "{created}");
    let handle = created["value"]["session"].as_u64().unwrap();
    let wrong_thread = std::thread::spawn(move || read(msime_client_view(handle)))
        .join()
        .unwrap();
    assert_eq!(wrong_thread["ok"], false);
    assert_eq!(read(msime_client_focus(handle, true))["ok"], true);
    read(msime_client_character(handle, b'U', true));
    for byte in b"4e2d" {
        assert_eq!(
            read(msime_client_character(handle, *byte, false))["ok"],
            true
        );
    }
    let result = read(msime_client_command(handle, 1));
    assert_eq!(result["value"]["commit"], "中");
    let punctuation = read(msime_client_character(handle, b',', false));
    assert_eq!(punctuation["ok"], true);
    assert_eq!(punctuation["value"]["handled"], true);
    assert_eq!(punctuation["value"]["commit"], "，");
    assert_eq!(read(msime_client_destroy(handle))["ok"], true);
    assert_eq!(read(msime_client_view(handle))["ok"], false);
    assert_eq!(read(msime_client_destroy(handle))["ok"], false);
}
#[test]
fn cloud_response_boundary_guards_identity_permission_and_buffers() {
    let dir = tempfile::tempdir().unwrap();
    let preferences = Preferences {
        scheme: InputScheme::Quanpin,
        cloud_candidates: true,
        ..chinese_preferences()
    };
    let handle = test_host_with_pinyin_fixture(dir.path(), preferences.clone());
    read(msime_client_focus(handle, true));
    assert!(read(msime_client_online_query(handle))["value"].is_null());
    for byte in b"nihao" {
        read(msime_client_character(handle, *byte, false));
    }
    let query = read(msime_client_online_query(handle))["value"].to_string();
    let body = r#"["SUCCESS", [["nihao", ["你好"]]]]"#.as_bytes();
    let apply = |target, query: &str, body: &[u8]| {
        read(unsafe {
            msime_client_apply_cloud_response(
                target,
                query.as_ptr(),
                query.len(),
                body.as_ptr(),
                body.len(),
            )
        })
    };
    let before = read(msime_client_view(handle))["value"].clone();
    assert!(!before["candidates"].as_array().unwrap().is_empty());
    for candidate in ["", "bad\nvalue"] {
        let result = read(unsafe {
            msime_client_apply_online_candidate(
                handle,
                query.as_ptr(),
                query.len(),
                candidate.as_ptr(),
                candidate.len(),
                0,
            )
        });
        assert_eq!(result["value"]["applied"], false);
        assert_eq!(result["value"]["view"], before);
    }
    let malformed = apply(handle, &query, b"not json");
    assert_eq!(malformed["value"]["applied"], false);
    assert_eq!(malformed["value"]["view"], before);
    assert_eq!(apply(handle, &query, body)["value"]["applied"], true);
    let after_cloud = read(msime_client_view(handle))["value"].clone();
    assert_ne!(after_cloud["generation"], before["generation"]);
    assert_eq!(after_cloud["editing_text"], before["editing_text"]);
    let other_dir = tempfile::tempdir().unwrap();
    let other = test_host_preferences(other_dir.path(), preferences.clone());
    read(msime_client_focus(other, true));
    for byte in b"nihao" {
        read(msime_client_character(other, *byte, false));
    }
    assert_eq!(apply(other, &query, body)["value"]["applied"], false);
    read(msime_client_character(handle, b'a', false));
    assert_eq!(apply(handle, &query, body)["value"]["applied"], false);
    let current = read(msime_client_online_query(handle))["value"].to_string();
    let disabled = Preferences {
        cloud_candidates: false,
        ..preferences
    };
    assert_eq!(update(handle, 1, &disabled)["value"]["deferred"], true);
    assert_eq!(
        read(msime_client_online_query(handle))["value"]["cloud_candidates"],
        false
    );
    assert_eq!(apply(handle, &current, body)["value"]["applied"], false);
    for (q, qlen, b, blen) in [
        (std::ptr::null(), 0, body.as_ptr(), body.len()),
        (query.as_ptr(), query.len(), std::ptr::null(), 0),
        (query.as_ptr(), 16385, body.as_ptr(), body.len()),
        (query.as_ptr(), query.len(), body.as_ptr(), 262145),
    ] {
        assert_eq!(
            read(unsafe { msime_client_apply_cloud_response(handle, q, qlen, b, blen) })["ok"],
            false
        );
    }
    assert_eq!(
        apply(handle, "invalid", body)["error"],
        "invalid online query document"
    );
    read(msime_client_destroy(other));
    read(msime_client_destroy(handle));
}

#[test]
fn cloud_candidate_requires_an_existing_local_candidate_page() {
    let dir = tempfile::tempdir().unwrap();
    let preferences = Preferences {
        scheme: InputScheme::Quanpin,
        cloud_candidates: true,
        ..chinese_preferences()
    };
    let handle = test_host_preferences(dir.path(), preferences);
    read(msime_client_focus(handle, true));
    for byte in b"nihao" {
        read(msime_client_character(handle, *byte, false));
    }
    let query = read(msime_client_online_query(handle))["value"].to_string();
    let before = read(msime_client_view(handle))["value"].clone();
    assert!(before["candidates"].as_array().unwrap().is_empty());
    let candidate = "你好";
    let result = read(unsafe {
        msime_client_apply_online_candidate(
            handle,
            query.as_ptr(),
            query.len(),
            candidate.as_ptr(),
            candidate.len(),
            0,
        )
    });
    assert_eq!(result["value"]["applied"], false);
    assert_eq!(result["value"]["view"], before);
    read(msime_client_destroy(handle));
}

#[test]
fn an_ai_credential_handed_over_in_memory_signs_requests_without_being_stored() {
    let dir = tempfile::tempdir().unwrap();
    let mut preferences = Preferences {
        scheme: InputScheme::Quanpin,
        ..chinese_preferences()
    };
    preferences.ai_assistant.enabled = true;
    preferences.ai_assistant.provider = "deepseek".into();
    preferences.ai_assistant.model = "synthetic-model".into();
    preferences.ai_assistant.endpoint = "https://api.deepseek.com/chat/completions".into();
    let handle = test_host_preferences(dir.path(), preferences);
    read(msime_client_focus(handle, true));
    for byte in b"nihao" {
        read(msime_client_character(handle, *byte, false));
    }
    let query = read(msime_client_online_query(handle))["value"].to_string();
    let request = |handle: u64| {
        read(unsafe { msime_client_ai_request_for_query(handle, query.as_ptr(), query.len()) })
    };
    let set = |token: &str| {
        read(unsafe { msime_client_set_ai_credential(handle, token.as_ptr(), token.len()) })
    };
    assert_ne!(request(handle)["ok"], true, "no token anywhere");
    assert_eq!(set("synthetic-keychain")["ok"], true);
    let descriptor = request(handle);
    assert_eq!(descriptor["ok"], true);
    assert_eq!(
        descriptor["value"]["headers"]["Authorization"],
        "Bearer synthetic-keychain"
    );
    assert!(!read(msime_client_view(handle))
        .to_string()
        .contains("synthetic-keychain"));
    assert!(!read(msime_client_online_query(handle))
        .to_string()
        .contains("synthetic-keychain"));
    assert_eq!(set("bad\ntoken")["ok"], false);
    assert_eq!(set("")["ok"], true);
    assert_ne!(request(handle)["ok"], true, "cleared");
    read(msime_client_destroy(handle));
}

#[test]
fn ai_queries_and_delivery_follow_pending_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let mut preferences = Preferences {
        scheme: InputScheme::Quanpin,
        ..chinese_preferences()
    };
    preferences.ai_assistant.enabled = true;
    preferences.ai_assistant.model = "synthetic-original".into();
    preferences.ai_assistant.token = "synthetic-private".into();
    // An enabled assistant with no endpoint has nowhere to send anything;
    // a real one is always configured with the provider's URL.
    preferences.ai_assistant.endpoint = "https://api.deepseek.com/chat/completions".into();
    let handle = test_host_preferences(dir.path(), preferences.clone());
    read(msime_client_focus(handle, true));
    for byte in b"nihaoshijie" {
        read(msime_client_character(handle, *byte, false));
    }
    let apply = |query: &Value, batch: bool| {
        let query = query.to_string();
        if batch {
            let candidates = serde_json::to_vec(&json!(["合成候选"])).unwrap();
            read(unsafe {
                msime_client_apply_online_candidates(
                    handle,
                    query.as_ptr(),
                    query.len(),
                    candidates.as_ptr(),
                    candidates.len(),
                    1,
                )
            })
        } else {
            let candidate = "合成候选".as_bytes();
            read(unsafe {
                msime_client_apply_online_candidate(
                    handle,
                    query.as_ptr(),
                    query.len(),
                    candidate.as_ptr(),
                    candidate.len(),
                    1,
                )
            })
        }
    };
    let original = read(msime_client_online_query(handle))["value"].clone();
    assert_eq!(original["ai_eligible"], true);
    assert!(!original.to_string().contains("synthetic-private"));
    let original_bytes = original.to_string();
    let descriptor = read(unsafe {
        msime_client_ai_request_for_query(handle, original_bytes.as_ptr(), original_bytes.len())
    });
    assert_eq!(descriptor["ok"], true);
    assert_eq!(descriptor["value"]["method"], "POST");
    assert_eq!(
        descriptor["value"]["headers"]["Content-Type"],
        "application/json"
    );
    for revision in 1..=5 {
        let old = read(msime_client_online_query(handle))["value"].clone();
        match revision {
            1 => preferences.ai_assistant.enabled = false,
            2 => {
                preferences.ai_assistant.enabled = true;
                preferences.ai_assistant.model = "synthetic-new".into();
            }
            3 => {
                preferences.ai_assistant.endpoint =
                    "https://synthetic.invalid/v1/chat/completions".into()
            }
            4 => preferences.ai_assistant.prompt = "synthetic prompt".into(),
            _ => preferences.ai_assistant.candidate_limit = 1,
        }
        assert_eq!(
            update(handle, revision, &preferences)["value"]["deferred"],
            true
        );
        let current = read(msime_client_online_query(handle))["value"].clone();
        assert_eq!(current["generation"], original["generation"]);
        assert_eq!(current["query_text"], original["query_text"]);
        assert_eq!(
            current["ai_assistant"].is_null(),
            !preferences.ai_assistant.enabled
        );
        if preferences.ai_assistant.enabled {
            assert_eq!(
                current["ai_assistant"]["model"],
                preferences.ai_assistant.model
            );
            assert_eq!(
                current["ai_assistant"]["endpoint"],
                preferences.ai_assistant.endpoint
            );
            assert_eq!(
                current["ai_assistant"]["prompt"],
                preferences.ai_assistant.prompt
            );
            assert_eq!(
                current["ai_assistant"]["candidate_limit"],
                preferences.ai_assistant.candidate_limit
            );
        }
        for batch in [false, true] {
            assert_eq!(apply(&old, batch)["value"]["applied"], false);
        }
    }
    let current = read(msime_client_online_query(handle))["value"].clone();
    let before_ai = read(msime_client_view(handle))["value"].clone();
    assert_eq!(apply(&current, true)["value"]["applied"], true);
    let after_ai = read(msime_client_view(handle))["value"].clone();
    assert_ne!(after_ai["generation"], before_ai["generation"]);
    read(msime_client_destroy(handle));
}
#[test]
fn custom_translation_plan_preserves_direction_and_filters_visible_sources() {
    let plan = |request: Value| {
        let bytes = serde_json::to_vec(&request).unwrap();
        read(unsafe { msime_client_custom_translation_plan(bytes.as_ptr(), bytes.len()) })
    };
    let candidates = json!([
        {"text":"Hello","source":4},
        {"text":"测试","source":0},
        {"text":"Hello","source":4},
        {"text":"smile","source":6},
        {"text":"smile","source":7},
        {"text":"123","source":0},
        {"text":"test😀","source":0},
        {"text":"x".repeat(41),"source":0},
        {"text":"unknown","source":10}
    ]);
    for target in ["en", "fr", "ja", "es", "ru", "de", "ko"] {
        assert_eq!(
            plan(json!({"target_language":target,"candidates":candidates}))["value"],
            json!([
                {"text":"Hello","key":"hello","source_language":"en","target_language":"zh"},
                {"text":"测试","key":"测试","source_language":"zh","target_language":target}
            ])
        );
    }
    for request in [
        json!({"target_language":"unknown","candidates":[]}),
        json!({"target_language":"en","candidates":vec![json!({"text":"hello","source":0}); 10]}),
        json!({"target_language":"en","candidates":[{"text":"hello","source":true}]}),
    ] {
        assert_eq!(plan(request)["ok"], false);
    }
    assert_eq!(
        read(unsafe { msime_client_custom_translation_plan(std::ptr::null(), 0) })["ok"],
        false
    );
}
#[test]
fn mixed_script_chinese_candidates_are_planned_and_saved_like_the_windows_source() {
    // Windows `IsCloudTranslatableChinese` only needs one Han codepoint and no emoji, so words such as 卡拉OK and T恤 are translated zh->target and their English gloss is learned.
    let bytes = serde_json::to_vec(&json!({
        "target_language": "en",
        "candidates": [
            {"text":"卡拉OK","source":0},
            {"text":"T恤","source":0},
            {"text":"T恤😀","source":0},
        ],
    }))
    .unwrap();
    assert_eq!(
        read(unsafe { msime_client_custom_translation_plan(bytes.as_ptr(), bytes.len()) })["value"],
        json!([
            {"text":"卡拉OK","key":"卡拉OK","source_language":"zh","target_language":"en"},
            {"text":"T恤","key":"T恤","source_language":"zh","target_language":"en"}
        ])
    );
    let user = tempfile::tempdir().unwrap();
    let user_path = user.path().to_str().unwrap();
    let request = serde_json::to_vec(&json!({
        "target_language": "en",
        "translations": [{"text":"卡拉OK","translation":"karaoke"}],
    }))
    .unwrap();
    let saved = read(unsafe {
        msime_client_translation_gloss_save(
            request.as_ptr(),
            request.len(),
            user_path.as_ptr(),
            user_path.len(),
        )
    });
    assert_eq!(saved["value"]["saved"], 1);
    let database = rusqlite::Connection::open(user.path().join("translation-glosses.db")).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT english_gloss FROM zh_en_glosses WHERE chinese='卡拉OK'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "karaoke"
    );
}
#[test]
fn learned_translation_buffers_are_bounded() {
    assert_eq!(
        read(unsafe { msime_client_ai_http_request(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_ai_http_request(b"x".as_ptr(), 65537) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_parse_ai_response(b"x".as_ptr(), 1048577, 1) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_parse_ai_response(b"x".as_ptr(), 1, 11) })["ok"],
        false
    );
    for (pointer, length) in [(std::ptr::null(), 0), (b"x".as_ptr(), 65537)] {
        assert_eq!(
            read(unsafe { msime_client_learned_translation_request(pointer, length) })["ok"],
            false
        );
    }
}
#[test]
fn tencent_translation_buffers_are_bounded() {
    assert_eq!(
        read(unsafe { msime_client_tencent_translation_http_request(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_tencent_translation_http_request(b"x".as_ptr(), 65537) })["ok"],
        false
    );
    for (length, expected) in [(1048577, 1), (1, 0), (1, 10)] {
        assert_eq!(
            read(unsafe {
                msime_client_parse_tencent_translation_response(b"x".as_ptr(), length, expected)
            })["ok"],
            false
        );
    }
}
#[test]
fn custom_translation_http_bridge_is_bounded_and_pure() {
    let build = |request: Value| {
        let bytes = serde_json::to_vec(&request).unwrap();
        read(unsafe { msime_client_custom_translation_http_request(bytes.as_ptr(), bytes.len()) })
    };
    let request = json!({"config":{"enabled":true,"endpoint":"https://translation.invalid/api","api_key":"synthetic"},
        "text":"hello","source_language":"en","target_language":"zh"});
    let value = build(request.clone());
    assert_eq!(value["ok"], true);
    assert_eq!(value["value"]["method"], "POST");
    assert_eq!(
        value["value"]["headers"]["Authorization"],
        "Bearer synthetic"
    );
    assert_eq!(
        value["value"]["body"],
        json!({"text":"hello","source_lang":"EN","target_lang":"ZH"})
    );
    assert_eq!(value["value"]["timeout_ms"], 2500);
    assert_eq!(value["value"]["max_response_bytes"], 1048576);
    let mut padded = request.clone();
    padded["config"]["endpoint"] = json!("  https://translation.invalid/api  ");
    padded["config"]["api_key"] = json!("  synthetic  ");
    let padded_value = build(padded);
    assert_eq!(
        padded_value["value"]["url"],
        "https://translation.invalid/api"
    );
    assert_eq!(
        padded_value["value"]["headers"]["Authorization"],
        "Bearer synthetic"
    );
    let mut disabled = request.clone();
    disabled["config"]["enabled"] = json!(false);
    assert!(build(disabled)["value"].is_null());
    let mut keyless = request.clone();
    keyless["config"]["api_key"] = json!("");
    assert!(build(keyless)["value"]["headers"]
        .get("Authorization")
        .is_none());
    for (field, value) in [
        ("text", "x".repeat(41)),
        ("source_language", "en\r\n".into()),
    ] {
        let mut invalid = request.clone();
        invalid[field] = json!(value);
        assert_eq!(
            build(invalid)["error"],
            "invalid custom translation parameters"
        );
    }
    for codepoint in (0..=0x1f).chain(0x7f..=0x9f) {
        let control = char::from_u32(codepoint).unwrap();
        let mut invalid = request.clone();
        invalid["text"] = json!(format!("before{control}after"));
        assert_eq!(
            build(invalid)["error"],
            "invalid custom translation parameters"
        );
    }
    for (field, value) in [
        ("endpoint", "file:///synthetic"),
        ("api_key", "synthetic\r\nheader"),
    ] {
        let mut invalid = request.clone();
        invalid["config"][field] = json!(value);
        assert_eq!(
            build(invalid)["error"],
            "invalid custom translation parameters"
        );
    }
    let parse = |body: &[u8]| {
        read(unsafe { msime_client_parse_custom_translation_response(body.as_ptr(), body.len()) })
    };
    assert_eq!(parse(br#"{"data":"translated"}"#)["value"], "translated");
    assert_eq!(
        parse(br#"{"data":"  hello\nworld\t "}"#)["value"],
        "hello world"
    );
    assert!(parse(br#"{"data":"bad\u0000gloss"}"#)["value"].is_null());
    for body in [
        b"invalid".as_slice(),
        br#"{"code":500,"data":"ignored"}"#,
        b"\xff",
    ] {
        assert!(parse(body)["value"].is_null());
    }
    assert!(parse(json!({"data":"x".repeat(4097)}).to_string().as_bytes())["value"].is_null());
    assert_eq!(
        read(unsafe { msime_client_custom_translation_http_request(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_parse_custom_translation_response(b"x".as_ptr(), 1048577) })
            ["ok"],
        false
    );
}
#[test]
fn invalid_buffers_and_commands_return_owned_errors() {
    assert_eq!(
        read(unsafe { msime_client_prepare_host(std::ptr::null(), 0) })["ok"],
        false
    );
    let invalid = br#"{"resources":"relative","state_root":"relative"}"#;
    assert_eq!(
        read(unsafe { msime_client_prepare_host(invalid.as_ptr(), invalid.len()) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_create(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(read(msime_client_command(0, 999))["ok"], false);
    unsafe { msime_client_string_free(std::ptr::null_mut()) };
}

#[test]
fn incomplete_local_mode_input_shows_the_raw_text_as_a_fallback_space_commits() {
    // Windows' PrepareCandidateList shows the raw composition as a Fallback row whenever a special mode has nothing else, and Space commits it; a bare Y or R prefix included.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("resources")).unwrap();
    std::fs::write(dir.path().join("resources/english.db"), b"fixture").unwrap();
    std::fs::write(dir.path().join("resources/dict_japanese.dat"), b"synthetic").unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    let only_fallback = |transition: &Value, text: &str| {
        let view = &transition["value"]["view"];
        let candidates = view["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 1, "{view}");
        assert_eq!(candidates[0]["text"], text, "{view}");
        assert_eq!(candidates[0]["source"], 9, "{view}");
    };
    let typed = |text: &[u8]| {
        let mut last = Value::Null;
        for byte in text {
            last = read(msime_client_character(
                handle,
                *byte,
                byte.is_ascii_uppercase(),
            ));
        }
        last
    };

    only_fallback(&typed(b"Y"), "Y");
    let committed = read(msime_client_command(handle, 1));
    assert_eq!(committed["value"]["commit"], "Y");
    assert!(committed["value"]["view"]["candidates"]
        .as_array()
        .unwrap()
        .is_empty());

    only_fallback(&typed(b"Kzzz"), "Kzzz");
    read(msime_client_command(handle, 3));

    only_fallback(&typed(b"U+"), "U+");
    read(msime_client_command(handle, 3));

    only_fallback(&typed(b"Txin"), "Txin");
    assert_eq!(
        read(msime_client_command(handle, 1))["value"]["commit"],
        "Txin"
    );

    only_fallback(&typed(b"R"), "R");
    assert_eq!(
        read(msime_client_command(handle, 1))["value"]["commit"],
        "R"
    );
    assert_eq!(read(msime_client_destroy(handle))["ok"], true);
}

#[test]
fn candidate_page_edge_commands_reach_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    for byte in b"nihao" {
        read(msime_client_character(handle, *byte, false));
    }
    let first = read(msime_client_command(handle, 104));
    assert_eq!(first["value"]["handled"], false);
    assert!(first["value"]["view"]["candidates"]
        .as_array()
        .unwrap()
        .is_empty());
    let last = read(msime_client_command(handle, 105));
    assert_eq!(last["value"]["handled"], false);
    assert_eq!(read(msime_client_destroy(handle))["ok"], true);
}

#[test]
fn complete_candidate_abi_keeps_view_paged_and_selects_a_later_entry() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_host(dir.path());
    read(msime_client_focus(handle, true));
    read(msime_client_character(handle, b'T', true));
    read(msime_client_character(handle, b'r', false));
    let transition = read(msime_client_character(handle, b'q', false));
    let view = &transition["value"]["view"];
    let generation = view["generation"].as_u64().unwrap();
    let visible_count = view["candidates"].as_array().unwrap().len();
    assert!(visible_count > 0);

    let complete = read(msime_client_all_candidates(handle));
    assert_eq!(complete["ok"], true);
    assert_eq!(complete["value"]["session"], handle);
    assert_eq!(complete["value"]["generation"], generation);
    assert_eq!(complete["value"]["preedit"], "Trq");
    let complete_count = complete["value"]["candidates"].as_array().unwrap().len();
    assert!(complete_count > visible_count);
    let later = complete["value"]["candidates"][visible_count]["id"]["index"]
        .as_u64()
        .unwrap() as usize;

    assert_eq!(
        read(msime_client_select(handle, generation, later))["ok"],
        false
    );
    assert_eq!(
        read(msime_client_select_any_candidate(
            handle,
            generation + 1,
            later
        ))["ok"],
        false
    );
    assert_eq!(
        read(msime_client_select_any_candidate(
            handle,
            generation,
            complete_count
        ))["ok"],
        false
    );
    let selected = read(msime_client_select_any_candidate(handle, generation, later));
    assert_eq!(selected["ok"], true);
    assert_eq!(selected["value"]["handled"], true);
    assert!(selected["value"]["commit"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(read(msime_client_destroy(handle))["ok"], true);
}

#[test]
#[cfg(unix)]
fn emoji_catalog_pagination_preserves_legacy_defaults() {
    let legacy: crate::ffi::EmojiCatalogQuery = serde_json::from_str("{}").unwrap();
    assert!(!legacy.cursor);
    assert_eq!(legacy.offset, 0);
    assert_eq!(legacy.panel.limit, 48);
    let page: crate::ffi::EmojiCatalogQuery = serde_json::from_str(
        r#"{"search":"synthetic","category":"symbols","offset":510,"limit":255}"#,
    )
    .unwrap();
    assert_eq!(page.offset, 510);
    assert_eq!(page.panel.search, "synthetic");
    assert_eq!(page.panel.category, "symbols");
    assert_eq!(page.panel.limit, 255);
    assert!(serde_json::from_str::<crate::ffi::EmojiCatalogQuery>(r#"{"offset":-1}"#).is_err());
}

#[test]
#[cfg(unix)]
fn emoji_catalog_cursor_advances_over_invalid_rows_and_preserves_duplicates() {
    let directory = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(directory.path().join("others.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE emoji(emoji TEXT,category TEXT,keywords TEXT,pinyin TEXT,sort_order INTEGER);
         CREATE TABLE kaomoji_catalog(kaomoji TEXT,keywords TEXT,sort_order INTEGER);
         CREATE TABLE symbol_catalog(symbol TEXT,category TEXT,parent_category TEXT,keywords TEXT,sort_order INTEGER);",
    ).unwrap();
    for (index, text) in [
        None,
        Some(""),
        Some("synthetic-same"),
        Some("synthetic-same"),
        Some("synthetic-tail"),
    ]
    .into_iter()
    .enumerate()
    {
        db.execute(
            "INSERT INTO emoji VALUES (?1,'fixture','match','',?2)",
            rusqlite::params![text, index as i64],
        )
        .unwrap();
        db.execute(
            "INSERT INTO kaomoji_catalog VALUES (?1,'match',?2)",
            rusqlite::params![text, index as i64],
        )
        .unwrap();
        db.execute(
            "INSERT INTO symbol_catalog VALUES (?1,'fixture','fixture','match',?2)",
            rusqlite::params![text, index as i64],
        )
        .unwrap();
    }
    let resources = directory.path().to_str().unwrap().as_bytes();
    let request = |category: &str, offset: usize, limit: u8, cursor: bool| {
        let query = serde_json::to_vec(&json!({
            "category": category, "offset": offset, "limit": limit, "cursor": cursor,
        }))
        .unwrap();
        read(unsafe {
            msime_client_emoji_catalog_request(
                query.as_ptr(),
                query.len(),
                resources.as_ptr(),
                resources.len(),
            )
        })
    };
    for category in ["", "kaomoji", "symbols"] {
        let empty = request(category, 0, 2, true);
        assert_eq!(empty["ok"], true);
        assert_eq!(
            empty["value"],
            json!({"items":[], "next_offset":2, "complete":false})
        );
        let duplicates = request(category, 2, 2, true);
        assert_eq!(duplicates["value"]["items"].as_array().unwrap().len(), 2);
        assert_eq!(duplicates["value"]["next_offset"], 4);
        assert_eq!(duplicates["value"]["complete"], false);
        let tail = request(category, 4, 2, true);
        assert_eq!(tail["value"]["items"][0]["text"], "synthetic-tail");
        assert_eq!(tail["value"]["next_offset"], 5);
        assert_eq!(tail["value"]["complete"], true);
        let exact = request(category, 4, 1, true);
        assert_eq!(exact["value"]["complete"], false);
        assert_eq!(
            request(category, 5, 1, true)["value"],
            json!({"items":[], "next_offset":5, "complete":true})
        );
        let legacy = request(category, 2, 2, false);
        assert_eq!(legacy["value"]["items"].as_array().unwrap().len(), 1);
        assert!(legacy["value"].get("complete").is_none());
        assert_eq!(request(category, 0, 0, true)["ok"], false);
    }
}

#[test]
#[cfg(unix)]
fn emoji_catalog_cursor_skips_invalid_groups_without_stalling() {
    let directory = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(directory.path().join("others.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE emoji(emoji TEXT,category TEXT,keywords TEXT,pinyin TEXT,sort_order INTEGER);
         INSERT INTO emoji VALUES ('synthetic-invalid',NULL,'','',0);
         INSERT INTO emoji VALUES ('synthetic-invalid','','','',1);
         INSERT INTO emoji VALUES ('synthetic-valid','fixture','','',2);",
    )
    .unwrap();
    let resources = directory.path().to_str().unwrap().as_bytes();
    let request = |offset: usize| {
        let query = serde_json::to_vec(&json!({"cursor":true,"offset":offset,"limit":2})).unwrap();
        read(unsafe {
            msime_client_emoji_catalog_request(
                query.as_ptr(),
                query.len(),
                resources.as_ptr(),
                resources.len(),
            )
        })
    };
    assert_eq!(
        request(0)["value"],
        json!({"items":[],"next_offset":2,"complete":false})
    );
    let tail = request(2);
    assert_eq!(tail["value"]["items"][0]["text"], "synthetic-valid");
    assert_eq!(tail["value"]["complete"], true);
}

#[test]
#[cfg(unix)]
fn emoji_catalog_ffi_reads_beyond_first_page() {
    let directory = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(directory.path().join("others.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE emoji(emoji TEXT,category TEXT,keywords TEXT,pinyin TEXT,sort_order INTEGER);
         CREATE TABLE kaomoji_catalog(kaomoji TEXT,keywords TEXT,sort_order INTEGER);
         CREATE TABLE symbol_catalog(symbol TEXT,category TEXT,parent_category TEXT,keywords TEXT,sort_order INTEGER);",
    ).unwrap();
    for index in 0..520 {
        let text = format!("synthetic-{index}");
        db.execute(
            "INSERT INTO emoji VALUES (?1,'fixture','match','',?2)",
            rusqlite::params![text, index],
        )
        .unwrap();
        db.execute(
            "INSERT INTO kaomoji_catalog VALUES (?1,'match',?2)",
            rusqlite::params![text, index],
        )
        .unwrap();
        db.execute(
            "INSERT INTO symbol_catalog VALUES (?1,'fixture','fixture','match',?2)",
            rusqlite::params![text, index],
        )
        .unwrap();
    }
    let resources = directory.path().to_str().unwrap().as_bytes();
    let request = |category: &str, offset: usize, limit: u8| {
        let query = serde_json::to_vec(
            &json!({"category":category,"search":"match","offset":offset,"limit":limit}),
        )
        .unwrap();
        read(unsafe {
            msime_client_emoji_catalog_request(
                query.as_ptr(),
                query.len(),
                resources.as_ptr(),
                resources.len(),
            )
        })
    };
    for category in ["", "kaomoji", "symbols"] {
        for (offset, count) in [(0, 255), (255, 255), (510, 10), (765, 0)] {
            let page = request(category, offset, 255);
            assert_eq!(page["ok"], true);
            assert_eq!(page["value"]["items"].as_array().unwrap().len(), count);
            if count > 0 {
                assert_eq!(
                    page["value"]["items"][0]["text"],
                    format!("synthetic-{offset}")
                );
            }
        }
    }
    db.execute(
        "UPDATE emoji SET emoji='synthetic-0' WHERE sort_order=1",
        [],
    )
    .unwrap();
    assert_eq!(
        request("", 0, 2)["value"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        request("", 2, 2)["value"]["items"][0]["text"],
        "synthetic-2"
    );
    assert_eq!(request("", 0, 0)["ok"], false);
    assert_eq!(request("", usize::MAX, 255)["ok"], false);
}

#[test]
#[cfg(unix)]
fn emoji_catalog_errors_are_not_empty_results() {
    let directory = tempfile::tempdir().unwrap();
    let resources = directory.path().to_str().unwrap().as_bytes();
    let request = |category: &str| {
        let query = serde_json::to_vec(&json!({"category":category})).unwrap();
        read(unsafe {
            msime_client_emoji_catalog_request(
                query.as_ptr(),
                query.len(),
                resources.as_ptr(),
                resources.len(),
            )
        })
    };
    let unavailable = json!({"ok":false,"error":"local emoji catalog unavailable"});
    assert_eq!(request(""), unavailable);
    let path = directory.path().join("others.db");
    assert!(!path.exists(), "read-only query must not create resources");
    std::fs::write(&path, b"synthetic invalid sqlite file").unwrap();
    for category in ["", "kaomoji", "symbols"] {
        assert_eq!(request(category), unavailable);
    }
    std::fs::remove_file(&path).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(request(""), unavailable);
    db.execute_batch("CREATE TABLE emoji(emoji TEXT,category TEXT,keywords TEXT,pinyin TEXT,sort_order INTEGER);
        CREATE TABLE kaomoji_catalog(kaomoji TEXT,keywords TEXT,sort_order INTEGER);
        CREATE TABLE symbol_catalog(symbol TEXT,category TEXT,parent_category TEXT,keywords TEXT,sort_order INTEGER);").unwrap();
    for category in ["", "kaomoji", "symbols"] {
        assert_eq!(request(category), json!({"ok":true,"value":{"items":[]}}));
    }
    // A query can prepare successfully but fail while stepping it.
    db.execute_batch(
        "DROP TABLE emoji;
        CREATE VIEW emoji AS SELECT abs(-9223372036854775808) AS emoji,
            '' AS category, '' AS keywords, '' AS pinyin, 0 AS sort_order;",
    )
    .unwrap();
    assert_eq!(request(""), unavailable);
}

#[test]
#[cfg(unix)]
fn emoji_groups_preserve_catalog_order_and_filter_before_paging() {
    let directory = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(directory.path().join("others.db")).unwrap();
    db.execute_batch("CREATE TABLE emoji(emoji TEXT,category TEXT,keywords TEXT,pinyin TEXT,sort_order INTEGER);
        INSERT INTO emoji VALUES ('one','Z','match','',1),('two','A','match','',2),('three','Z','match','',3),('four','Z','other','',4);
        CREATE TABLE symbol_catalog(symbol TEXT,category TEXT,parent_category TEXT,keywords TEXT,sort_order INTEGER);
        INSERT INTO symbol_catalog VALUES ('one','Z','parent','match',1),('two','A','parent','match',2),('three','Z','parent','match',3),('four','Z','parent','other',4);
        CREATE TABLE kaomoji_catalog(kaomoji TEXT,keywords TEXT,sort_order INTEGER);
        INSERT INTO kaomoji_catalog VALUES ('fixture','match',1);").unwrap();
    let resources = directory.path().to_str().unwrap().as_bytes();
    let request = |query: Value| {
        let query = serde_json::to_vec(&query).unwrap();
        read(unsafe {
            msime_client_emoji_catalog_request(
                query.as_ptr(),
                query.len(),
                resources.as_ptr(),
                resources.len(),
            )
        })
    };
    for category in ["", "symbols"] {
        assert_eq!(
            request(json!({"category":category,"list_groups":true}))["value"]["groups"],
            json!(["Z", "A"])
        );
        let page =
            request(json!({"category":category,"group":"Z","search":"match","offset":1,"limit":1}));
        assert_eq!(page["ok"], true);
        assert_eq!(page["value"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["value"]["items"][0]["text"], "three");
        assert_eq!(
            request(json!({"category":category,"group":"' OR 1=1 --"}))["value"]["items"],
            json!([])
        );
    }
    assert_eq!(
        request(json!({"category":"kaomoji","list_groups":true}))["value"]["groups"],
        json!(["All"])
    );
    assert_eq!(
        request(json!({"category":"kaomoji","group":"missing"}))["value"]["items"],
        json!([])
    );
    db.execute_batch("UPDATE symbol_catalog SET category='Shared', parent_category=CASE WHEN sort_order=2 THEN 'Parent-B' ELSE 'Parent-A' END WHERE sort_order<4;
        UPDATE symbol_catalog SET parent_category='' WHERE sort_order=4;").unwrap();
    assert_eq!(
        request(json!({"list_symbol_groups":true}))["value"]["symbol_groups"],
        json!([
            {"parent":"Parent-A","title":"Shared"}, {"parent":"Parent-B","title":"Shared"}, {"parent":"Z","title":"Z"}
        ])
    );
    let page = request(
        json!({"category":"symbols","parent":"Parent-A","group":"Shared","search":"match","offset":1,"limit":1}),
    );
    assert_eq!(page["value"]["items"][0]["text"], "three");
    let other = request(json!({"category":"symbols","parent":"Parent-B","group":"Shared"}));
    assert_eq!(other["value"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(other["value"]["items"][0]["text"], "two");
    assert_eq!(
        request(json!({"category":"symbols","parent":"missing"}))["value"]["items"],
        json!([])
    );
    assert_eq!(request(json!({"parent":"Parent-A"}))["ok"], false);
    db.execute_batch("DROP TABLE emoji").unwrap();
    assert_eq!(request(json!({"list_groups":true}))["ok"], false);
}

#[test]
fn translation_persistence_rejects_control_keys_before_writing() {
    let user = tempfile::tempdir().unwrap();
    let user_path = user.path().to_str().unwrap();
    let save = |translations: Value| {
        let request = serde_json::to_vec(&json!({
            "target_language": "en",
            "translations": translations,
        }))
        .unwrap();
        read(unsafe {
            msime_client_translation_gloss_save(
                request.as_ptr(),
                request.len(),
                user_path.as_ptr(),
                user_path.len(),
            )
        })
    };

    for codepoint in (0..=0x1f).chain(0x7f..=0x9f) {
        let control = char::from_u32(codepoint).unwrap();
        let result = save(json!([
            {"text":"你好","translation":"hello"},
            {"text":format!("测试{control}"),"translation":"test"},
        ]));
        assert_eq!(result["ok"], false);
        assert_eq!(
            result["error"],
            "translation persistence entries exceed limits"
        );
        assert!(!user.path().join("translation-glosses.db").exists());
    }

    let saved = save(json!([
        {"text":"你好","translation":"  hello\tworld\r\n"},
    ]));
    assert_eq!(saved["value"]["saved"], 1);
    let database = rusqlite::Connection::open(user.path().join("translation-glosses.db")).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT english_gloss FROM zh_en_glosses WHERE chinese='你好'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "hello world"
    );
}

#[test]
fn translation_persistence_waits_behind_dictionary_maintenance() {
    let user = tempfile::tempdir().unwrap();
    let user_path = user.path().to_str().unwrap();
    let request = serde_json::to_vec(&json!({
        "target_language": "en",
        "translations": [{"text":"你好","translation":"hello"}],
    }))
    .unwrap();
    let save = || {
        read(unsafe {
            msime_client_translation_gloss_save(
                request.as_ptr(),
                request.len(),
                user_path.as_ptr(),
                user_path.len(),
            )
        })
    };
    // A data directory move or an import holds the directory exclusively; a translation that lands meanwhile must not write into it.
    let maintenance = DictionaryAccess::try_maintenance(user.path(), user.path())
        .unwrap()
        .unwrap();
    let refused = save();
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"], "dictionary maintenance busy");
    assert!(!user.path().join("translation-glosses.db").exists());
    drop(maintenance);
    assert_eq!(save()["value"]["saved"], 1);
    assert!(user.path().join("translation-glosses.db").is_file());
}

#[test]
fn candidate_gloss_requests_reject_control_keys() {
    let resources = tempfile::tempdir().unwrap();
    let resources_path = resources.path().to_str().unwrap().as_bytes();
    let call = |text: String| {
        let request = serde_json::to_vec(&json!({
            "generation": 1,
            "candidates": [{"text": text, "source": 0}],
        }))
        .unwrap();
        read(unsafe {
            msime_client_candidate_gloss_request(
                request.as_ptr(),
                request.len(),
                resources_path.as_ptr(),
                resources_path.len(),
            )
        })
    };

    for codepoint in (0..=0x1f).chain(0x7f..=0x9f) {
        let control = char::from_u32(codepoint).unwrap();
        let result = call(format!("测试{control}"));
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"], "candidate gloss entries exceed limits");
    }
}

#[test]
fn candidate_gloss_request_uses_packaged_dictionary_and_bounds_input() {
    let directory = tempfile::tempdir().unwrap();
    let db = rusqlite::Connection::open(directory.path().join("english.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE english_words(word TEXT COLLATE BINARY NOT NULL,display TEXT NOT NULL,weight INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(word,display)) WITHOUT ROWID;
         CREATE TABLE en_zh_glosses(english TEXT COLLATE BINARY PRIMARY KEY,chinese_gloss TEXT NOT NULL) WITHOUT ROWID;
         CREATE TABLE zh_en_glosses(chinese TEXT COLLATE BINARY PRIMARY KEY,english_gloss TEXT NOT NULL) WITHOUT ROWID;
         INSERT INTO english_words VALUES ('hello','hello',1);
         INSERT INTO en_zh_glosses VALUES ('hello',' 你好； 您好 ；喂');
         INSERT INTO zh_en_glosses VALUES ('你好',' hello ; greeting ; salutation');
         INSERT INTO zh_en_glosses VALUES ('你好！','hello there');",
    )
    .unwrap();
    let resources = directory.path().to_str().unwrap().as_bytes();
    let call = |request: Value, resources: &[u8]| {
        let request = serde_json::to_vec(&request).unwrap();
        read(unsafe {
            msime_client_candidate_gloss_request(
                request.as_ptr(),
                request.len(),
                resources.as_ptr(),
                resources.len(),
            )
        })
    };
    let result = call(
        json!({
            "generation": 42,
            "candidates": [
                {"text":"你好","source":0},
                {"text":"Hello","source":4},
                {"text":"🙂","source":6},
                {"text":"mixed混合","source":0},
                {"text":"你好！","source":0}
            ]
        }),
        resources,
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["value"]["generation"], 42);
    assert_eq!(
        result["value"]["translations"],
        json!([
            {"text":"你好","translation":"hello; greeting"},
            {"text":"Hello","translation":"你好; 您好"},
            {"text":"你好！","translation":"hello there"}
        ])
    );
    let user = tempfile::tempdir().unwrap();
    let user_path = user.path().to_str().unwrap();
    let save = |target: &str, translations: Value| {
        let request =
            serde_json::to_vec(&json!({"target_language":target,"translations":translations}))
                .unwrap();
        read(unsafe {
            msime_client_translation_gloss_save(
                request.as_ptr(),
                request.len(),
                user_path.as_ptr(),
                user_path.len(),
            )
        })
    };
    let entries = json!([
        {"text":"你好","translation":" learned   greeting "},
        {"text":"SYNTHETIC","translation":"合成释义"},
        {"text":"unchanged","translation":"UNCHANGED"},
        {"text":"long","translation":"x".repeat(33)},
        {"text":"🙂","translation":"emoji"}
    ]);
    assert_eq!(save("fr", entries.clone())["value"]["saved"], 0);
    assert!(!user.path().join("translation-glosses.db").exists());
    assert_eq!(save("en", entries)["value"]["saved"], 2);
    // Each API call opens a fresh Engine dictionary, proving durable reuse.
    let learned = call(
        json!({"generation":43,"user_data":user_path,"candidates":[
            {"text":"你好","source":0},{"text":"Synthetic","source":4},{"text":"Hello","source":4}
        ]}),
        resources,
    );
    assert_eq!(
        learned["value"]["translations"],
        json!([
            {"text":"你好","translation":"learned greeting"},
            {"text":"Synthetic","translation":"合成释义"},
            {"text":"Hello","translation":"你好; 您好"}
        ])
    );
    assert_eq!(
        db.query_row(
            "SELECT english_gloss FROM zh_en_glosses WHERE chinese='你好'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        " hello ; greeting ; salutation"
    );
    assert_eq!(
        call(
            json!({"generation":1,"user_data":"relative","candidates":[]}),
            resources
        )["ok"],
        false
    );
    assert_eq!(
        call(json!({"generation":1,"candidates":[]}), b"relative")["ok"],
        false
    );
    assert_eq!(
        call(
            json!({"generation":1,"candidates":[{"text":"","source":0}]}),
            resources
        )["ok"],
        false
    );
    db.execute(
        "UPDATE zh_en_glosses SET english_gloss=?1 WHERE chinese='你好'",
        ["x".repeat(4097)],
    )
    .unwrap();
    assert_eq!(
        call(
            json!({"generation":1,"candidates":[{"text":"你好","source":0}]}),
            resources
        )["ok"],
        false
    );
    let missing = tempfile::tempdir().unwrap();
    assert_eq!(
        call(
            json!({"generation":1,"candidates":[{"text":"你好","source":0}]}),
            missing.path().to_str().unwrap().as_bytes()
        ),
        json!({"ok":false,"error":"candidate gloss dictionary unavailable"})
    );
    assert!(!missing.path().join("english.db").exists());
}

#[test]
fn english_completion_request_queries_dictionary_and_rejects_invalid_input() {
    let directory = tempfile::tempdir().unwrap();
    let database = rusqlite::Connection::open(directory.path().join("english.db")).unwrap();
    database
        .execute_batch(
            "CREATE TABLE english_words(word TEXT COLLATE BINARY NOT NULL,display TEXT NOT NULL,weight INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(word,display)) WITHOUT ROWID;
             INSERT INTO english_words VALUES ('hello','Hello',10),('help','help',5),('hero','hero',1);",
        )
        .unwrap();
    let resources = directory.path().to_str().unwrap().as_bytes();
    let call = |request: Value, resources: &[u8]| {
        let request = serde_json::to_vec(&request).unwrap();
        read(unsafe {
            msime_client_english_completions_request(
                request.as_ptr(),
                request.len(),
                resources.as_ptr(),
                resources.len(),
            )
        })
    };

    assert_eq!(
        call(json!({"prefix":"he","limit":3}), resources),
        json!({"ok":true,"value":{"prefix":"he","items":["Hello","help","hero"]}})
    );
    assert_eq!(
        call(json!({"prefix":"he","limit":1}), resources)["value"]["items"],
        json!(["Hello"])
    );

    for request in [
        json!({"prefix":"","limit":1}),
        json!({"prefix":"he1","limit":1}),
        json!({"prefix":format!("he{}", '\u{1}'),"limit":1}),
        json!({"prefix":"he","limit":0}),
        json!({"prefix":"he","limit":33}),
    ] {
        assert_eq!(call(request, resources)["ok"], false);
    }
    assert_eq!(
        call(json!({"prefix":"he","limit":1}), b"relative")["error"],
        "resources path must be absolute"
    );
    assert!(!directory.path().join("english.db-journal").exists());
}

/// The settled model is found beside a resource bundle, and its absence is not an error.
///
/// Beside rather than inside, because `prepare_host_configuration` verifies the resource directory
/// against the shared dictionary lock and that check requires an exact match — a model dropped in
/// there would fail the very check that proves a shipped dictionary is intact.
#[test]
fn a_settled_model_beside_the_resources_is_discovered() {
    let root = tempfile::tempdir().expect("tempdir");
    let resources = root.path().join("resources");
    std::fs::create_dir_all(&resources).expect("resources");
    assert_eq!(super::settled_model_beside(&resources), None);

    let beside = root.path().join("settled-model");
    std::fs::create_dir_all(&beside).expect("beside");
    // A directory of the right name is not a model.
    std::fs::create_dir_all(beside.join("sentence-model-desktop.safetensors")).expect("decoy");
    assert_eq!(super::settled_model_beside(&resources), None);

    std::fs::remove_dir(beside.join("sentence-model-desktop.safetensors")).expect("decoy");
    std::fs::write(
        beside.join("sentence-model-desktop.safetensors"),
        b"weights",
    )
    .expect("model");
    assert_eq!(
        super::settled_model_beside(&resources).as_deref(),
        beside.join("sentence-model-desktop.safetensors").to_str()
    );
}

#[test]
fn translation_queries_only_clear_chinese_candidates_for_the_network() {
    // A gloss model has nothing to say about a Latin letter, a digit or an emoji, and asking spends the
    // account's bounded quota to put noise under candidates that should carry no gloss. The flag gates the
    // gloss endpoint only: a user's own translator detects the direction per candidate and still sees
    // English ones, and the offline dictionary sees everything because it never leaves the machine.
    for (input, text, online) in [
        (&b"U4e2d"[..], "\u{4e2d}", true),
        (&b"U0041"[..], "A", false),
        (&b"U0031"[..], "1", false),
        (&b"U1f600"[..], "\u{1f600}", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let preferences = Preferences {
            candidate_translations: true,
            ..Preferences::default()
        };
        let handle = test_host_preferences(dir.path(), preferences);
        read(msime_client_focus(handle, true));
        for byte in input {
            read(msime_client_character(
                handle,
                *byte,
                byte.is_ascii_uppercase(),
            ));
        }
        let query = read(msime_client_translation_query(handle));
        assert_eq!(
            query["value"]["candidates"],
            json!([{ "text": text, "online_gloss": online }]),
            "candidate {text:?} was cleared for the online gloss endpoint incorrectly"
        );
    }
}

/// Every entry point the C header promises is actually exported, and the other way round.
///
/// Native hosts compile against `include/msime_client.h`; the Rust side is the implementation.
/// Nothing was comparing the two, so `msime_client_rerank_settled` shipped as a Rust export with
/// no declaration — invisible here and a compile error in every native host that reached for it.
/// A text comparison is enough to catch that, and catches the reverse omission too.
#[test]
fn the_c_header_and_the_rust_exports_agree() {
    const HEADER: &str = include_str!("../include/msime_client.h");
    // Walked rather than listed: a guard that needs a new entry every time a module is added is
    // a guard that silently stops covering things.
    fn read_all(directory: &std::path::Path, into: &mut String) {
        for entry in std::fs::read_dir(directory).expect("source directory") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                read_all(&path, into);
            } else if path.extension().is_some_and(|e| e == "rs")
                // This file included would match its own matcher's literals.
                && path.file_name().is_some_and(|name| name != "tests.rs")
            {
                into.push_str(&std::fs::read_to_string(&path).expect("source"));
            }
        }
    }
    let mut sources = String::new();
    read_all(std::path::Path::new("src"), &mut sources);

    let names = |text: &str, prefix: &str| -> std::collections::BTreeSet<String> {
        text.match_indices(prefix)
            .map(|(start, _)| {
                text[start..]
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .filter(|name| name.len() > prefix.len())
            .collect()
    };

    let declared = names(HEADER, "msime_client_");
    // Only what is actually exported: a bare text match also catches `msime_client_core::`, the
    // crate this one depends on, whose paths are not entry points.
    let exported: std::collections::BTreeSet<String> = sources
        .split("#[no_mangle]")
        .skip(1)
        .filter_map(|block| {
            let start = block.find("fn msime_client_")? + "fn ".len();
            Some(
                block[start..]
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()?
                    .to_owned(),
            )
        })
        .collect();
    assert!(!declared.is_empty() && !exported.is_empty());

    // One direction only. The header also declares types and callback typedefs under the same
    // prefix — `msime_client_key_event`, `msime_client_focus_lease` — which are not functions and
    // have no Rust export to match. The direction that breaks a host build is the other one: an
    // export a native host cannot see because nothing declares it.
    let undeclared: Vec<_> = exported.difference(&declared).cloned().collect();
    assert!(
        undeclared.is_empty(),
        "exported from Rust, absent from include/msime_client.h: {undeclared:?}"
    );
}

#[test]
fn published_defaults_complete_every_nested_preference_object() {
    // A platform host patches one key of a nested preference object and needs the
    // rest of that object's members, because only the object as a whole is
    // optional. This is the document it fills them from, so it has to carry every
    // member of every nested object - and what it produces has to be accepted back
    // by the same parser the host's session creation uses.
    let document = read(msime_client_default_preferences());
    assert!(document["ok"].as_bool() == Some(true));
    let defaults = &document["value"];
    let parsed: msime_client_core::preferences::Preferences =
        serde_json::from_value(defaults.clone()).expect("published defaults parse as Preferences");
    assert_eq!(
        parsed,
        msime_client_core::preferences::Preferences::default()
    );
    assert_eq!(defaults["mixed_input"]["minimum_prefix"], 5);

    // The two objects the Linux host patches, spelled out: a partial one of these
    // is what stopped a session being created at all.
    for (object, members) in [
        (
            "mixed_input",
            vec!["english", "minimum_prefix", "emoji", "kaomoji"],
        ),
        (
            "local_modes",
            vec![
                "unicode",
                "date_time",
                "quick_phrase",
                "emoji",
                "kaomoji",
                "super_jianpin",
                "temporary_english",
                "temporary_japanese",
            ],
        ),
    ] {
        let value = defaults
            .get(object)
            .unwrap_or_else(|| panic!("{object} missing from the published defaults"));
        for member in members {
            assert!(
                value.get(member).is_some(),
                "{object}.{member} missing from the published defaults"
            );
        }
    }
}

#[test]
#[cfg(not(target_os = "android"))]
fn personal_dictionary_sync_reads_the_same_options_as_create() {
    // Both keyboard hosts hand this entry the options they create a session with. Reading them as a request envelope refused every call: Android dropped the error, and HarmonyOS retried by rebuilding its Engine session every two seconds for as long as the keyboard was up.
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_str().unwrap().to_owned();
    let options = json!({
        "api_version": 1,
        "resources": format!("{root}/resources"),
        "user_data": format!("{root}/user"),
        "cache": format!("{root}/cache"),
        "dictionaries": format!("{root}/dictionaries"),
        "preferences": msime_client_core::preferences::Preferences::default(),
        "preferences_directory": root,
    })
    .to_string();
    let synced =
        read(unsafe { msime_client_personal_dictionary_sync(options.as_ptr(), options.len()) });
    assert_eq!(synced["ok"], true, "{synced}");
    assert_eq!(synced["value"]["pending_count"], 0);

    let envelope = json!({ "options": serde_json::from_str::<serde_json::Value>(&options).unwrap(), "action": {"operation": "retry", "request_id": "x"} }).to_string();
    let refused =
        read(unsafe { msime_client_personal_dictionary_sync(envelope.as_ptr(), envelope.len()) });
    assert_eq!(refused["error"], "invalid dictionary request");
}

#[test]
#[cfg(not(target_os = "android"))]
fn importing_a_personal_dictionary_file_queues_instead_of_taking_the_engine_lock() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_str().unwrap().to_owned();
    let options = json!({
        "api_version": 1,
        "resources": format!("{root}/resources"),
        "user_data": format!("{root}/user"),
        "cache": format!("{root}/cache"),
        "dictionaries": format!("{root}/dictionaries"),
        "preferences": msime_client_core::preferences::Preferences::default(),
        "preferences_directory": root,
    });
    let file = json!({
        "format": "msime-personal-dictionary",
        "version": 1,
        "entries": [
            {"kind": "pinyin", "key": "shui'shan", "value": "水杉", "weight": 100},
            {"kind": "quickPhrase", "key": "zjd", "value": "在家等", "weight": 100},
        ],
    })
    .to_string();
    let request = json!({
        "options": options,
        "action": {"operation": "import_personal", "text": file, "request_id": "ui-1"},
    })
    .to_string();

    let queued =
        read(unsafe { msime_client_personal_dictionary_request(request.as_ptr(), request.len()) });
    assert_eq!(queued["ok"], true);
    assert_eq!(queued["value"]["queued"], true);
    // The count is what the card shows the user, so it has to be the queue's own answer rather
    // than the number of lines that were sent.
    assert_eq!(queued["value"]["pending_count"], 2);

    // The same request through the Engine route is refused, which is the whole reason the queued
    // one exists: that route needs the maintenance lock, and a keyboard holding a session owns it.
    let engine = read(unsafe { msime_client_dictionary(request.as_ptr(), request.len()) });
    assert_eq!(engine["ok"], false);

    // A second import adds to the queue rather than replacing it: two files imported before the
    // keyboard next starts must both survive.
    let second = json!({
        "options": options,
        "action": {
            "operation": "import_personal",
            "text": json!({
                "format": "msime-personal-dictionary",
                "version": 1,
                "entries": [{"kind": "english", "key": "ime", "value": "IME", "weight": 100}],
            })
            .to_string(),
            "request_id": "ui-2",
        },
    })
    .to_string();
    let again =
        read(unsafe { msime_client_personal_dictionary_request(second.as_ptr(), second.len()) });
    assert_eq!(again["value"]["pending_count"], 3);

    // A cloud word selected for local download uses the same durable queue, but is normalized by
    // the Engine before it is persisted. The settings page may therefore send the cloud spelling
    // verbatim without becoming a second author of pinyin validation rules.
    let cloud = json!({
        "options": options,
        "action": {
            "operation": "queue_edit",
            "previous": null,
            "replacement": {
                "kind": "pinyin",
                "key": "NI HAO",
                "value": "拟好",
                "weight": 100_000,
            },
            "request_id": "ui-cloud",
        },
    })
    .to_string();
    let downloaded =
        read(unsafe { msime_client_personal_dictionary_request(cloud.as_ptr(), cloud.len()) });
    assert_eq!(downloaded["value"]["pending_count"], 4);
    let state: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.path().join("PersonalDictionary/sync.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["requests"][3]["replacement"]["key"], "ni'hao");

    let invalid_cloud = json!({
        "options": options,
        "action": {
            "operation": "queue_edit",
            "previous": null,
            "replacement": {
                "kind": "pinyin",
                "key": "nihao",
                "value": "坏词",
                "weight": 100_000,
            },
            "request_id": "ui-cloud-invalid",
        },
    })
    .to_string();
    assert_eq!(
        read(unsafe {
            msime_client_personal_dictionary_request(invalid_cloud.as_ptr(), invalid_cloud.len())
        })["ok"],
        false
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(directory.path().join("PersonalDictionary/sync.json")).unwrap(),
        )
        .unwrap()["requests"]
            .as_array()
            .unwrap()
            .len(),
        4
    );

    // A file this host cannot read is refused before anything is queued, so a malformed import
    // cannot leave the queue half-written.
    let malformed = json!({
        "options": options,
        "action": {"operation": "import_personal", "text": "not json", "request_id": "ui-3"},
    })
    .to_string();
    assert_eq!(
        read(unsafe {
            msime_client_personal_dictionary_request(malformed.as_ptr(), malformed.len())
        })["ok"],
        false
    );

    // Without a shared directory there is no queue to write to, and inventing one beside the
    // resources would put the words somewhere the keyboard never looks.
    let mut rootless = options.clone();
    rootless
        .as_object_mut()
        .unwrap()
        .remove("preferences_directory");
    let without = json!({
        "options": rootless,
        "action": {"operation": "import_personal", "text": file, "request_id": "ui-4"},
    })
    .to_string();
    assert_eq!(
        read(unsafe { msime_client_personal_dictionary_request(without.as_ptr(), without.len()) })
            ["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_personal_dictionary_request(std::ptr::null(), 0) })["ok"],
        false
    );
}

#[test]
#[cfg(not(target_os = "android"))]
fn a_queued_dictionary_file_imports_what_it_can_and_reports_the_rest() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_str().unwrap().to_owned();
    let options = json!({
        "api_version": 1,
        "resources": format!("{root}/resources"),
        "user_data": format!("{root}/user"),
        "cache": format!("{root}/cache"),
        "dictionaries": format!("{root}/dictionaries"),
        "preferences": msime_client_core::preferences::Preferences::default(),
        "preferences_directory": root,
    });
    // One row the Engine refuses and one repeated word: the queue alone would refuse the whole file for either.
    let text =
        "水杉\tshui'shan\t100\n你好\tnihaoma\t100\n水杉\tshui'shan\t100\n在家\tzai'jia\t100\n";
    let request = json!({
        "options": options,
        "action": {"operation": "import", "kind": "pinyin", "format": "standard", "text": text, "request_id": "ui-import-1"},
    })
    .to_string();
    let queued =
        read(unsafe { msime_client_personal_dictionary_request(request.as_ptr(), request.len()) });
    assert_eq!(queued["ok"], true, "{queued}");
    assert_eq!(queued["value"]["queued"], true);
    assert_eq!(queued["value"]["applied"], 2);
    assert_eq!(queued["value"]["pending_count"], 2);
    assert_eq!(queued["value"]["failed"], 1);
    assert_eq!(queued["value"]["first_failures"][0]["line"], 2);
    assert_eq!(queued["value"]["truncated"], false);

    // The queue holds 128 words; a longer file queues the first 128 and says the rest were not read.
    let long: String = (0..200)
        .map(|index: u8| {
            // Quick phrase codes are letters only, so the index is spelled in letters.
            let code = [b'a' + index / 26, b'a' + index % 26].map(char::from);
            format!("短语{index}\tq{}{}\t100\n", code[0], code[1])
        })
        .collect();
    let fresh = tempfile::tempdir().unwrap();
    let mut long_options = options.clone();
    long_options["preferences_directory"] = json!(fresh.path().to_str().unwrap());
    let request = json!({
        "options": long_options,
        "action": {"operation": "import", "kind": "quick_phrase", "format": "standard", "text": long, "request_id": "ui-import-2"},
    })
    .to_string();
    let queued =
        read(unsafe { msime_client_personal_dictionary_request(request.as_ptr(), request.len()) });
    assert_eq!(queued["value"]["applied"], 128, "{queued}");
    assert_eq!(queued["value"]["pending_count"], 128);
    assert_eq!(queued["value"]["truncated"], true);

    // A file with nothing usable in it is a failed import, and nothing is queued for it.
    let request = json!({
        "options": long_options,
        "action": {"operation": "import", "kind": "pinyin", "format": "standard", "text": "你好\tnihaoma\t100\n", "request_id": "ui-import-3"},
    })
    .to_string();
    let refused =
        read(unsafe { msime_client_personal_dictionary_request(request.as_ptr(), request.len()) });
    assert_eq!(refused["ok"], false);

    // The parse-only entry point answers the same words and report and writes nothing.
    let resources = tempfile::tempdir().unwrap();
    let path = resources.path().to_str().unwrap();
    let parse = json!({"kind": "pinyin", "format": "standard", "text": text}).to_string();
    let parsed = read(unsafe {
        msime_client_dictionary_import_entries(
            parse.as_ptr(),
            parse.len(),
            path.as_ptr(),
            path.len(),
        )
    });
    assert_eq!(parsed["ok"], true, "{parsed}");
    let entries = parsed["value"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["key"], "shui'shan");
    assert_eq!(entries[0]["value"], "水杉");
    assert_eq!(parsed["value"]["applied"], 2);
    assert_eq!(parsed["value"]["failed"], 1);
    assert_eq!(std::fs::read_dir(resources.path()).unwrap().count(), 0);
    let relative = "relative/resources";
    assert_eq!(
        read(unsafe {
            msime_client_dictionary_import_entries(
                parse.as_ptr(),
                parse.len(),
                relative.as_ptr(),
                relative.len(),
            )
        })["ok"],
        false
    );
    assert_eq!(
        read(unsafe {
            msime_client_dictionary_import_entries(std::ptr::null(), 0, path.as_ptr(), path.len())
        })["ok"],
        false
    );
}

#[test]
#[cfg(not(target_os = "android"))]
fn the_dictionary_manifest_answers_what_is_installed_or_says_it_cannot() {
    let directory = tempfile::tempdir().unwrap();
    let resources = directory.path();
    let read_manifest = || {
        let path = resources.to_str().unwrap();
        read(unsafe { msime_client_dictionary_manifest(path.as_ptr(), path.len()) })
    };
    let commit = "d0dc0c2b594b5540b5de99ad12085c786410626e";

    // No manifest is a refusal, not an empty answer: the file is packaged, so its absence means
    // the installation is not what it should be.
    assert_eq!(read_manifest()["ok"], false);

    // The real shape, with every field the packaged manifest carries. Only two come back — the
    // page is asking what is installed and where it came from, not for journal modes.
    std::fs::write(
        resources.join("dictionary-manifest.json"),
        json!({
            "manifest_version": 1,
            "profile": "desktop",
            "format_version": 1,
            "engine_compatibility": {"dictionary_format": 1, "japanese_model_magic": "MSJPDT1"},
            "source": {
                "repository": "metasequoiaime/msime-engine",
                "path": "dictionary",
                "commit": commit,
                "dirty": false,
            },
            "sqlite_journal_mode": "delete",
            "references": {"ECDICT": {"repository": "https://example.invalid", "commit": "b"}},
        })
        .to_string(),
    )
    .unwrap();
    let manifest = read_manifest();
    assert_eq!(manifest["ok"], true);
    assert_eq!(manifest["value"]["profile"], "desktop");
    assert_eq!(manifest["value"]["sourceCommit"], commit);
    assert_eq!(manifest["value"].as_object().unwrap().len(), 2);

    // A commit that is not one is refused rather than shown. A version stated wrongly is worse
    // than one not stated, which is the whole reason this reports instead of guessing.
    for broken in [
        json!({"profile": "desktop", "source": {"commit": "not-a-commit"}}),
        json!({"profile": "desktop", "source": {"commit": "abc"}}),
        json!({"profile": "", "source": {"commit": commit}}),
        json!({"profile": "desktop"}),
        json!({"source": {"commit": commit}}),
    ] {
        std::fs::write(
            resources.join("dictionary-manifest.json"),
            broken.to_string(),
        )
        .unwrap();
        assert_eq!(read_manifest()["ok"], false, "accepted {broken}");
    }
    std::fs::write(resources.join("dictionary-manifest.json"), "not json").unwrap();
    assert_eq!(read_manifest()["ok"], false);

    // A relative directory is refused rather than resolved against whatever the process happens
    // to have as its working directory.
    let relative = "engine";
    assert_eq!(
        read(unsafe { msime_client_dictionary_manifest(relative.as_ptr(), relative.len()) })["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_dictionary_manifest(std::ptr::null(), 0) })["ok"],
        false
    );
}

/// Options published before a package upgrade name the previous dictionary generation; only those are re-prepared, and only the generation-bearing keys change.
#[test]
fn stale_dictionary_generation_is_prepared_and_other_keys_survive() {
    let document = json!({
        "api_version": 1,
        "resources": "/usr/share/msime-client/resources",
        "user_data": "/home/u/.config/msime-client/user",
        "cache": "/home/u/.config/msime-client/cache",
        "dictionaries": "/home/u/.config/msime-client/user/dictionaries/old",
        "preferences_directory": "/home/u/.config/msime-client",
        "online_provider_socket": "/run/user/1000/msime-online.sock",
    });
    let mut requested = None;
    let refreshed = super::refreshed_host_options(&document, "new", |resources, state| {
        requested = Some((resources.to_owned(), state.to_owned()));
        Ok(json!({
            "resources": "/usr/share/msime-client/resources",
            "dictionaries": "/home/u/.config/msime-client/user/dictionaries/new",
            "preferences": {},
        }))
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        requested,
        Some((
            PathBuf::from("/usr/share/msime-client/resources"),
            PathBuf::from("/home/u/.config/msime-client")
        ))
    );
    let mut expected = document.clone();
    expected["dictionaries"] = json!("/home/u/.config/msime-client/user/dictionaries/new");
    assert_eq!(refreshed, expected);
}

#[test]
fn current_or_unfamiliar_options_are_not_prepared() {
    let current = json!({
        "resources": "/r",
        "user_data": "/s/user",
        "dictionaries": "/s/user/dictionaries/new",
        "preferences_directory": "/s",
    });
    let mut unfamiliar = current.clone();
    unfamiliar["dictionaries"] = json!("/elsewhere/dictionaries/old");
    let mut moved = current.clone();
    moved["user_data"] = json!("/t/user");
    let mut relative = current.clone();
    relative["resources"] = json!("r");
    for document in [current, unfamiliar, moved, relative, json!({})] {
        let refreshed = super::refreshed_host_options(&document, "new", |_, _| {
            panic!("must not prepare {document}")
        })
        .unwrap();
        assert_eq!(refreshed, None);
    }
}

#[test]
fn a_failed_preparation_is_reported_and_incomplete_output_rejected() {
    let stale = json!({
        "resources": "/r",
        "user_data": "/s/user",
        "dictionaries": "/s/user/dictionaries/old",
        "preferences_directory": "/s",
    });
    assert!(super::refreshed_host_options(&stale, "new", |_, _| Err("busy".into())).is_err());
    assert!(
        super::refreshed_host_options(&stale, "new", |_, _| Ok(json!({"resources": "/r"})))
            .is_err()
    );
}

/// A symlinked locator is left alone: replacing it would turn the link into a private copy.
#[cfg(unix)]
#[test]
fn refresh_leaves_a_symlinked_options_file_alone() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target.json");
    std::fs::write(&target, b"{}").unwrap();
    let link = directory.path().join("runtime-options.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(!super::refresh_host_options(&link).unwrap());
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    let current = directory.path().join("current.json");
    std::fs::write(&current, b"{\"resources\":\"/r\"}").unwrap();
    assert!(!super::refresh_host_options(&current).unwrap());
}

/// Downloaded dictionaries that an upgrade left behind the compiled lock are reported as `dictionary_outdated`, the one refresh failure hosts turn into a pointer at `msime-linux-setup --update --download`, and the options file keeps pointing at the working previous generation.
#[test]
fn refresh_reports_outdated_resources_and_leaves_the_options_alone() {
    let directory = tempfile::tempdir().unwrap();
    let resources = directory.path().join("resources");
    std::fs::create_dir(&resources).unwrap();
    // A file the previous lock pinned; the compiled lock names none of it.
    std::fs::write(resources.join("msime.db"), b"previous generation").unwrap();
    let state = directory.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let options = state.join("runtime-options.json");
    let document = serde_json::to_vec_pretty(&json!({
        "api_version": 1,
        "resources": resources,
        "user_data": state.join("user"),
        "cache": state.join("cache"),
        "dictionaries": state.join("user/dictionaries/previous"),
        "preferences_directory": state,
        "preferences": {},
    }))
    .unwrap();
    std::fs::write(&options, &document).unwrap();

    let error = super::refresh_host_options(&options).unwrap_err();
    assert!(error.is::<super::DictionaryOutdated>(), "{error}");
    assert!(
        error
            .to_string()
            .starts_with(super::DICTIONARY_OUTDATED_PREFIX),
        "{error}"
    );
    assert_eq!(std::fs::read(&options).unwrap(), document);
    let mut entries: Vec<_> = std::fs::read_dir(&state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    assert_eq!(entries, ["runtime-options.json"]);

    // The same prefix reaches a host through the C ABI.
    let path = options.to_str().unwrap();
    let raw = unsafe { super::msime_client_refresh_host(path.as_ptr(), path.len()) };
    let response: Value =
        serde_json::from_str(unsafe { std::ffi::CStr::from_ptr(raw) }.to_str().unwrap()).unwrap();
    unsafe { super::msime_client_string_free(raw) };
    assert_eq!(response["ok"], json!(false));
    assert!(response["error"]
        .as_str()
        .unwrap()
        .starts_with(super::DICTIONARY_OUTDATED_PREFIX));
    assert_eq!(std::fs::read(&options).unwrap(), document);
}

/// Anything other than a mismatch keeps its own error, so a host does not send the user to download dictionaries that are not the problem.
#[test]
fn only_a_resource_mismatch_counts_as_outdated() {
    use msime_client_core::resources::ResourceError;
    let mismatch = super::outdated_resources(Box::new(ResourceError::Integrity));
    assert!(mismatch.is::<super::DictionaryOutdated>());
    let unexpected =
        super::outdated_resources(Box::new(ResourceError::ExistingGeneration("stale".into())));
    assert!(unexpected.is::<super::DictionaryOutdated>());
    for other in [
        Box::new(ResourceError::InvalidManifest) as Box<dyn std::error::Error>,
        Box::new(ResourceError::Io(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        ))),
        Box::new(std::io::Error::from(std::io::ErrorKind::NotFound)),
        "busy".into(),
    ] {
        let text = other.to_string();
        let mapped = super::outdated_resources(other);
        assert!(!mapped.is::<super::DictionaryOutdated>(), "{text}");
        assert_eq!(mapped.to_string(), text);
    }
}
#[test]
fn vocabulary_boundary_imports_reviews_and_reports_one_whole_status() {
    let directory = tempfile::tempdir().unwrap();
    let call = |day: &str, action: Value| {
        let request = serde_json::to_vec(&json!({
            "directory": directory.path(),
            "resources": directory.path(),
            "day": day,
            "action": action,
        }))
        .unwrap();
        read(unsafe { msime_client_vocabulary_review(request.as_ptr(), request.len()) })
    };

    // A fresh directory has no books and therefore no queue, rather than an error.
    let empty = call("2026-09-23", json!({"operation": "load"}));
    assert_eq!(empty["ok"], true);
    assert_eq!(empty["value"]["wordbooks"].as_array().unwrap().len(), 0);
    assert_eq!(empty["value"]["settings"]["wordbook"], "");
    assert_eq!(empty["value"]["due"], 0);
    assert_eq!(empty["value"]["queue"].as_array().unwrap().len(), 0);

    // Importing selects the book it just created: leaving the user to pick it out of a list is a
    // step with exactly one right answer.
    let imported = call(
        "2026-09-23",
        json!({
            "operation": "import",
            "name": "合成词表",
            "text": "alpha,/a/,adj. 甲\nbeta,adj. 乙\ngamma,adj. 丙\n",
        }),
    );
    assert_eq!(imported["ok"], true);
    let books = imported["value"]["wordbooks"].as_array().unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0]["name"], "合成词表");
    assert_eq!(books[0]["total"], 3);
    assert_eq!(books[0]["builtin"], false);
    let book_id = books[0]["id"].as_str().unwrap().to_owned();
    assert!(book_id.starts_with("user-"), "the library mints the id");
    assert_eq!(imported["value"]["settings"]["wordbook"], book_id);
    assert_eq!(imported["value"]["introducing"], 3);
    assert_eq!(imported["value"]["queue"].as_array().unwrap().len(), 3);
    assert_eq!(imported["value"]["queue"][0]["word"], "alpha");
    assert_eq!(imported["value"]["queue"][0]["phonetic"], "/a/");
    assert_eq!(imported["value"]["queue"][1]["phonetic"], "");

    // 认识 schedules the card a day out, so it leaves today's queue and the day counts one answer.
    let answered = call(
        "2026-09-23",
        json!({"operation": "answer", "word": "alpha", "known": true}),
    );
    assert_eq!(answered["value"]["answeredToday"], 1);
    assert_eq!(answered["value"]["queue"][0]["word"], "beta");

    // 不认识 keeps the card in the same session.
    let failed = call(
        "2026-09-23",
        json!({"operation": "answer", "word": "beta", "known": false}),
    );
    assert_eq!(failed["value"]["answeredToday"], 2);
    let queue: Vec<&str> = failed["value"]["queue"]
        .as_array()
        .unwrap()
        .iter()
        .map(|card| card["word"].as_str().unwrap())
        .collect();
    assert!(
        queue.contains(&"beta"),
        "a failed card stays in its session"
    );

    // The next day the recalled card is due again and the counts start over.
    let tomorrow = call("2026-09-24", json!({"operation": "load"}));
    assert_eq!(tomorrow["value"]["answeredToday"], 0);
    assert_eq!(tomorrow["value"]["due"], 2);

    // Settings round-trip through the boundary.
    let settings = call(
        "2026-09-23",
        json!({
            "operation": "set_settings",
            "wordbook": book_id,
            "new_per_day": 1,
            "session_limit": 50,
        }),
    );
    assert_eq!(settings["value"]["settings"]["newPerDay"], 1);
    assert_eq!(settings["value"]["settings"]["sessionLimit"], 50);

    // Reset clears the progress and keeps the book and the selection: the button says 清空进度,
    // not 删除词表.
    let reset = call("2026-09-23", json!({"operation": "reset"}));
    assert_eq!(reset["value"]["answeredToday"], 0);
    assert_eq!(reset["value"]["settings"]["wordbook"], book_id);
    assert_eq!(reset["value"]["wordbooks"].as_array().unwrap().len(), 1);

    // Removing the book takes its schedule with it and clears the selection.
    let removed = call(
        "2026-09-23",
        json!({"operation": "remove", "wordbook": book_id}),
    );
    assert_eq!(removed["value"]["wordbooks"].as_array().unwrap().len(), 0);
    assert_eq!(removed["value"]["settings"]["wordbook"], "");
    assert_eq!(removed["value"]["queue"].as_array().unwrap().len(), 0);
}
#[test]
fn vocabulary_boundary_rejects_a_bad_envelope_without_touching_the_store() {
    let directory = tempfile::tempdir().unwrap();
    let call = |value: Value| {
        let request = serde_json::to_vec(&value).unwrap();
        read(unsafe { msime_client_vocabulary_review(request.as_ptr(), request.len()) })
    };

    assert_eq!(
        read(unsafe { msime_client_vocabulary_review(std::ptr::null(), 0) })["ok"],
        false
    );
    assert_eq!(
        call(json!({
            "directory": "relative/path",
            "resources": "relative/path",
            "day": "2026-09-23",
            "action": {"operation": "load"},
        }))["ok"],
        false,
        "the directory must be absolute"
    );
    assert_eq!(
        call(json!({
            "directory": directory.path(),
            "resources": directory.path(),
            "day": "2026-13-01",
            "action": {"operation": "import", "name": "坏日期", "text": "a,adj. 甲\n"},
        }))["ok"],
        false,
        "an unparseable day is refused"
    );
    assert_eq!(
        call(json!({
            "directory": directory.path(),
            "resources": directory.path(),
            "day": "2026-09-23",
            "action": {"operation": "import", "name": "空的", "text": "# 只有注释\n"},
        }))["ok"],
        false,
        "a file with no usable rows is refused rather than stored empty"
    );
    // An unknown key in the envelope is a hard failure, the way every other request type treats it.
    assert_eq!(
        call(json!({
            "directory": directory.path(),
            "resources": directory.path(),
            "day": "2026-09-23",
            "surprise": true,
            "action": {"operation": "load"},
        }))["ok"],
        false
    );
    assert!(
        !directory.path().join("vocabulary-progress.json").exists(),
        "a refused request writes nothing"
    );
}

#[test]
fn voice_hotword_correction_rewrites_homophones() {
    let request = json!({ "text": "我在名天科技上班", "hotwords": [{"text": "明天科技", "pinyin": "ming tian ke ji"}] }).to_string();
    let corrected =
        read(unsafe { msime_client_voice_hotword_correct(request.as_ptr(), request.len()) });
    assert_eq!(
        corrected,
        json!({"ok": true, "value": {"text": "我在明天科技上班"}})
    );
    let malformed = b"{\"text\": 1}";
    assert_eq!(
        read(unsafe { msime_client_voice_hotword_correct(malformed.as_ptr(), malformed.len()) })
            ["ok"],
        false
    );
    assert_eq!(
        read(unsafe { msime_client_voice_hotword_correct(std::ptr::null(), 4) })["ok"],
        false
    );
}

#[test]
fn voice_local_models_list_install_cancel_and_remove_validate_their_requests() {
    let root = tempfile::tempdir().unwrap();
    let request = json!({ "root": root.path() }).to_string();
    let listed = read(unsafe { msime_client_voice_local_models(request.as_ptr(), request.len()) });
    assert_eq!(listed["ok"], true, "{listed}");
    let models = listed["value"]["models"].as_array().unwrap();
    assert!(!models.is_empty());
    assert!(models.iter().all(|model| model["installed"] == false));
    let default = listed["value"]["default"].as_str().unwrap();
    assert!(models.iter().any(|model| model["id"] == default));

    let relative = json!({ "root": "models" }).to_string();
    assert_eq!(
        read(unsafe { msime_client_voice_local_models(relative.as_ptr(), relative.len()) })["ok"],
        false
    );

    let unknown = json!({ "root": root.path(), "id": "no-such-model" }).to_string();
    let install = read(unsafe {
        msime_client_voice_local_model_install(
            unknown.as_ptr(),
            unknown.len(),
            None,
            std::ptr::null_mut(),
        )
    });
    assert_eq!(install["ok"], false, "{install}");
    let remove =
        read(unsafe { msime_client_voice_local_model_remove(unknown.as_ptr(), unknown.len()) });
    assert_eq!(remove["ok"], false, "{remove}");

    let bad_mirror =
        json!({ "root": root.path(), "id": default, "mirror": "http://mirror.example" })
            .to_string();
    let install = read(unsafe {
        msime_client_voice_local_model_install(
            bad_mirror.as_ptr(),
            bad_mirror.len(),
            None,
            std::ptr::null_mut(),
        )
    });
    assert_eq!(install["ok"], false, "{install}");

    let removed = json!({ "root": root.path(), "id": default }).to_string();
    assert_eq!(
        read(unsafe { msime_client_voice_local_model_remove(removed.as_ptr(), removed.len()) })
            ["ok"],
        true
    );

    let cancel = json!({ "id": default }).to_string();
    assert_eq!(
        read(unsafe { msime_client_voice_local_model_cancel(cancel.as_ptr(), cancel.len()) }),
        json!({"ok": true, "value": false})
    );
    assert_eq!(
        read(unsafe { msime_client_voice_local_model_cancel(std::ptr::null(), 0) })["ok"],
        true
    );
}

#[test]
fn mcp_status_and_install_check_their_requests_before_touching_a_file() {
    let status =
        |request: &[u8]| read(unsafe { msime_client_mcp_status(request.as_ptr(), request.len()) });
    let install =
        |request: &[u8]| read(unsafe { msime_client_mcp_install(request.as_ptr(), request.len()) });

    // Before the input method is set up there is no entry to show, but the server path still is.
    let value = &status(br#"{"options":null}"#)["value"];
    assert!(value["command"]
        .as_str()
        .unwrap()
        .ends_with(&format!("msime-mcp{}", std::env::consts::EXE_SUFFIX)));
    assert!(value["config"].is_null());
    assert!(value["clients"]
        .as_array()
        .unwrap()
        .iter()
        .all(|client| client["configured"] == false));

    let options = if cfg!(windows) {
        r"C:\\state\\runtime-options.json"
    } else {
        "/state/runtime-options.json"
    };
    let value = &status(format!(r#"{{"options":"{options}"}}"#).as_bytes())["value"];
    let snippet: Value = serde_json::from_str(value["config"].as_str().unwrap()).unwrap();
    assert_eq!(snippet["mcpServers"]["msime"]["command"], value["command"]);
    assert_eq!(
        snippet["mcpServers"]["msime"]["args"],
        json!(["--options", options.replace(r"\\", r"\")])
    );

    assert_eq!(
        status(br#"{"options":"relative.json"}"#)["error"],
        "mcp options must be absolute"
    );
    assert_eq!(status(br#"{"unknown":1}"#)["error"], "invalid mcp request");
    assert_eq!(
        read(unsafe { msime_client_mcp_status(std::ptr::null(), 4) })["error"],
        "invalid mcp request buffer"
    );
    assert_eq!(
        install(br#"{"options":null,"client":"cursor"}"#)["error"],
        "mcp_options_missing"
    );
    assert_eq!(
        install(br#"{"options":null,"client":"other"}"#)["error"],
        "invalid mcp request"
    );
}
