//! Unit tests for the parent module, in their own file because the module
//! is large enough that mixing them with the implementation obscured both.
//! Same `mod tests` as before, so `use super::*` still names the parent.

use super::*;

#[test]
fn voice_commit_mode_defaults_for_legacy_documents() {
    let mut value = serde_json::to_value(Preferences::default()).unwrap();
    value["voice_input"]
        .as_object_mut()
        .unwrap()
        .remove("commit_mode");
    let restored: Preferences = serde_json::from_value(value).unwrap();
    assert_eq!(restored.voice_input.commit_mode, "tsf");
}

#[test]
fn oversized_preference_documents_are_rejected_before_loading() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preferences.json");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_DOCUMENT_BYTES + 1)
        .unwrap();
    assert!(matches!(
        PreferencesStore::new(directory.path()).load(),
        Err(PreferencesError::DocumentTooLarge)
    ));
}

#[test]
fn doubao_auth_mode_defaults_and_roundtrips() {
    let mut value = serde_json::to_value(Preferences::default()).unwrap();
    value["voice_input"]
        .as_object_mut()
        .unwrap()
        .remove("doubao_auth_mode");
    let restored: Preferences = serde_json::from_value(value).unwrap();
    assert_eq!(restored.voice_input.doubao_auth_mode, "");

    let mut explicit = Preferences::default();
    explicit.voice_input.doubao_auth_mode = "legacy".into();
    let roundtripped: Preferences =
        serde_json::from_value(serde_json::to_value(explicit).unwrap()).unwrap();
    assert_eq!(roundtripped.voice_input.doubao_auth_mode, "legacy");
}

#[test]
fn legacy_voice_upgrade_preserves_existing_credentials() {
    let mut value = serde_json::to_value(Preferences::default()).unwrap();
    let voice = value["voice_input"].as_object_mut().unwrap();
    voice.insert("asr_token".into(), "synthetic-asr-token".into());
    voice.insert("polish_token".into(), "synthetic-polish-token".into());
    voice.remove("commit_mode");
    voice.remove("doubao_auth_mode");
    voice.remove("asr_tokens");
    voice.remove("polish_tokens");
    let restored: Preferences = serde_json::from_value(value).unwrap();
    assert_eq!(restored.voice_input.asr_token, "synthetic-asr-token");
    assert_eq!(restored.voice_input.polish_token, "synthetic-polish-token");
    assert_eq!(restored.voice_input.commit_mode, "tsf");
    assert_eq!(restored.voice_input.doubao_auth_mode, "");
    assert!(restored.voice_input.asr_tokens.is_empty());
    assert!(restored.voice_input.polish_tokens.is_empty());
}

#[test]
fn unreachable_voice_providers_normalize_on_read_without_rewriting_the_file() {
    // A file written by a build that offered "local_whisper" must still load.
    // No backend implements it: the Linux provider builds
    // {openai, groq, siliconflow, doubao}, so it would fail every recording.
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let mut document = serde_json::to_value(PreferencesSnapshot {
        format_version: 1,
        revision: 3,
        preferences: Preferences::default(),
    })
    .unwrap();
    document["preferences"]["voice_input"]["asr_provider"] =
        serde_json::Value::String("local_whisper".into());
    document["preferences"]["voice_input"]["polish_provider"] =
        serde_json::Value::String("nonesuch".into());
    let path = directory.path().join("preferences.json");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let original = std::fs::read(&path).unwrap();

    let snapshot = store.load().expect("a legacy file still loads");
    assert_eq!(
        snapshot.preferences.voice_input.asr_provider,
        Preferences::default().voice_input.asr_provider
    );
    assert_eq!(
        snapshot.preferences.voice_input.polish_provider,
        Preferences::default().voice_input.polish_provider
    );
    // Reading must not rewrite the user's file.
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn ai_assistant_without_a_provider_key_loads_with_the_default_provider() {
    assert_eq!(
        default_ai_provider(),
        AiAssistantPreferences::default().provider
    );
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let mut document = serde_json::to_value(PreferencesSnapshot {
        format_version: 1,
        revision: 3,
        preferences: Preferences::default(),
    })
    .unwrap();
    document["preferences"]["ai_assistant"] = serde_json::json!({ "enabled": true });
    std::fs::write(
        directory.path().join("preferences.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();

    let snapshot = store
        .load()
        .expect("a section without provider still loads");
    assert_eq!(snapshot.preferences.ai_assistant.provider, "deepseek");
    let saved = store.save(3, snapshot.preferences.clone()).unwrap();
    assert_eq!(store.load().unwrap(), saved);
}

#[test]
fn every_reachable_voice_provider_validates_and_others_are_rejected_on_save() {
    for provider in ASR_PROVIDERS {
        let preferences = Preferences {
            voice_input: VoiceInputPreferences {
                asr_provider: provider.into(),
                ..Preferences::default().voice_input
            },
            ..Preferences::default()
        };
        assert!(
            preferences.validate().is_ok(),
            "{provider} should be accepted"
        );
    }
    for provider in POLISH_PROVIDERS {
        let preferences = Preferences {
            voice_input: VoiceInputPreferences {
                polish_provider: provider.into(),
                ..Preferences::default().voice_input
            },
            ..Preferences::default()
        };
        assert!(preferences.validate().is_ok(), "{provider} should polish");
    }
    // Saving a provider no backend implements is refused rather than stored.
    for rejected in ["local_whisper", "cloud", "", "DOUBAO"] {
        let preferences = Preferences {
            voice_input: VoiceInputPreferences {
                asr_provider: rejected.into(),
                ..Preferences::default().voice_input
            },
            ..Preferences::default()
        };
        assert!(
            matches!(
                preferences.validate(),
                Err(PreferencesError::InvalidVoiceInput)
            ),
            "{rejected} should be rejected"
        );
    }
    // Recognition has no DeepSeek profile even though polishing does.
    let preferences = Preferences {
        voice_input: VoiceInputPreferences {
            asr_provider: "deepseek".into(),
            ..Preferences::default().voice_input
        },
        ..Preferences::default()
    };
    assert!(preferences.validate().is_err());
}

#[test]
fn local_recognition_stores_an_absolute_model_path_and_refuses_anything_else() {
    let with_path = |path: &str| Preferences {
        voice_input: VoiceInputPreferences {
            asr_provider: "local".into(),
            asr_model_path: path.into(),
            ..Preferences::default().voice_input
        },
        ..Preferences::default()
    };
    // A Windows path is absolute too: the Windows host saves one, and the same document is validated wherever it is read.
    for accepted in [
        "",
        "/Users/someone/models/ggml-large-v3-turbo.bin",
        "/Users/someone/Library/Application Support/msime/voice-models/x-asr-zh-en-streaming",
        r"C:\Users\someone\AppData\Roaming\msime\voice-models\sense-voice-small",
        "D:/models/ggml.bin",
        r"\\?\C:\models\x-asr-zh-en-streaming",
        r"\\fileserver\share\models\ggml.bin",
    ] {
        assert!(
            with_path(accepted).validate().is_ok(),
            "{accepted:?} should be accepted"
        );
    }
    // A relative path resolves against whichever process happens to read it, and a control character
    // reaches the recognizer as a filename it cannot open. Both fail while the user holds the shortcut.
    for rejected in [
        "models/ggml.bin",
        "~/models/ggml.bin",
        "/models/gg\nml.bin",
        r"C:models\ggml.bin",
        r"\models\ggml.bin",
        "C:\\models\\gg\tml.bin",
    ] {
        assert!(
            matches!(
                with_path(rejected).validate(),
                Err(PreferencesError::InvalidVoiceInput)
            ),
            "{rejected:?} should be rejected"
        );
    }
    assert!(with_path(&"/".repeat(4097)).validate().is_err());
}

#[test]
fn local_model_mirror_is_empty_or_an_https_prefix() {
    let with_mirror = |mirror: &str| Preferences {
        voice_input: VoiceInputPreferences {
            asr_model_mirror: mirror.into(),
            ..Preferences::default().voice_input
        },
        ..Preferences::default()
    };
    assert!(Preferences::default()
        .voice_input
        .asr_model_mirror
        .is_empty());
    for accepted in [
        "",
        "https://ghproxy.example.test",
        "https://mirror.example.test/gh/",
    ] {
        assert!(
            with_mirror(accepted).validate().is_ok(),
            "{accepted:?} should be accepted"
        );
    }
    // A plain-HTTP mirror would let anyone on the path swap the model; the checksum still catches it, but the download should not be attempted at all.
    for rejected in [
        "http://ghproxy.example.test",
        "https://",
        "ghproxy.example.test",
        "https://mirror.example.test/\n",
        "https://mirror example.test",
    ] {
        assert!(
            matches!(
                with_mirror(rejected).validate(),
                Err(PreferencesError::InvalidVoiceInput)
            ),
            "{rejected:?} should be rejected"
        );
    }
    let long = format!("https://{}", "a".repeat(2048));
    assert!(with_mirror(&long).validate().is_err());
    // Older documents without the field still load, with no mirror.
    let mut legacy = serde_json::to_value(Preferences::default()).unwrap();
    legacy["voice_input"]
        .as_object_mut()
        .unwrap()
        .remove("asr_model_mirror");
    let loaded: Preferences = serde_json::from_value(legacy).unwrap();
    assert!(loaded.voice_input.asr_model_mirror.is_empty());
}

#[test]
fn the_shipped_defaults_are_themselves_reachable() {
    let defaults = Preferences::default();
    assert!(ASR_PROVIDERS.contains(&defaults.voice_input.asr_provider.as_str()));
    assert!(POLISH_PROVIDERS.contains(&defaults.voice_input.polish_provider.as_str()));
    assert!(defaults.validate().is_ok());
}

#[test]
fn system_voice_provider_round_trips_without_cloud_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let mut preferences = Preferences::default();
    preferences.voice_input.asr_provider = "system".into();
    preferences.voice_input.asr_endpoint.clear();
    preferences.voice_input.asr_model.clear();
    preferences.voice_input.asr_token.clear();
    let saved = store.save(0, preferences.clone()).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded, saved);
    assert_eq!(loaded.preferences, preferences);
    assert_eq!(loaded.preferences.voice_input.asr_provider, "system");
}

#[test]
fn candidate_english_gloss_is_opt_in_and_round_trips() {
    let defaults = Preferences::default();
    assert!(!defaults.candidate_english_gloss);
    let mut legacy = serde_json::to_value(&defaults).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("candidate_english_gloss");
    assert!(
        !serde_json::from_value::<Preferences>(legacy)
            .unwrap()
            .candidate_english_gloss
    );
    let enabled = Preferences {
        candidate_english_gloss: true,
        ..defaults
    };
    assert!(
        serde_json::from_str::<Preferences>(&serde_json::to_string(&enabled).unwrap())
            .unwrap()
            .candidate_english_gloss
    );
}

#[test]
fn english_suggestions_default_on_and_legacy_documents_preserve_it() {
    let defaults = Preferences::default();
    assert!(defaults.english_suggestions);
    let mut legacy = serde_json::to_value(&defaults).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("english_suggestions");
    assert!(
        serde_json::from_value::<Preferences>(legacy)
            .unwrap()
            .english_suggestions
    );
    let mut disabled = defaults;
    disabled.english_suggestions = false;
    assert!(
        !serde_json::from_str::<Preferences>(&serde_json::to_string(&disabled).unwrap())
            .unwrap()
            .english_suggestions
    );
}

// Telemetry is opt-in on every host that reads this switch: a fresh profile, and a document written before the switch existed, must both say off, so upgrading never turns reporting on behind the user.
#[test]
fn telemetry_is_opt_in_and_survives_a_save() {
    let defaults = Preferences::default();
    assert!(!defaults.telemetry_enabled);
    let serialized = serde_json::to_value(&defaults).unwrap();
    assert_eq!(
        serialized["telemetry_enabled"],
        serde_json::Value::Bool(false)
    );
    let mut legacy = serialized.clone();
    legacy.as_object_mut().unwrap().remove("telemetry_enabled");
    assert!(
        !serde_json::from_value::<Preferences>(legacy)
            .unwrap()
            .telemetry_enabled
    );
    let mut malformed = serialized;
    malformed["telemetry_enabled"] = "yes".into();
    assert!(serde_json::from_value::<Preferences>(malformed).is_err());

    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let enabled = Preferences {
        telemetry_enabled: true,
        ..defaults
    };
    assert!(enabled.validate().is_ok());
    let saved = store.save(0, enabled).unwrap();
    assert!(saved.preferences.telemetry_enabled);
    assert!(store.load().unwrap().preferences.telemetry_enabled);
}

#[test]
fn translation_account_is_opt_in_and_omitted_until_chosen() {
    let defaults = Preferences::default();
    assert!(!defaults.translation_account);
    // An older strict parser must still read a document that never chose the account.
    let serialized = serde_json::to_value(&defaults).unwrap();
    assert!(serialized.get("translation_account").is_none());
    assert!(
        !serde_json::from_value::<Preferences>(serialized.clone())
            .unwrap()
            .translation_account
    );
    let mut malformed = serialized;
    malformed["translation_account"] = "yes".into();
    assert!(serde_json::from_value::<Preferences>(malformed).is_err());

    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let chosen = Preferences {
        translation_account: true,
        ..defaults
    };
    assert!(chosen.validate().is_ok());
    assert_eq!(
        serde_json::to_value(&chosen).unwrap()["translation_account"],
        serde_json::Value::Bool(true)
    );
    let saved = store.save(0, chosen).unwrap();
    assert!(saved.preferences.translation_account);
    let loaded = store.load().unwrap().preferences;
    assert!(loaded.translation_account);
    assert!(!loaded.restored_to_defaults().translation_account);
}

#[test]
fn secondary_candidate_translation_language_is_optional_and_round_trips() {
    let defaults = Preferences::default();
    let serialized = serde_json::to_value(&defaults).unwrap();
    assert!(!serialized
        .as_object()
        .unwrap()
        .contains_key("translation_secondary_language"));
    let mut enabled = defaults;
    enabled.translation_secondary_language = Some(TranslationTargetLanguage::Ja);
    let encoded = serde_json::to_string(&enabled).unwrap();
    assert_eq!(
        serde_json::from_str::<Preferences>(&encoded)
            .unwrap()
            .translation_secondary_language,
        Some(TranslationTargetLanguage::Ja)
    );
    let mut legacy_value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    legacy_value
        .as_object_mut()
        .unwrap()
        .remove("translation_secondary_language");
    let legacy = serde_json::from_value::<Preferences>(legacy_value).unwrap();
    assert_eq!(legacy.translation_secondary_language, None);
}

#[test]
fn wubi_code_hint_defaults_on_and_legacy_documents_stay_implicit() {
    let defaults = Preferences::default();
    assert!(defaults.wubi_code_hint_enabled());
    let legacy = serde_json::to_value(&defaults).unwrap();
    assert!(!legacy.as_object().unwrap().contains_key("wubi_code_hint"));
    assert!(serde_json::from_value::<Preferences>(legacy)
        .unwrap()
        .wubi_code_hint_enabled());

    let disabled = Preferences {
        wubi_code_hint: Some(false),
        ..defaults
    };
    assert!(
        !serde_json::from_str::<Preferences>(&serde_json::to_string(&disabled).unwrap())
            .unwrap()
            .wubi_code_hint_enabled()
    );
}

#[test]
fn wubi_mixed_pinyin_defaults_off_and_roundtrips() {
    let defaults = Preferences::default();
    assert!(!defaults.wubi_mixed_pinyin);
    let mut legacy = serde_json::to_value(&defaults).unwrap();
    legacy.as_object_mut().unwrap().remove("wubi_mixed_pinyin");
    assert!(
        !serde_json::from_value::<Preferences>(legacy)
            .unwrap()
            .wubi_mixed_pinyin
    );

    let enabled = Preferences {
        wubi_mixed_pinyin: true,
        ..defaults
    };
    assert!(
        serde_json::from_str::<Preferences>(&serde_json::to_string(&enabled).unwrap())
            .unwrap()
            .wubi_mixed_pinyin
    );
}

#[test]
fn diagnostic_logging_defaults_off_and_survives_a_round_trip() {
    let defaults = Preferences::default();
    assert!(!defaults.diagnostic_log.server && !defaults.diagnostic_log.tsf);

    // A configuration written before the field existed keeps logging off
    // rather than starting to write a file the user never asked for.
    let mut document = serde_json::to_value(&defaults).unwrap();
    document.as_object_mut().unwrap().remove("diagnostic_log");
    let legacy: Preferences = serde_json::from_value(document).unwrap();
    assert_eq!(legacy.diagnostic_log, DiagnosticLogPreferences::default());

    // The two hosts are separate processes and are enabled separately.
    let preferences = Preferences {
        diagnostic_log: DiagnosticLogPreferences {
            server: true,
            tsf: false,
        },
        ..Preferences::default()
    };
    let restored: Preferences =
        serde_json::from_str(&serde_json::to_string(&preferences).unwrap()).unwrap();
    assert!(restored.diagnostic_log.server && !restored.diagnostic_log.tsf);

    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let saved = store.save(0, preferences).unwrap();
    assert!(saved.preferences.diagnostic_log.server);
    assert_eq!(
        store.load().unwrap().preferences.diagnostic_log,
        saved.preferences.diagnostic_log
    );
}

#[test]
fn fuzzy_pinyin_preserves_disabled_rules_and_rejects_unknown_ids() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let rules = [FuzzyPinyinRule::ZZh, FuzzyPinyinRule::FH]
        .into_iter()
        .collect();
    let fuzzy_pinyin = FuzzyPinyinPreferences {
        enabled: false,
        rules,
        seeded: false,
    };
    assert_eq!(fuzzy_pinyin.active_rules(), 0);
    let disabled = store
        .save(
            0,
            Preferences {
                fuzzy_pinyin,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(
        disabled.preferences.fuzzy_pinyin.rules,
        [FuzzyPinyinRule::ZZh, FuzzyPinyinRule::FH]
            .into_iter()
            .collect()
    );
    let mut enabled = disabled.preferences;
    enabled.fuzzy_pinyin.enabled = true;
    assert_eq!(enabled.fuzzy_pinyin.active_rules(), (1 << 0) | (1 << 4));
    let enabled = store.save(disabled.revision, enabled).unwrap();
    assert_eq!(store.load().unwrap(), enabled);

    let mut invalid = serde_json::to_value(enabled.preferences).unwrap();
    invalid["fuzzy_pinyin"]["rules"] = serde_json::json!(["z-zh", "unsupported"]);
    assert!(serde_json::from_value::<Preferences>(invalid).is_err());
}

#[test]
fn fuzzy_pinyin_first_enable_seeds_once_and_preserves_pruned_rules() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let disabled = store.save(0, Preferences::default()).unwrap();

    let mut first = disabled.preferences.clone();
    first.fuzzy_pinyin.enabled = true;
    let first = store.save(disabled.revision, first).unwrap();
    assert!(first.preferences.fuzzy_pinyin.seeded);
    assert_eq!(first.preferences.fuzzy_pinyin.rules.len(), 11);

    let mut pruned = first.preferences.clone();
    pruned.fuzzy_pinyin.rules = [FuzzyPinyinRule::ZZh].into_iter().collect();
    let pruned = store.save(first.revision, pruned).unwrap();
    let mut disabled = pruned.preferences.clone();
    disabled.fuzzy_pinyin.enabled = false;
    let disabled = store.save(pruned.revision, disabled).unwrap();
    let mut restored = disabled.preferences.clone();
    restored.fuzzy_pinyin.enabled = true;
    let restored = store.save(disabled.revision, restored).unwrap();
    assert_eq!(
        restored.preferences.fuzzy_pinyin.rules,
        [FuzzyPinyinRule::ZZh].into_iter().collect()
    );

    let mut empty = restored.preferences;
    empty.fuzzy_pinyin.enabled = false;
    empty.fuzzy_pinyin.rules.clear();
    let empty = store.save(restored.revision, empty).unwrap();
    let mut empty_enabled = empty.preferences;
    empty_enabled.fuzzy_pinyin.enabled = true;
    let empty_enabled = store.save(empty.revision, empty_enabled).unwrap();
    assert!(empty_enabled.preferences.fuzzy_pinyin.rules.is_empty());
}

#[test]
fn capture_obeys_shared_enablement_and_preserves_corrupt_history() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let file = directory.path().join("clipboard_history.json");
    let preferences = Preferences {
        clipboard_history: false,
        ..Preferences::default()
    };
    let disabled = store.save(0, preferences).unwrap();
    assert!(!store
        .capture_clipboard_text("synthetic disabled".into())
        .unwrap());
    assert!(!file.exists());
    let mut preferences = disabled.preferences;
    preferences.clipboard_history = true;
    let enabled = store.save(disabled.revision, preferences).unwrap();
    assert!(store
        .capture_clipboard_text("synthetic first".into())
        .unwrap());
    assert!(!store
        .capture_clipboard_text("synthetic\0invalid".into())
        .unwrap());
    let history: Vec<crate::clipboard::ClipboardHistoryEntry> =
        serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "synthetic first");
    let mut preferences = enabled.preferences;
    preferences.clipboard_history = false;
    let disabled = store.save(enabled.revision, preferences).unwrap();
    assert!(!store
        .capture_clipboard_text("synthetic stopped".into())
        .unwrap());
    let history: Vec<crate::clipboard::ClipboardHistoryEntry> =
        serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "synthetic first");
    fs::write(&file, b"broken synthetic document").unwrap();
    let mut preferences = disabled.preferences;
    preferences.clipboard_history = true;
    store.save(disabled.revision, preferences).unwrap();
    assert!(store
        .capture_clipboard_text("synthetic rejected".into())
        .is_err());
    assert_eq!(fs::read(&file).unwrap(), b"broken synthetic document");
}

#[test]
fn default_ime_mode_legacy_defaults_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("default_ime_mode");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), bytes).unwrap();
    // A document written before the field existed takes the platform default: English on Windows, as the source product's factory template, and Chinese everywhere else.
    let expected = if cfg!(windows) {
        DefaultImeMode::English
    } else {
        DefaultImeMode::Chinese
    };
    assert_eq!(DefaultImeMode::default(), expected);
    assert_eq!(Preferences::default().default_ime_mode, expected);
    assert_eq!(store.load().unwrap().preferences.default_ime_mode, expected);
    // An explicit English is still English; only the absent case moved.
    let mut value = serde_json::to_value(Preferences::default()).unwrap();
    value["default_ime_mode"] = "english".into();
    let saved = store
        .save(0, serde_json::from_value(value).unwrap())
        .unwrap();
    assert_eq!(
        store.load().unwrap().preferences.default_ime_mode,
        DefaultImeMode::English
    );
    assert_eq!(saved.preferences.default_ime_mode, DefaultImeMode::English);
}

#[test]
fn local_mode_defaults_and_each_switch_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("local_modes");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert_eq!(
        store.load().unwrap().preferences.local_modes,
        LocalModePreferences::default()
    );
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    for (revision, key) in [
        "unicode",
        "date_time",
        "quick_phrase",
        "emoji",
        "kaomoji",
        "super_jianpin",
        "temporary_english",
        "temporary_japanese",
    ]
    .iter()
    .enumerate()
    {
        let mut value = serde_json::to_value(Preferences::default()).unwrap();
        value["local_modes"][*key] = false.into();
        let saved = store
            .save(revision as u64, serde_json::from_value(value).unwrap())
            .unwrap();
        assert_eq!(store.load().unwrap(), saved);
    }
}

#[test]
fn appearance_preferences_legacy_defaults_and_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    for key in [
        "theme",
        "settings_theme",
        "toolbar_theme",
        "screen_keyboard_theme",
        "touch_keyboard_skin",
        "ui_backend",
        "candidate_follow_cursor",
        "input_mode_hud",
    ] {
        legacy["preferences"].as_object_mut().unwrap().remove(key);
    }
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded, PreferencesSnapshot::default());
    assert_eq!(
        loaded.preferences.quanpin_helpcode,
        default_quanpin_helpcode()
    );
    assert_eq!(
        loaded.preferences.shuangpin_helpcode,
        default_shuangpin_helpcode()
    );
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    let preferences = Preferences {
        theme: ThemeMode::Light,
        settings_theme: SettingsTheme::Dark,
        toolbar_theme: SettingsTheme::Light,
        ui_backend: UiBackend::Webview2,
        candidate_follow_cursor: false,
        input_mode_hud: false,
        ..Preferences::default()
    };
    let saved = store.save(0, preferences).unwrap();
    assert_eq!(store.load().unwrap(), saved);
}

#[test]
fn character_width_defaults_for_legacy_files_and_roundtrips() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("character_width");
    fs::write(store.path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert_eq!(
        store.load().unwrap().preferences.character_width,
        CharacterWidthPreference::Halfwidth
    );

    let saved = store
        .save(
            0,
            Preferences {
                character_width: CharacterWidthPreference::Fullwidth,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(
        store.load().unwrap().preferences.character_width,
        CharacterWidthPreference::Fullwidth
    );
    let document = serde_json::to_value(saved).unwrap();
    assert_eq!(document["preferences"]["character_width"], "fullwidth");

    let mut invalid = document["preferences"].clone();
    invalid["character_width"] = "invalid".into();
    assert!(serde_json::from_value::<Preferences>(invalid).is_err());
}

#[test]
fn toolbar_theme_roundtrips_independently_and_rejects_unknown_values() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    for (revision, toolbar_theme) in [
        SettingsTheme::Dark,
        SettingsTheme::Light,
        SettingsTheme::Follow,
    ]
    .into_iter()
    .enumerate()
    {
        let preferences = Preferences {
            theme: ThemeMode::System,
            settings_theme: SettingsTheme::Light,
            candidate_theme: SettingsTheme::Dark,
            toolbar_theme,
            ..Preferences::default()
        };
        let saved = store.save(revision as u64, preferences).unwrap();
        assert_eq!(store.load().unwrap(), saved);
    }
    let mut invalid = serde_json::to_value(Preferences::default()).unwrap();
    invalid["toolbar_theme"] = "system".into();
    assert!(serde_json::from_value::<Preferences>(invalid).is_err());
}

#[test]
fn screen_keyboard_theme_roundtrips_independently() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    for (revision, screen_keyboard_theme) in [
        SettingsTheme::Dark,
        SettingsTheme::Light,
        SettingsTheme::Follow,
    ]
    .into_iter()
    .enumerate()
    {
        let preferences = Preferences {
            theme: ThemeMode::System,
            toolbar_theme: SettingsTheme::Dark,
            screen_keyboard_theme,
            ..Preferences::default()
        };
        let saved = store.save(revision as u64, preferences).unwrap();
        assert_eq!(store.load().unwrap(), saved);
    }
    let mut invalid = serde_json::to_value(Preferences::default()).unwrap();
    invalid["screen_keyboard_theme"] = "system".into();
    assert!(serde_json::from_value::<Preferences>(invalid).is_err());
}

#[test]
fn touch_keyboard_skin_uses_apple_ordered_ids_and_is_independent() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    for (revision, touch_keyboard_skin) in [
        TouchKeyboardSkin::Forest,
        TouchKeyboardSkin::Ocean,
        TouchKeyboardSkin::Rose,
        TouchKeyboardSkin::Porcelain,
        TouchKeyboardSkin::Typewriter,
        TouchKeyboardSkin::Candy,
        TouchKeyboardSkin::Midnight,
        TouchKeyboardSkin::Blueprint,
        TouchKeyboardSkin::Custom,
    ]
    .into_iter()
    .enumerate()
    {
        let preferences = Preferences {
            candidate_skin: "graphite".to_owned(),
            touch_keyboard_skin,
            ..Preferences::default()
        };
        let saved = store.save(revision as u64, preferences).unwrap();
        assert_eq!(saved.preferences.touch_keyboard_skin, touch_keyboard_skin);
        assert_eq!(saved.preferences.candidate_skin, "graphite");
        assert_eq!(store.load().unwrap(), saved);
    }
    let mut invalid = serde_json::to_value(Preferences::default()).unwrap();
    invalid["touch_keyboard_skin"] = "fluent".into();
    assert!(serde_json::from_value::<Preferences>(invalid).is_err());
}

#[test]
fn custom_touch_keyboard_skin_matches_apple_fields_and_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("custom_touch_keyboard_skin");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert_eq!(
        store.load().unwrap().preferences.custom_touch_keyboard_skin,
        TouchKeyboardSkinDesign::default()
    );
    assert_eq!(fs::read(store.path()).unwrap(), bytes);

    let design = TouchKeyboardSkinDesign {
        background: 0x151022,
        key_background: 0x291E40,
        key_foreground: 0xFFFFFF,
        accent: 0xD4BBFF,
        action_background: 0x69469B,
        corner_radius: 12.0,
        border_width: 1.5,
        shadow: 0.25,
        pattern: 3,
        monospaced: true,
        key_shape: Some(TouchSkinKeyShape::Pebble),
        key_material: Some(TouchSkinKeyMaterial::Glass),
        key_opacity: Some(0.45),
        gradient_end: Some(0x30224A),
        gradient_horizontal: Some(true),
        pattern_opacity: Some(0.2),
        custom_border_color: Some(0xA987E8),
        photo: Some("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".into()),
        photo_shade: Some(0.8),
        photo_position: Some(1.0),
    };
    let preferences = Preferences {
        touch_keyboard_skin: TouchKeyboardSkin::Custom,
        custom_touch_keyboard_skin: design.clone(),
        ..Preferences::default()
    };
    let saved = store.save(0, preferences).unwrap();
    assert_eq!(saved.preferences.custom_touch_keyboard_skin, design);
    let value = serde_json::to_value(&saved.preferences.custom_touch_keyboard_skin).unwrap();
    assert_eq!(value["keyBackground"], 0x291E40);
    assert_eq!(value["keyShape"], "pebble");
    assert!(value.get("key_background").is_none());

    for invalid in [
        TouchKeyboardSkinDesign {
            background: 0x1000000,
            ..TouchKeyboardSkinDesign::default()
        },
        TouchKeyboardSkinDesign {
            corner_radius: 21.0,
            ..TouchKeyboardSkinDesign::default()
        },
        TouchKeyboardSkinDesign {
            key_opacity: Some(0.24),
            ..TouchKeyboardSkinDesign::default()
        },
        TouchKeyboardSkinDesign {
            photo: Some("not-base64".into()),
            ..TouchKeyboardSkinDesign::default()
        },
    ] {
        let mut preferences = saved.preferences.clone();
        preferences.custom_touch_keyboard_skin = invalid;
        assert!(matches!(
            store.save(saved.revision, preferences),
            Err(PreferencesError::InvalidTouchKeyboardSkinDesign)
        ));
        assert_eq!(store.load().unwrap(), saved);
    }
}

#[test]
fn mixed_input_legacy_roundtrip_and_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("mixed_input");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert_eq!(
        store.load().unwrap().preferences.mixed_input,
        MixedInputPreferences::default()
    );
    assert_eq!(
        store.load().unwrap().preferences.mixed_input.minimum_prefix,
        5
    );
    assert_eq!(
        store.load().unwrap().preferences.mixed_input.emoji,
        cfg!(any(windows, target_os = "macos"))
    );
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    for mask in 0..8 {
        let preferences = Preferences {
            mixed_input: MixedInputPreferences {
                english: mask & 1 != 0,
                emoji: mask & 2 != 0,
                kaomoji: mask & 4 != 0,
                minimum_prefix: mask + 1,
            },
            ..Preferences::default()
        };
        let saved = store.save(u64::from(mask), preferences).unwrap();
        assert_eq!(store.load().unwrap(), saved);
    }
    let saved = store.load().unwrap();
    for value in [0, 9, 255] {
        let mut invalid = saved.preferences.clone();
        invalid.mixed_input.minimum_prefix = value;
        assert!(matches!(
            store.save(saved.revision, invalid),
            Err(PreferencesError::InvalidMixedInput)
        ));
        assert_eq!(store.load().unwrap(), saved);
    }
}

#[test]
fn frequency_legacy_defaults_modes_and_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("frequency");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert_eq!(
        store.load().unwrap().preferences.frequency,
        FrequencyPreferences::default()
    );
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    for (revision, mode) in [
        FrequencyMode::Disabled,
        FrequencyMode::Pin,
        FrequencyMode::Halve,
        FrequencyMode::Linear,
        FrequencyMode::Promote,
    ]
    .into_iter()
    .enumerate()
    {
        let saved = store
            .save(
                revision as u64,
                Preferences {
                    frequency: FrequencyPreferences {
                        mode,
                        trigger_count: 10,
                        linear_step: 10,
                    },
                    ..Preferences::default()
                },
            )
            .unwrap();
        assert_eq!(store.load().unwrap(), saved);
    }
    let saved = store.load().unwrap();
    for value in [0, 11, 255] {
        for trigger in [true, false] {
            let mut preferences = Preferences::default();
            if trigger {
                preferences.frequency.trigger_count = value;
            } else {
                preferences.frequency.linear_step = value;
            }
            assert!(matches!(
                store.save(saved.revision, preferences),
                Err(PreferencesError::InvalidFrequency)
            ));
            assert_eq!(store.load().unwrap(), saved);
        }
    }
}

#[test]
fn word_character_legacy_roundtrip_and_conflict_protection() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("word_character");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    let defaults = store.load().unwrap().preferences.word_character;
    assert_eq!(defaults, WordCharacterPreferences::default());
    assert!(defaults.enabled);
    assert_eq!(defaults.keys, WordCharacterKeys::Brackets);
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    for (revision, keys) in [WordCharacterKeys::Brackets, WordCharacterKeys::MinusEqual]
        .into_iter()
        .enumerate()
    {
        let mut preferences = Preferences {
            word_character: WordCharacterPreferences {
                enabled: true,
                keys,
            },
            ..Preferences::default()
        };
        preferences.navigation.brackets = false;
        preferences.navigation.minus_equal = false;
        let saved = store.save(revision as u64, preferences.clone()).unwrap();
        assert_eq!(store.load().unwrap(), saved);
        match keys {
            WordCharacterKeys::Brackets => preferences.navigation.brackets = true,
            WordCharacterKeys::MinusEqual => preferences.navigation.minus_equal = true,
        }
        assert!(matches!(
            store.save(saved.revision, preferences),
            Err(PreferencesError::ConflictingKeyBindings)
        ));
        assert_eq!(store.load().unwrap(), saved);
    }
}

#[test]
fn navigation_accepts_windows_candidate_arrow_alias() {
    let value = serde_json::json!({"minus_equal": true, "comma_period": true, "brackets": false, "tab": true, "page_up_down": true, "mouse_wheel": true, "candidate_arrow_navigation": false});
    let parsed: NavigationPreferences = serde_json::from_value(value).unwrap();
    assert!(!parsed.arrows);
    assert!(parsed.mouse_wheel);
}

#[test]
fn navigation_defaults_and_independent_flags_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("navigation");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert_eq!(
        store.load().unwrap().preferences.navigation,
        NavigationPreferences::default()
    );
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    let preferences = Preferences {
        word_character: WordCharacterPreferences {
            enabled: false,
            keys: WordCharacterKeys::Brackets,
        },
        navigation: NavigationPreferences {
            minus_equal: false,
            comma_period: false,
            brackets: true,
            tab: false,
            page_up_down: false,
            mouse_wheel: false,
            arrows: false,
        },
        ..Preferences::default()
    };
    let saved = store.save(0, preferences).unwrap();
    assert_eq!(store.load().unwrap(), saved);
    let mut invalid = serde_json::to_value(saved).unwrap();
    invalid["preferences"]["navigation"]["tab"] = "invalid".into();
    let bytes = serde_json::to_vec(&invalid).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert!(store.save(1, Preferences::default()).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
}

#[test]
fn number_row_selection_defaults_on_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("number_row_selection");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert!(store.load().unwrap().preferences.number_row_selection);
    assert_eq!(fs::read(store.path()).unwrap(), bytes);

    let saved = store
        .save(
            0,
            Preferences {
                number_row_selection: false,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert!(!saved.preferences.number_row_selection);
    assert_eq!(store.load().unwrap(), saved);
}

#[test]
fn remembered_chinese_scheme_roundtrips_without_changing_legacy_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let legacy = serde_json::to_vec(&PreferencesSnapshot::default()).unwrap();
    fs::write(store.path(), &legacy).unwrap();
    assert_eq!(store.load().unwrap().preferences.last_chinese_scheme, None);
    assert_eq!(fs::read(store.path()).unwrap(), legacy);
    for (revision, scheme) in [
        ChineseScheme::Quanpin,
        ChineseScheme::Shuangpin,
        ChineseScheme::Wubi,
    ]
    .into_iter()
    .enumerate()
    {
        let saved = store
            .save(
                revision as u64,
                Preferences {
                    scheme: InputScheme::Japanese,
                    last_chinese_scheme: Some(scheme),
                    ..Preferences::default()
                },
            )
            .unwrap();
        assert_eq!(store.load().unwrap(), saved);
    }
    let mut invalid = serde_json::to_value(store.load().unwrap()).unwrap();
    invalid["preferences"]["last_chinese_scheme"] = "japanese".into();
    let bytes = serde_json::to_vec(&invalid).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert!(store.save(3, Preferences::default()).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
}

#[test]
fn touch_keyboard_layout_defaults_and_roundtrips_without_rewriting_legacy_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("touch_keyboard_layout");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert_eq!(
        store.load().unwrap().preferences.touch_keyboard_layout,
        TouchKeyboardLayout::TwentySixKey
    );
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    store
        .save(
            0,
            Preferences {
                touch_keyboard_layout: TouchKeyboardLayout::NineKey,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(
        store.load().unwrap().preferences.touch_keyboard_layout,
        TouchKeyboardLayout::NineKey
    );
    let saved = store
        .save(
            1,
            Preferences {
                touch_keyboard_layout: TouchKeyboardLayout::Handwriting,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(
        store.load().unwrap().preferences.touch_keyboard_layout,
        TouchKeyboardLayout::Handwriting
    );
    let mut invalid = serde_json::to_value(saved).unwrap();
    invalid["preferences"]["touch_keyboard_layout"] = "future_layout".into();
    let bytes = serde_json::to_vec(&invalid).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert!(store.load().is_err());
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
}

#[test]
fn touch_keyboard_scheme_visibility_matches_apple_order_and_fallback_contract() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let legacy = serde_json::to_vec(&PreferencesSnapshot::default()).unwrap();
    fs::write(store.path(), &legacy).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(
        loaded.preferences.touch_keyboard_schemes.enabled,
        TouchKeyboardScheme::ALL.into_iter().collect()
    );
    assert_eq!(loaded.preferences.touch_keyboard_schemes.selected, None);
    assert_eq!(fs::read(store.path()).unwrap(), legacy);

    let visible = [
        TouchKeyboardScheme::NineKey,
        TouchKeyboardScheme::Handwriting,
    ]
    .into_iter()
    .collect();
    let saved = store
        .save(
            0,
            Preferences {
                touch_keyboard_schemes: TouchKeyboardSchemePreferences {
                    enabled: visible,
                    selected: Some(TouchKeyboardScheme::Handwriting),
                },
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(store.load().unwrap(), saved);

    for value in [
        serde_json::json!({"enabled": [], "selected": null}),
        serde_json::json!({"enabled": ["nine_key"], "selected": "handwriting"}),
    ] {
        let mut invalid = serde_json::to_value(&saved).unwrap();
        invalid["preferences"]["touch_keyboard_schemes"] = value;
        let bytes = serde_json::to_vec(&invalid).unwrap();
        fs::write(store.path(), &bytes).unwrap();
        assert!(matches!(
            store.save(1, Preferences::default()),
            Err(PreferencesError::InvalidTouchKeyboardSchemes)
        ));
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }

    let mut unknown = serde_json::to_value(&saved).unwrap();
    unknown["preferences"]["touch_keyboard_schemes"]["enabled"] =
        serde_json::json!(["nine_key", "future_scheme"]);
    let bytes = serde_json::to_vec(&unknown).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert!(store.load().is_err());
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
}

#[test]
fn touch_keyboard_spacing_uses_apple_defaults_bounds_and_legacy_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    for key in [
        "touch_key_spacing_tenths",
        "touch_row_spacing_tenths",
        "touch_keyboard_height_adjustment",
        "touch_voice_shortcut",
    ] {
        legacy["preferences"].as_object_mut().unwrap().remove(key);
    }
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.preferences.touch_key_spacing_tenths, 60);
    assert_eq!(loaded.preferences.touch_row_spacing_tenths, 70);
    assert_eq!(loaded.preferences.touch_keyboard_height_adjustment, 0);
    assert!(!loaded.preferences.touch_voice_shortcut);
    assert_eq!(fs::read(store.path()).unwrap(), bytes);

    let saved = store
        .save(
            0,
            Preferences {
                touch_key_spacing_tenths: 35,
                touch_row_spacing_tenths: 95,
                touch_keyboard_height_adjustment: 24,
                touch_voice_shortcut: true,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(saved.preferences.touch_key_spacing_tenths, 35);
    assert_eq!(saved.preferences.touch_row_spacing_tenths, 95);
    assert_eq!(saved.preferences.touch_keyboard_height_adjustment, 24);
    assert!(saved.preferences.touch_voice_shortcut);

    for (key, value) in [
        ("touch_key_spacing_tenths", 29),
        ("touch_key_spacing_tenths", 61),
        ("touch_row_spacing_tenths", 39),
        ("touch_row_spacing_tenths", 101),
    ] {
        let mut invalid = saved.preferences.clone();
        match key {
            "touch_key_spacing_tenths" => invalid.touch_key_spacing_tenths = value,
            _ => invalid.touch_row_spacing_tenths = value,
        }
        assert!(matches!(
            invalid.validate(),
            Err(PreferencesError::InvalidTouchKeyboardSpacing)
        ));
    }

    for value in [-13, 49] {
        let mut invalid = saved.preferences.clone();
        invalid.touch_keyboard_height_adjustment = value;
        assert!(matches!(
            invalid.validate(),
            Err(PreferencesError::InvalidTouchKeyboardSpacing)
        ));
    }
}

#[test]
fn helpcode_legacy_defaults_and_independent_schemes_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    for key in ["quanpin_helpcode", "shuangpin_helpcode"] {
        legacy["preferences"].as_object_mut().unwrap().remove(key);
    }
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert_eq!(store.load().unwrap(), PreferencesSnapshot::default());
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    for (revision, schema) in [
        HelpcodeSchema::Lantian,
        HelpcodeSchema::Ziranma,
        HelpcodeSchema::Shouyou2,
        HelpcodeSchema::Shouyouplus,
        HelpcodeSchema::Xiaohe,
    ]
    .into_iter()
    .enumerate()
    {
        let preferences = Preferences {
            quanpin_helpcode: HelpcodePreferences {
                enabled: false,
                schema,
                show_in_candidate_window: true,
            },
            ..Preferences::default()
        };
        let saved = store.save(revision as u64, preferences).unwrap();
        assert_eq!(store.load().unwrap(), saved);
        assert_eq!(
            saved.preferences.shuangpin_helpcode,
            default_shuangpin_helpcode()
        );
    }
    let unknown = fs::read_to_string(store.path())
        .unwrap()
        .replace("xiaohe", "unknown");
    fs::write(store.path(), &unknown).unwrap();
    assert!(store.save(5, Preferences::default()).is_err());
    assert_eq!(fs::read_to_string(store.path()).unwrap(), unknown);
}

#[test]
fn autocorrect_legacy_default_and_disabled_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("autocorrect");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    let loaded = store.load().unwrap();
    assert!(loaded.preferences.autocorrect);
    assert!(loaded.preferences.quanpin_autocorrect_transposition());
    assert!(loaded.preferences.quanpin_autocorrect_neighbor());
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    let preferences = Preferences {
        autocorrect: false,
        ..Preferences::default()
    };
    let saved = store.save(0, preferences).unwrap();
    assert!(!saved.preferences.autocorrect);
    assert!(saved.preferences.quanpin_autocorrect_transposition());
    assert!(saved.preferences.quanpin_autocorrect_neighbor());

    let explicit = Preferences {
        autocorrect: true,
        quanpin: QuanpinPreferences {
            autocorrect_transposition: Some(true),
            autocorrect_neighbor: Some(false),
        },
        ..Preferences::default()
    };
    let saved = store.save(saved.revision, explicit).unwrap();
    assert!(saved.preferences.quanpin_autocorrect_transposition());
    assert!(!saved.preferences.quanpin_autocorrect_neighbor());
}

#[test]
fn traditional_chinese_output_legacy_default_and_enabled_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("traditional_chinese_output");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    assert!(!store.load().unwrap().preferences.traditional_chinese_output);
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    let preferences = Preferences {
        traditional_chinese_output: true,
        ..Preferences::default()
    };
    store.save(0, preferences).unwrap();
    assert!(store.load().unwrap().preferences.traditional_chinese_output);
}

#[test]
fn shuangpin_profiles_preserve_legacy_files_and_reject_unknown_values() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let legacy = r#"{"format_version":1,"revision":7,"preferences":{"scheme":"shuangpin","candidate_page_size":5,"learning":false,"chinese_punctuation":true}}"#;
    fs::write(store.path(), legacy).unwrap();
    assert_eq!(
        store.load().unwrap().preferences.shuangpin_profile,
        ShuangpinProfile::Xiaohe
    );
    assert_eq!(fs::read_to_string(store.path()).unwrap(), legacy);
    let mut revision = 7;
    for profile in [
        ShuangpinProfile::Xiaohe,
        ShuangpinProfile::Ziranma,
        ShuangpinProfile::Shoudao,
        ShuangpinProfile::Microsoft,
    ] {
        let saved = store
            .save(
                revision,
                Preferences {
                    scheme: InputScheme::Shuangpin,
                    shuangpin_profile: profile,
                    ..Preferences::default()
                },
            )
            .unwrap();
        assert_eq!(store.load().unwrap(), saved);
        revision = saved.revision;
    }
    let unknown = fs::read_to_string(store.path())
        .unwrap()
        .replace("microsoft", "future_profile");
    fs::write(store.path(), &unknown).unwrap();
    assert!(store.load().is_err());
    assert!(store.save(revision, Preferences::default()).is_err());
    assert_eq!(fs::read_to_string(store.path()).unwrap(), unknown);
}

#[test]
fn try_load_distinguishes_busy_missing_and_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    assert_eq!(
        store.try_load().unwrap(),
        Some(PreferencesSnapshot::default())
    );
    let lock = store.lock().unwrap();
    assert_eq!(store.try_load().unwrap(), None);
    drop(lock);
    let saved = store.save(0, Preferences::default()).unwrap();
    assert_eq!(store.try_load().unwrap(), Some(saved));
    fs::write(store.path(), "broken").unwrap();
    assert!(store.try_load().is_err());
    assert_eq!(fs::read_to_string(store.path()).unwrap(), "broken");
}

#[test]
fn persists_across_instances_and_rejects_stale_save() {
    let dir = tempfile::tempdir().unwrap();
    let first = PreferencesStore::new(dir.path());
    let second = PreferencesStore::new(dir.path());
    let preferences = Preferences {
        learning: false,
        ..Preferences::default()
    };
    let saved = first.save(0, preferences).unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(second.load().unwrap(), saved);
    assert!(matches!(
        second.save(0, Preferences::default()),
        Err(PreferencesError::Conflict)
    ));
    assert_eq!(first.load().unwrap(), saved);
}

#[test]
fn invalid_values_do_not_change_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let initial = store.save(0, Preferences::default()).unwrap();
    for size in [0, 10, 255] {
        assert!(matches!(
            store.save(
                1,
                Preferences {
                    candidate_page_size: size,
                    ..Preferences::default()
                }
            ),
            Err(PreferencesError::InvalidPageSize)
        ));
    }
    assert_eq!(store.load().unwrap(), initial);
}

#[test]
fn ai_assistant_rejects_unknown_provider_and_invalid_candidate_limit() {
    let mut preferences = Preferences::default();
    for provider in AI_PROVIDERS {
        preferences.ai_assistant.provider = provider.into();
        assert!(
            preferences.validate().is_ok(),
            "{provider} should be accepted"
        );
    }
    preferences.ai_assistant.provider = "unknown".into();
    assert!(matches!(
        preferences.validate(),
        Err(PreferencesError::InvalidAiAssistant)
    ));
    preferences.ai_assistant.provider = "openai".into();
    preferences.ai_assistant.candidate_limit = 11;
    assert!(matches!(
        preferences.validate(),
        Err(PreferencesError::InvalidAiAssistant)
    ));
}

#[test]
fn default_ai_assistant_requests_carry_the_builtin_associative_prompt() {
    // The stored slot stays empty on purpose. Android's keyboard and the iOS keyboard mirror read `ai_assistant.prompt` as their polish instruction and substitute a polish prompt only when it is blank, so storing the Windows associative text here would turn their polish answers into candidate JSON. Blank means "use the built-in prompt" at request time instead: here, and in the Linux provider's copy of the same text.
    let mut ai = Preferences::default().ai_assistant;
    assert!(ai.prompt.is_empty());
    ai.enabled = true;
    ai.endpoint = "https://synthetic.invalid/chat".into();
    ai.model = "synthetic-model".into();
    ai.token = "synthetic-token".into();
    let request = crate::ai::AiSuggestionRequest {
        segmented_pinyin: vec!["shu".into(), "ru".into()],
        context: String::new(),
        candidate_limit: ai.candidate_limit,
    };
    let descriptor = crate::ai::chat_completion_http_request(&ai, &request)
        .unwrap()
        .unwrap();
    let system = &descriptor["body"]["messages"][0];
    assert_eq!(system["role"], "system");
    assert_eq!(system["content"], crate::ai::DEFAULT_CANDIDATE_PROMPT);
    assert!(crate::ai::DEFAULT_CANDIDATE_PROMPT.contains("JSON"));
    assert!(crate::ai::DEFAULT_CANDIDATE_PROMPT.contains("\"candidates\""));
}

#[test]
fn candidate_font_size_bounds_are_strict() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let initial = store.save(0, Preferences::default()).unwrap();
    for size in [11, 33, 255] {
        assert!(matches!(
            store.save(
                1,
                Preferences {
                    candidate_font_size: size,
                    ..Preferences::default()
                }
            ),
            Err(PreferencesError::InvalidCandidateFontSize)
        ));
    }
    let saved = store
        .save(
            1,
            Preferences {
                candidate_font_size: 12,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(saved.preferences.candidate_font_size, 12);
    let other_dir = tempfile::tempdir().unwrap();
    let other = PreferencesStore::new(other_dir.path());
    other.save(0, Preferences::default()).unwrap();
    let saved = other
        .save(
            1,
            Preferences {
                candidate_font_size: 32,
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(saved.preferences.candidate_font_size, 32);
    assert_eq!(initial.preferences.candidate_font_size, 18);
    assert_eq!(initial.preferences.candidate_preedit_font_size, 15);
    assert_eq!(initial.preferences.candidate_page_size, 6);
    assert_eq!(initial.preferences.candidate_skin, "willow_green");
    assert_eq!(initial.preferences.theme, ThemeMode::System);
    assert_eq!(initial.preferences.candidate_font_family, "Noto Sans SC");
    assert_eq!(
        initial.preferences.candidate_fallback_fonts,
        vec!["Noto Sans SC".to_owned(), "Microsoft YaHei".to_owned()]
    );
}

#[test]
fn candidate_appearance_colors_accept_hex_and_reject_unsafe_values() {
    let mut preferences = Preferences::default();
    macro_rules! check {
        ($field:ident) => {{
            preferences.$field = Some("#12aBcD".to_owned());
            assert!(preferences.validate().is_ok());
            preferences.$field = Some("#12345678".to_owned());
            assert!(preferences.validate().is_err());
            preferences.$field = None;
        }};
    }
    check!(candidate_text_color);
    check!(candidate_number_color);
    check!(candidate_accent_color);
    check!(candidate_selected_color);
    check!(candidate_hover_color);
    check!(candidate_surface_color);
    check!(candidate_border_color);
}

#[test]
fn candidate_skin_ids_are_safe_and_bounded() {
    let mut preferences = Preferences::default();
    for skin in ["fluent", "willow_green", "external.skin-1"] {
        preferences.candidate_skin = skin.to_owned();
        assert!(preferences.validate().is_ok(), "{skin}");
    }
    for skin in ["", "-unsafe", "Upper", "../escape", &"a".repeat(65)] {
        preferences.candidate_skin = skin.to_owned();
        assert!(
            matches!(
                preferences.validate(),
                Err(PreferencesError::InvalidCandidateSkin)
            ),
            "{skin}"
        );
    }
}

#[test]
fn unicode_font_families_and_ordered_fallbacks_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let fallback_fonts: Vec<String> = (0..32).map(|index| format!("示例字体{index}")).collect();
    let preferences = Preferences {
        candidate_font_family: "示例主字体".to_owned(),
        candidate_fallback_fonts: fallback_fonts.clone(),
        ..Preferences::default()
    };
    let saved = store.save(0, preferences).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.preferences.candidate_font_family, "示例主字体");
    assert_eq!(loaded.preferences.candidate_fallback_fonts, fallback_fonts);
    assert_eq!(loaded.revision, saved.revision);
}

#[test]
fn candidate_fallback_fonts_reject_invalid_lists() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let initial = store.save(0, Preferences::default()).unwrap();
    for fonts in [
        vec!["".to_owned()],
        vec!["a".repeat(129)],
        vec!["字".repeat(43)],
        (0..33).map(|index| format!("Font{index}")).collect(),
    ] {
        assert!(matches!(
            store.save(
                1,
                Preferences {
                    candidate_fallback_fonts: fonts,
                    ..Preferences::default()
                }
            ),
            Err(PreferencesError::InvalidCandidateFontFamily)
        ));
    }
    let valid = vec!["Noto Sans CJK SC".to_owned(); 32];
    let saved = store
        .save(
            1,
            Preferences {
                candidate_fallback_fonts: valid.clone(),
                ..Preferences::default()
            },
        )
        .unwrap();
    assert_eq!(saved.preferences.candidate_fallback_fonts, valid);
    assert_eq!(store.load().unwrap().revision, 2);
    assert_eq!(initial.revision, 1);
}

#[test]
fn unicode_font_names_retain_utf8_byte_budget() {
    let exact_limit = format!("{}ab", "字".repeat(42));
    assert_eq!(exact_limit.len(), 128);
    let mut preferences = Preferences {
        candidate_font_family: exact_limit.clone(),
        candidate_fallback_fonts: vec![exact_limit.clone()],
        ..Preferences::default()
    };
    assert!(preferences.validate().is_ok());
    for invalid in [String::new(), format!("{exact_limit}c"), "字".repeat(43)] {
        preferences.candidate_font_family = invalid;
        assert!(matches!(
            preferences.validate(),
            Err(PreferencesError::InvalidCandidateFontFamily)
        ));
    }
}

#[test]
fn candidate_font_names_reject_control_characters() {
    for invalid in ["Primary\nFont", "Primary\u{7f}Font", "Primary\u{85}Font"] {
        let mut preferences = Preferences {
            candidate_font_family: invalid.to_owned(),
            ..Preferences::default()
        };
        assert!(matches!(
            preferences.validate(),
            Err(PreferencesError::InvalidCandidateFontFamily)
        ));

        preferences.candidate_font_family = default_candidate_font_family();
        preferences.candidate_english_font = Some(invalid.to_owned());
        assert!(matches!(
            preferences.validate(),
            Err(PreferencesError::InvalidCandidateFontFamily)
        ));

        preferences.candidate_english_font = None;
        preferences.candidate_fallback_fonts = vec![invalid.to_owned()];
        assert!(matches!(
            preferences.validate(),
            Err(PreferencesError::InvalidCandidateFontFamily)
        ));
    }
}

#[test]
fn malformed_future_and_unknown_documents_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    for bytes in ["broken".to_owned(), serde_json::to_string(&PreferencesSnapshot { format_version: 2, ..PreferencesSnapshot::default() }).unwrap(),
        r#"{"format_version":1,"revision":0,"preferences":{"scheme":"quanpin","candidate_page_size":5,"learning":true,"chinese_punctuation":true,"future_option":true}}"#.to_owned()] {
        fs::write(store.path(), &bytes).unwrap();
        assert!(store.save(0, Preferences::default()).is_err());
        assert_eq!(fs::read_to_string(store.path()).unwrap(), bytes);
    }
}

#[test]
fn concurrent_stores_have_exactly_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            let store = PreferencesStore::new(dir.path());
            std::thread::spawn(move || {
                barrier.wait();
                store.save(0, Preferences::default())
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(PreferencesError::Conflict)))
            .count(),
        7
    );
}

#[test]
fn floating_toolbar_component_defaults_and_roundtrip() {
    let defaults = Preferences::default().floating_toolbar;
    assert!(
        defaults.enabled && defaults.english_mode && defaults.fullwidth && defaults.punctuation
    );
    assert!(defaults.character_set && defaults.emoji && defaults.settings);
    assert!(!defaults.screen_keyboard);
    let json = serde_json::to_string(&Preferences::default()).unwrap();
    let restored: Preferences = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.floating_toolbar, defaults);
}

#[test]
fn legacy_preferences_without_toolbar_use_component_defaults() {
    let mut value = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    value["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("floating_toolbar");
    let restored: PreferencesSnapshot = serde_json::from_value(value).unwrap();
    assert!(restored.preferences.floating_toolbar.enabled);
    assert!(restored.preferences.floating_toolbar.english_mode);
    assert!(restored.preferences.floating_toolbar.fullwidth);
    assert!(!restored.preferences.floating_toolbar.screen_keyboard);
}

#[test]
fn tencent_translation_defaults_migrate_and_round_trip() {
    let defaults = Preferences::default();
    assert!(defaults.tencent_tmt.enabled);
    assert!(defaults.tencent_tmt.secret_id.is_empty());
    assert!(defaults.tencent_tmt.secret_key.is_empty());
    assert_eq!(defaults.tencent_tmt.region, "ap-guangzhou");
    let mut legacy = serde_json::to_value(&defaults).unwrap();
    legacy.as_object_mut().unwrap().remove("tencent_tmt");
    let restored: Preferences = serde_json::from_value(legacy).unwrap();
    assert_eq!(restored.tencent_tmt, defaults.tencent_tmt);
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut configured = defaults;
    configured.tencent_tmt.secret_id = "AKIDsynthetic".into();
    configured.tencent_tmt.secret_key = "synthetic".into();
    configured.tencent_tmt.region = "ap-shanghai".into();
    configured.candidate_page_size = 7;
    store.save(0, configured.clone()).unwrap();
    assert_eq!(store.load().unwrap().preferences, configured);
}

#[test]
fn tencent_translation_rejects_unbounded_or_injected_parameters() {
    for (id, key, region) in [
        ("x".repeat(4097), String::new(), String::new()),
        (String::new(), "x".repeat(4097), String::new()),
        ("bad id".into(), String::new(), String::new()),
        (String::new(), "bad\r\nkey".into(), String::new()),
        (String::new(), String::new(), "x".repeat(65)),
        (String::new(), String::new(), "region\r\nheader".into()),
    ] {
        let mut preferences = Preferences::default();
        preferences.tencent_tmt.secret_id = id;
        preferences.tencent_tmt.secret_key = key;
        preferences.tencent_tmt.region = region;
        assert!(matches!(
            preferences.validate(),
            Err(PreferencesError::InvalidTencentTmt)
        ));
    }
}

#[test]
fn niutrans_translation_defaults_migrate_and_validate() {
    let defaults = Preferences::default();
    assert!(!defaults.niutrans.enabled);
    let mut legacy = serde_json::to_value(&defaults).unwrap();
    legacy.as_object_mut().unwrap().remove("niutrans");
    let restored: Preferences = serde_json::from_value(legacy).unwrap();
    assert_eq!(restored.niutrans, defaults.niutrans);
    let mut configured = defaults.clone();
    configured.niutrans.enabled = true;
    configured.niutrans.app_id = "synthetic-app".into();
    configured.niutrans.apikey = "synthetic-key".into();
    assert!(configured.validate().is_ok());
    for (app_id, apikey) in [
        ("x".repeat(4097), String::new()),
        (String::new(), "x".repeat(4097)),
        ("bad\napp".into(), String::new()),
        (String::new(), "bad\rkey".into()),
        ("<YOUR_APP_ID>".into(), String::new()),
        (String::new(), "FAKESECRET_key".into()),
    ] {
        let mut invalid = defaults.clone();
        invalid.niutrans.app_id = app_id;
        invalid.niutrans.apikey = apikey;
        assert!(matches!(
            invalid.validate(),
            Err(PreferencesError::InvalidNiuTrans)
        ));
    }
}

#[test]
fn custom_translation_defaults_and_validation_are_stable() {
    let defaults = Preferences::default();
    assert!(!defaults.custom_translation.enabled);
    assert!(defaults.custom_translation.endpoint.is_empty());
    let json = serde_json::to_string(&defaults).unwrap();
    let restored: Preferences = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.custom_translation, defaults.custom_translation);

    let mut valid = defaults.clone();
    valid.custom_translation.api_key = "masked-test-key".into();
    for endpoint in [
        "https://translate.example/api",
        "http://127.0.0.1:1188/translate",
        "http://[::1]:1188/translate",
        "http://translate.example/api",
    ] {
        valid.custom_translation.endpoint = endpoint.into();
        assert!(valid.validate().is_ok());
    }

    for endpoint in [
        "ftp://translate.example/api",
        "https://translate.example/\napi",
    ] {
        let mut invalid = defaults.clone();
        invalid.custom_translation.endpoint = endpoint.into();
        assert!(matches!(
            invalid.validate(),
            Err(PreferencesError::InvalidCustomTranslation)
        ));
    }
    let mut oversized = defaults;
    oversized.custom_translation.api_key = "x".repeat(4097);
    assert!(matches!(
        oversized.validate(),
        Err(PreferencesError::InvalidCustomTranslation)
    ));
}

// The settings page has five smart-punctuation toggles, and the Windows server
// reads all five out of the saved document. Three of them had no field here,
// and `deny_unknown_fields` means an unknown key does not get dropped - the
// whole save is rejected. Turning any one of them on therefore stopped the
// settings window saving anything at all.
#[test]
fn smart_punctuation_sub_switches_survive_a_save() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let keys = [
        "smart_punctuation_space_convert",
        "smart_punctuation_direct_digit",
        "smart_punctuation_direct_letter",
    ];

    // A document that predates them reads them as their defaults, which is what the family's
    // parent says: the two halves of 智能标点 follow it, and the space rewrite does not.
    let following_parent = !cfg!(any(windows, target_os = "macos"));
    let defaults = serde_json::to_value(Preferences::default()).unwrap();
    assert_eq!(
        defaults["smart_punctuation_space_convert"],
        serde_json::Value::Bool(false)
    );
    for key in [
        "smart_punctuation_direct_digit",
        "smart_punctuation_direct_letter",
    ] {
        assert_eq!(
            defaults[key],
            serde_json::Value::Bool(following_parent),
            "{key}"
        );
    }
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    for key in keys {
        legacy["preferences"]
            .as_object_mut()
            .unwrap()
            .remove(key)
            .unwrap();
    }
    fs::write(store.path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
    let loaded = store.load().unwrap().preferences;
    assert!(!loaded.smart_punctuation_space_convert);
    assert_eq!(loaded.smart_punctuation_direct_digit, following_parent);
    assert_eq!(loaded.smart_punctuation_direct_letter, following_parent);

    // One at a time, from all three off, so a key that was written into the wrong field shows up
    // as its neighbour turning on rather than being hidden by a default that already said true.
    for (revision, key) in keys.iter().enumerate() {
        let mut value = serde_json::to_value(Preferences::default()).unwrap();
        for other in keys {
            value[other] = false.into();
        }
        value[*key] = true.into();
        let saved = store
            .save(revision as u64, serde_json::from_value(value).unwrap())
            .unwrap();
        assert_eq!(
            saved.preferences.smart_punctuation_space_convert,
            *key == keys[0]
        );
        assert_eq!(
            saved.preferences.smart_punctuation_direct_digit,
            *key == keys[1]
        );
        assert_eq!(
            saved.preferences.smart_punctuation_direct_letter,
            *key == keys[2]
        );
        assert_eq!(store.load().unwrap(), saved);
    }
}

// The source ships every smart-punctuation switch disabled, and the running host reads this document rather than the installed template. A fresh Windows or macOS profile must therefore start with the family off, macOS following the desktop product it ports; Linux, Android, iOS and HarmonyOS keep what they have shipped. A stored value is never reinterpreted either way.
#[test]
fn smart_punctuation_first_run_follows_the_source_on_desktop_ports() {
    let expected = !cfg!(any(windows, target_os = "macos"));
    let defaults = Preferences::default();
    assert_eq!(defaults.smart_punctuation, expected);
    assert_eq!(defaults.smart_punctuation_repeat, expected);
    // The two halves of 智能标点 follow it. The reference has no such halves - one switch there
    // means "ASCII after a letter or a digit", which is the sentence this page shows under the
    // parent - so with them off the parent was on out of the box and did nothing.
    assert_eq!(defaults.smart_punctuation_direct_digit, expected);
    assert_eq!(defaults.smart_punctuation_direct_letter, expected);
    // Space-after-punctuation is not one of those halves: it rewrites a character the user already
    // saw land, and the reference has no equivalent at all, so it stays off until asked for.
    assert!(!defaults.smart_punctuation_space_convert);

    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    let preferences = legacy["preferences"].as_object_mut().unwrap();
    preferences.remove("smart_punctuation").unwrap();
    preferences.remove("smart_punctuation_repeat").unwrap();
    fs::write(store.path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
    let loaded = store.load().unwrap().preferences;
    assert_eq!(loaded.smart_punctuation, expected);
    assert_eq!(loaded.smart_punctuation_repeat, expected);

    // A document that states the opposite keeps stating it.
    let mut stored = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    stored["preferences"]["smart_punctuation"] = (!expected).into();
    stored["preferences"]["smart_punctuation_repeat"] = (!expected).into();
    fs::write(store.path(), serde_json::to_vec(&stored).unwrap()).unwrap();
    let loaded = store.load().unwrap().preferences;
    assert_eq!(loaded.smart_punctuation, !expected);
    assert_eq!(loaded.smart_punctuation_repeat, !expected);
}

// The source ships muting, DDC and text polishing on; `config.default.toml` says so for Windows, and macOS follows the desktop product it ports. Each is a plain voice switch, so no platform reason keeps them off there. The other hosts keep what they have shipped, and a stored value is never reinterpreted.
#[test]
fn voice_first_run_follows_the_source_on_desktop_ports() {
    let expected = cfg!(any(windows, target_os = "macos"));
    let voice = Preferences::default().voice_input;
    assert_eq!(voice.mute_system_audio, expected);
    assert_eq!(voice.doubao_enable_ddc, expected);
    assert_eq!(voice.polish_text, expected);
    // Polishing the text is not the same switch as the separate polish pass; that one stays off.
    assert!(!voice.polish_enabled);

    let keys = ["mute_system_audio", "doubao_enable_ddc", "polish_text"];
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    let section = legacy["preferences"]["voice_input"]
        .as_object_mut()
        .unwrap();
    for key in keys {
        section.remove(key).unwrap();
    }
    fs::write(store.path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
    let loaded = store.load().unwrap().preferences.voice_input;
    assert_eq!(loaded.mute_system_audio, expected);
    assert_eq!(loaded.doubao_enable_ddc, expected);
    assert_eq!(loaded.polish_text, expected);

    let mut stored = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    for key in keys {
        stored["preferences"]["voice_input"][key] = (!expected).into();
    }
    fs::write(store.path(), serde_json::to_vec(&stored).unwrap()).unwrap();
    let loaded = store.load().unwrap().preferences.voice_input;
    assert_eq!(loaded.mute_system_audio, !expected);
    assert_eq!(loaded.doubao_enable_ddc, !expected);
    assert_eq!(loaded.polish_text, !expected);
}

// The source template ships voice polishing through DeepSeek (`deepseek-v4-flash`) and the AI assistant on against the same endpoint and model; the Windows installer template says the same, and macOS follows the desktop product it ports. The other hosts keep SiliconFlow/Qwen and the assistant off, and a stored value is never reinterpreted.
#[test]
fn polish_and_ai_first_run_follow_the_source_on_desktop_ports() {
    let desktop = cfg!(any(windows, target_os = "macos"));
    let defaults = Preferences::default();
    defaults.validate().expect("default preferences validate");
    let voice = &defaults.voice_input;
    let ai = &defaults.ai_assistant;
    if desktop {
        assert_eq!(voice.polish_provider, "deepseek");
        assert_eq!(
            voice.polish_endpoint,
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(voice.polish_model, "deepseek-v4-flash");
        assert!(ai.enabled);
        assert_eq!(ai.endpoint, "https://api.deepseek.com/chat/completions");
        assert_eq!(ai.model, "deepseek-v4-flash");
    } else {
        assert_eq!(voice.polish_provider, "siliconflow");
        assert_eq!(
            voice.polish_endpoint,
            "https://api.siliconflow.cn/v1/chat/completions"
        );
        assert_eq!(voice.polish_model, "Qwen/Qwen3-8B");
        assert!(!ai.enabled);
        assert!(ai.endpoint.is_empty());
        assert!(ai.model.is_empty());
    }
    assert_eq!(ai.provider, "deepseek");
    // No token ships, so turning the assistant on sends nothing until the user adds one.
    assert!(ai.token.is_empty() && ai.tokens.is_empty());
    assert!(voice.polish_token.is_empty() && voice.polish_tokens.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]["ai_assistant"]
        .as_object_mut()
        .unwrap()
        .remove("enabled")
        .unwrap();
    fs::write(store.path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
    let loaded = store.load().unwrap().preferences;
    assert_eq!(loaded.ai_assistant.enabled, desktop);

    let mut stored = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    stored["preferences"]["ai_assistant"]["enabled"] = false.into();
    stored["preferences"]["voice_input"]["polish_provider"] = "siliconflow".into();
    stored["preferences"]["voice_input"]["polish_endpoint"] =
        "https://api.siliconflow.cn/v1/chat/completions".into();
    stored["preferences"]["voice_input"]["polish_model"] = "Qwen/Qwen3-8B".into();
    fs::write(store.path(), serde_json::to_vec(&stored).unwrap()).unwrap();
    let loaded = store.load().unwrap().preferences;
    assert!(!loaded.ai_assistant.enabled);
    assert_eq!(loaded.voice_input.polish_provider, "siliconflow");
    assert_eq!(
        loaded.voice_input.polish_endpoint,
        "https://api.siliconflow.cn/v1/chat/completions"
    );
    assert_eq!(loaded.voice_input.polish_model, "Qwen/Qwen3-8B");
}

#[test]
fn mixed_emoji_first_run_follows_the_source_on_desktop_ports() {
    let expected = cfg!(any(windows, target_os = "macos"));
    let mixed = Preferences::default().mixed_input;
    assert_eq!(mixed.emoji, expected);
    // The source ships kaomoji mixed-input off on every platform.
    assert!(!mixed.kaomoji);

    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut stored = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    stored["preferences"]["mixed_input"]["emoji"] = (!expected).into();
    fs::write(store.path(), serde_json::to_vec(&stored).unwrap()).unwrap();
    let loaded = store.load().unwrap().preferences.mixed_input;
    assert_eq!(loaded.emoji, !expected);
    assert!(!loaded.kaomoji);
}

/// The HarmonyOS host has no way to match on this type.
///
/// Its settings page reaches this store through the C ABI, whose error channel is a single string
/// shared by every entry point, so it recovers the code the shared UI decodes by matching the text
/// itself in `PreferencesErrorCode` (`platforms/harmony/entry/src/main/ets/keyboard/settings/`).
/// Rewording a variant there silently costs that failure its own sentence, so the wordings that
/// mapping names are pinned here, where the rewording would happen.
#[test]
fn error_wordings_the_harmony_host_matches_on() {
    assert_eq!(
        PreferencesError::Conflict.to_string(),
        "preferences changed; reload before saving"
    );
    assert_eq!(
        PreferencesError::InvalidPageSize.to_string(),
        "candidate page size must be between 1 and 9"
    );
    assert_eq!(
        PreferencesError::InvalidFrequency.to_string(),
        "frequency trigger count and linear step must be between 1 and 10"
    );
    assert_eq!(
        PreferencesError::InvalidMixedInput.to_string(),
        "mixed English minimum prefix must be between 1 and 8"
    );
    assert_eq!(
        PreferencesError::InvalidFloatingToolbar.to_string(),
        "floating toolbar settings are invalid"
    );
    assert_eq!(
        PreferencesError::ConflictingKeyBindings.to_string(),
        "word-to-character and paging cannot use the same keys"
    );
    assert_eq!(
        PreferencesError::UnsupportedFormat.to_string(),
        "unsupported preferences format"
    );
    // Matched by prefix, because the cause it carries varies.
    assert!(
        PreferencesError::Json(serde_json::from_str::<u8>("x").unwrap_err())
            .to_string()
            .starts_with("invalid preferences document:")
    );
}

/// A restore puts every setting back and costs the user no secret.
///
/// The secrets are asserted by walking the serialized document rather than by listing the fields:
/// a credential added later would otherwise be reset by `restored_to_defaults` with nothing to
/// notice it, and the whole point of the method is that it never silently throws one away.
#[test]
fn restoring_defaults_keeps_what_cannot_be_retyped() {
    let mut edited = Preferences {
        candidate_page_size: 7,
        learning: !Preferences::default().learning,
        scheme: InputScheme::Wubi,
        ..Preferences::default()
    };
    edited.keybindings.switch_language_shift = false;
    edited.fuzzy_pinyin.seeded = true;
    edited.fuzzy_pinyin.enabled = true;
    edited.voice_input.asr_provider = "doubao".into();
    edited.voice_input.asr_token = "fixture-asr-token".into();
    edited.voice_input.asr_endpoint = "https://asr.example.test/v1".into();
    edited.voice_input.asr_model_path = "/Users/fixture/models/ggml.bin".into();
    edited.voice_input.asr_model_mirror = "https://mirror.example.test/".into();
    edited.voice_input.polish_token = "fixture-polish-token".into();
    edited.voice_input.polish_enabled = true;
    edited.ai_assistant.token = "fixture-ai-token".into();
    edited.ai_assistant.enabled = !Preferences::default().ai_assistant.enabled;
    edited.custom_translation.api_key = "fixture-custom-key".into();
    edited.tencent_tmt.secret_id = "fixture-tencent-id".into();
    edited.tencent_tmt.secret_key = "fixture-tencent-key".into();
    edited.niutrans.app_id = "fixture-niutrans-app".into();
    edited.niutrans.apikey = "fixture-niutrans-key".into();

    let restored = edited.restored_to_defaults();
    let defaults = Preferences::default();

    // The settings are back.
    assert_eq!(restored.candidate_page_size, defaults.candidate_page_size);
    assert_eq!(restored.learning, defaults.learning);
    assert_eq!(restored.scheme, defaults.scheme);
    assert_eq!(
        restored.keybindings.switch_language_shift,
        defaults.keybindings.switch_language_shift
    );
    assert_eq!(restored.fuzzy_pinyin.enabled, defaults.fuzzy_pinyin.enabled);
    assert!(!restored.voice_input.polish_enabled);
    assert_eq!(restored.ai_assistant.enabled, defaults.ai_assistant.enabled);

    // The fixture itself has to be a document the store would accept, or this proves nothing.
    edited.validate().expect("edited fixture validates");

    // Nothing that reads as a secret was dropped, whoever adds one next.
    let document = serde_json::to_string(&restored).expect("restored preferences serialize");
    for secret in [
        "fixture-asr-token",
        "fixture-polish-token",
        "fixture-ai-token",
        "fixture-custom-key",
        "fixture-tencent-id",
        "fixture-tencent-key",
        "fixture-niutrans-key",
    ] {
        assert!(
            document.contains(secret),
            "{secret} did not survive a restore"
        );
    }
    // What addresses the same service travels with it.
    assert_eq!(restored.voice_input.asr_provider, "doubao");
    assert_eq!(
        restored.voice_input.asr_endpoint,
        "https://asr.example.test/v1"
    );
    assert_eq!(
        restored.voice_input.asr_model_path,
        "/Users/fixture/models/ggml.bin"
    );
    assert_eq!(
        restored.voice_input.asr_model_mirror,
        "https://mirror.example.test/"
    );
    // The seeding marker is not a setting: clearing it would re-seed rules the user turned off.
    assert!(restored.fuzzy_pinyin.seeded);

    // A restore is idempotent and produces a document the store will accept.
    restored.validate().expect("restored preferences validate");
    assert_eq!(restored.restored_to_defaults(), restored);
}

/// Both halves of this product can name the renderer they mean.
///
/// `ui_backend` is written `d2d` in the Windows factory configuration and `direct2d` by this type,
/// so a document carrying the factory spelling was rejected rather than read - and a rejected
/// preference document does not lose one field, it falls back wholesale. The reference also accepts
/// `webview` and `web` for the same choice, having written both at different times.
///
/// Serialisation is unchanged: the aliases are read-only, so nothing here starts writing a second
/// spelling of its own.
#[test]
fn ui_backend_reads_every_spelling_this_product_has_written() {
    for (value, expected) in [
        ("direct2d", UiBackend::Direct2d),
        ("d2d", UiBackend::Direct2d),
        ("webview2", UiBackend::Webview2),
        ("webview", UiBackend::Webview2),
        ("web", UiBackend::Webview2),
    ] {
        assert_eq!(
            serde_json::from_str::<UiBackend>(&format!("\"{value}\"")).unwrap(),
            expected,
            "{value} should name a renderer this product understands"
        );
    }
    // An unknown value is still an error rather than a silent default: the reference falls back to
    // native for one, but it is reading a single key, while here the whole document goes with it.
    assert!(serde_json::from_str::<UiBackend>("\"opengl\"").is_err());
    // One spelling out, whichever ones come in.
    assert_eq!(
        serde_json::to_string(&UiBackend::Direct2d).unwrap(),
        "\"direct2d\""
    );
    assert_eq!(
        serde_json::to_string(&UiBackend::Webview2).unwrap(),
        "\"webview2\""
    );
}

// Staged writes nobody is going to finish.
//
// `NamedTempFile` removes itself when dropped, but a process killed between creating the file and
// renaming it drops nothing, and the input method is stopped exactly that way every time it is
// reinstalled. Two such files were sitting in the data directory of a machine running this client,
// one holding a copy of the preferences and one of the typing statistics; nothing would ever have
// removed them.
#[test]
fn saving_clears_staged_writes_that_were_abandoned() {
    use std::time::{Duration, SystemTime};

    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    store.save(0, Preferences::default()).unwrap();

    let age = |path: &std::path::Path, seconds: u64| {
        let when = SystemTime::now() - Duration::from_secs(seconds);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    };

    let abandoned = directory.path().join(".tmpAbandoned");
    std::fs::write(&abandoned, b"{}").unwrap();
    age(&abandoned, 48 * 60 * 60);

    // A staged write from a moment ago may belong to something still running - the statistics
    // document stages into this same directory under a lock of its own - so it is left alone.
    let in_flight = directory.path().join(".tmpInFlight");
    std::fs::write(&in_flight, b"{}").unwrap();

    // Age alone is not the rule: a file that is not a staged write keeps its place however old it
    // is, and so does a directory that happens to be named like one.
    let unrelated = directory.path().join("old-export.json");
    std::fs::write(&unrelated, b"{}").unwrap();
    age(&unrelated, 48 * 60 * 60);
    let directory_named_like_a_temporary = directory.path().join(".tmpDirectory");
    std::fs::create_dir(&directory_named_like_a_temporary).unwrap();

    store
        .save(
            1,
            Preferences {
                candidate_font_size: 20,
                ..Preferences::default()
            },
        )
        .unwrap();

    assert!(!abandoned.exists(), "the abandoned staged write is removed");
    assert!(in_flight.exists(), "a staged write from a moment ago stays");
    assert!(unrelated.exists());
    assert!(directory_named_like_a_temporary.is_dir());
    // And the save itself did what it was asked.
    assert_eq!(store.load().unwrap().preferences.candidate_font_size, 20);
}

fn corrupt_backups(directory: &Path) -> Vec<PathBuf> {
    let mut backups: Vec<PathBuf> = fs::read_dir(directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("preferences.json.corrupt-"))
        })
        .collect();
    backups.sort();
    backups
}

fn expect_recovered(outcome: RecoveryOutcome) -> (PreferencesSnapshot, PathBuf, bool) {
    match outcome {
        RecoveryOutcome::Recovered {
            snapshot,
            backup_path,
            salvaged,
        } => (snapshot, backup_path, salvaged),
        RecoveryOutcome::NotNeeded(_) => panic!("expected a recovery"),
    }
}

#[test]
fn recover_backs_up_truncated_json_and_writes_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let damaged = br#"{"format_version":1,"revision":4,"preferences":{"scheme":"#;
    fs::write(directory.path().join("preferences.json"), damaged).unwrap();
    assert!(matches!(store.load(), Err(PreferencesError::Json(_))));

    let (snapshot, backup, salvaged) = expect_recovered(store.recover().unwrap());
    assert!(!salvaged);
    assert_eq!(fs::read(&backup).unwrap(), damaged);
    assert_eq!(backup.parent().unwrap(), directory.path());
    let name = backup.file_name().unwrap().to_str().unwrap();
    let stamp = name.strip_prefix("preferences.json.corrupt-").unwrap();
    assert_eq!(stamp.len(), 15);
    assert_eq!(stamp.as_bytes()[8], b'-');
    assert!(stamp
        .bytes()
        .enumerate()
        .all(|(index, byte)| index == 8 || byte.is_ascii_digit()));
    assert_eq!(snapshot.preferences, Preferences::default());
    // The revision could not be read, so it restarts above anything a host counted to.
    assert!(snapshot.revision > 1_000_000_000);
    assert_eq!(store.load().unwrap(), snapshot);
    // The repaired document saves like any other.
    store
        .save(snapshot.revision, Preferences::default())
        .unwrap();
}

#[test]
fn recover_treats_an_empty_document_as_damaged() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    fs::write(directory.path().join("preferences.json"), b"").unwrap();
    let (snapshot, backup, salvaged) = expect_recovered(store.recover_malformed().unwrap());
    assert!(!salvaged);
    assert!(fs::read(backup).unwrap().is_empty());
    assert_eq!(store.load().unwrap(), snapshot);
}

#[test]
fn recover_drops_only_the_unknown_and_ill_typed_fields() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let mut preferences = Preferences {
        scheme: InputScheme::Wubi,
        candidate_page_size: 7,
        candidate_font_size: 22,
        clipboard_history: true,
        ..Preferences::default()
    };
    preferences.ai_assistant.token = "synthetic-ai-token".into();
    let saved = store.save(0, preferences.clone()).unwrap();
    let mut document = serde_json::to_value(&saved).unwrap();
    document["preferences"]["from_a_newer_build"] = serde_json::json!(true);
    document["preferences"]["learning"] = serde_json::json!("yes");
    fs::write(
        directory.path().join("preferences.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    assert!(store.load().is_err());

    let (snapshot, _, salvaged) = expect_recovered(store.recover().unwrap());
    assert!(salvaged);
    assert_eq!(snapshot.revision, saved.revision + 1);
    let expected = Preferences {
        learning: Preferences::default().learning,
        ..preferences
    };
    assert_eq!(snapshot.preferences, expected);
    assert_eq!(store.load().unwrap(), snapshot);
}

#[test]
fn recover_keeps_a_credential_beside_a_bad_sibling() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let mut preferences = Preferences::default();
    preferences.custom_translation.endpoint = "https://translate.example/api".into();
    preferences.custom_translation.api_key = "synthetic-translation-key".into();
    preferences.tencent_tmt.secret_id = "SyntheticId".into();
    let saved = store.save(0, preferences).unwrap();
    let mut document = serde_json::to_value(&saved).unwrap();
    document["preferences"]["custom_translation"]["enabled"] = serde_json::json!("sometimes");
    document["preferences"]["tencent_tmt"]["region"] = serde_json::json!("not a region!");
    fs::write(
        directory.path().join("preferences.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();

    let (snapshot, _, salvaged) = expect_recovered(store.recover().unwrap());
    assert!(salvaged);
    let translation = &snapshot.preferences.custom_translation;
    assert_eq!(translation.api_key, "synthetic-translation-key");
    assert_eq!(translation.endpoint, "https://translate.example/api");
    assert_eq!(
        translation.enabled,
        CustomTranslationPreferences::default().enabled
    );
    assert_eq!(snapshot.preferences.tencent_tmt.secret_id, "SyntheticId");
    assert_eq!(
        snapshot.preferences.tencent_tmt.region,
        TencentTmtPreferences::default().region
    );
}

#[test]
fn recover_handles_a_future_format_but_malformed_scope_leaves_it() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let saved = store
        .save(
            0,
            Preferences {
                candidate_page_size: 6,
                ..Preferences::default()
            },
        )
        .unwrap();
    let mut document = serde_json::to_value(&saved).unwrap();
    document["format_version"] = serde_json::json!(2);
    let bytes = serde_json::to_vec(&document).unwrap();
    let path = directory.path().join("preferences.json");
    fs::write(&path, &bytes).unwrap();

    // The automatic path refuses a well-formed document from a newer build.
    assert!(matches!(
        store.recover_malformed(),
        Err(PreferencesError::UnsupportedFormat)
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(corrupt_backups(directory.path()).is_empty());

    let (snapshot, backup, salvaged) = expect_recovered(store.recover().unwrap());
    assert!(salvaged);
    assert_eq!(fs::read(backup).unwrap(), bytes);
    assert_eq!(snapshot.format_version, 1);
    assert_eq!(snapshot.revision, saved.revision + 1);
    assert_eq!(snapshot.preferences.candidate_page_size, 6);
}

#[test]
fn recover_is_not_needed_for_valid_missing_or_repaired_documents() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    assert_eq!(
        store.recover().unwrap(),
        RecoveryOutcome::NotNeeded(PreferencesSnapshot::default())
    );
    assert!(!directory.path().join("preferences.json").exists());

    let saved = store.save(0, Preferences::default()).unwrap();
    let before = fs::read(directory.path().join("preferences.json")).unwrap();
    assert_eq!(
        store.recover().unwrap(),
        RecoveryOutcome::NotNeeded(saved.clone())
    );
    assert_eq!(
        fs::read(directory.path().join("preferences.json")).unwrap(),
        before
    );
    assert!(corrupt_backups(directory.path()).is_empty());

    fs::write(directory.path().join("preferences.json"), b"{").unwrap();
    let (repaired, _, _) = expect_recovered(store.recover().unwrap());
    assert_eq!(
        store.recover().unwrap(),
        RecoveryOutcome::NotNeeded(repaired)
    );
    assert_eq!(corrupt_backups(directory.path()).len(), 1);
}

#[test]
fn recover_suffixes_a_backup_name_that_is_taken() {
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    // Two backups within the same second share a stamp; retry the rare pair that straddles a second.
    let collided = (0..5).any(|_| {
        let first = store.write_backup(b"first").unwrap();
        let second = store.write_backup(b"second").unwrap();
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&second).unwrap(), b"second");
        let first = first.file_name().unwrap().to_str().unwrap().to_owned();
        second.file_name().unwrap().to_str().unwrap() == format!("{first}-1")
    });
    assert!(collided);

    // A whole recovery whose stamp is taken keeps both backups.
    let path = directory.path().join("preferences.json");
    fs::write(&path, b"damaged").unwrap();
    let (_, backup, _) = expect_recovered(store.recover().unwrap());
    assert_eq!(fs::read(backup).unwrap(), b"damaged");
    assert!(corrupt_backups(directory.path()).len() >= 3);
}

#[cfg(unix)]
#[test]
fn recover_leaves_the_original_when_the_backup_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(directory.path());
    let path = directory.path().join("preferences.json");
    fs::write(&path, b"{\"format_version\":").unwrap();
    // Create the lock file while the directory is still writable.
    assert!(store.load().is_err());
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let probe = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.path().join("probe"));
    if probe.is_ok() {
        // Running with privileges that ignore directory permissions; nothing to observe.
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let result = store.recover();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(result, Err(PreferencesError::Io(_))));
    assert_eq!(fs::read(&path).unwrap(), b"{\"format_version\":");
    assert!(corrupt_backups(directory.path()).is_empty());
}

#[test]
fn touch_toolbar_keeps_the_original_buttons_by_default_and_round_trips_partial_documents() {
    let dir = tempfile::tempdir().unwrap();
    let store = PreferencesStore::new(dir.path());
    let mut legacy = serde_json::to_value(PreferencesSnapshot::default()).unwrap();
    legacy["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("touch_toolbar");
    fs::write(store.path(), serde_json::to_vec(&legacy).unwrap()).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(
        loaded.preferences.touch_toolbar,
        TouchToolbarPreferences::default()
    );
    let defaults = loaded.preferences.touch_toolbar;
    assert!(defaults.layout && defaults.emoji && defaults.skin);
    assert!(!defaults.clipboard && !defaults.ai && !defaults.character_set);

    // A document written before a switch existed leaves that switch at its default.
    let partial: TouchToolbarPreferences =
        serde_json::from_value(serde_json::json!({"emoji": false, "clipboard": true})).unwrap();
    assert!(!partial.emoji && partial.clipboard && partial.layout && !partial.ai);

    let saved = store
        .save(
            0,
            Preferences {
                touch_toolbar: TouchToolbarPreferences {
                    skin: false,
                    punctuation: true,
                    ..TouchToolbarPreferences::default()
                },
                ..Preferences::default()
            },
        )
        .unwrap();
    assert!(!saved.preferences.touch_toolbar.skin);
    assert!(saved.preferences.touch_toolbar.punctuation);
    assert!(!store.load().unwrap().preferences.touch_toolbar.skin);
}
