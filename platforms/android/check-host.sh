#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "$0")/../.." && pwd)
# Several guards below are "fail if rg finds this". Without rg each of those exits 127, which the if reads as "not found", so a missing binary would pass every one of them silently.
if ! command -v rg >/dev/null 2>&1; then
  echo "ripgrep (rg) is required by the Android host contract checks" >&2
  exit 1
fi
android_sdk=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}
if [[ -z "$android_sdk" ]]; then
  echo "Set ANDROID_SDK_ROOT to an installed Android SDK" >&2
  exit 1
fi
android_jar="$android_sdk/platforms/android-35/android.jar"
if [[ ! -f "$android_jar" ]]; then
  echo "Android API 35 platform is required" >&2
  exit 1
fi
output_dir=$(mktemp -d)
trap 'rm -f "$output_dir/manifest.apk" "$output_dir/resources.zip"; find "$output_dir" -name "*.class" -delete; find "$output_dir" -depth -type d -empty -delete' EXIT
# Command 9 was unmapped when this guard was added; it is now Action::Finish in
# crates/host-api/src/ffi/input.rs, and the declined-punctuation path needs it.
# What must not come back is the literal, which is how the unmapped call got in.
if rg -n 'NativeClient\.command\([^,]+, 9\)' "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java"; then
  echo "Android input service must name command 9 (FINISH_COMPOSITION_COMMAND), not inline it" >&2
  exit 1
fi
if ! rg -q 'msime_client_command' "$repo_root/crates/host-api/src/ffi/input.rs" \
  || ! rg -q '^\s*9 => Action::Finish,' "$repo_root/crates/host-api/src/ffi/input.rs"; then
  echo "Shared host command 9 is no longer Action::Finish; FINISH_COMPOSITION_COMMAND is stale" >&2
  exit 1
fi
# Japanese kana variants are Engine state, not a host-maintained lookup table. Keep the named
# Android command and its shared FFI mapping together so a future enum change cannot silently
# turn the visible 小゛゜ key into a no-op.
if rg -n 'command\(10\)' "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java"; then
  echo "Android input service must name command 10 (CYCLE_KANA_VARIANT_COMMAND), not inline it" >&2
  exit 1
fi
if ! rg -q '^\s*10 => Action::Command\(Command::CycleKanaVariant\),' \
    "$repo_root/crates/host-api/src/ffi/input.rs"; then
  echo "Shared host command 10 is no longer CycleKanaVariant; Android command is stale" >&2
  exit 1
fi
if rg -n 'VariantGroup|showJapaneseVariants' \
    "$repo_root/platforms/android/java/app/msime/client/keyboard/JapaneseNineKeyLayout.java" \
    "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java"; then
  echo "Android must not duplicate Engine-owned Japanese kana variant tables" >&2
  exit 1
fi
# Hardware navigation must use the shared command numbers through one named policy. Keep the
# service from growing another inline key-code table that can drift from the FFI mapping.
if ! rg -q 'HardwareKeyPolicy\.commandFor' \
    "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java"; then
  echo "Android hardware navigation must route through HardwareKeyPolicy" >&2
  exit 1
fi
if ! rg -q 'KEYCODE_FORWARD_DEL.*-> 8' \
    "$repo_root/platforms/android/java/app/msime/client/policy/HardwareKeyPolicy.java" \
    || ! rg -q '^\s*8 => Action::Command\(Command::DeleteForward\),' \
    "$repo_root/crates/host-api/src/ffi/input.rs"; then
  echo "Android forward-delete mapping no longer matches the shared Host API" >&2
  exit 1
fi
# Double-pinyin labels belong to the Engine profile tables. Android may gate visibility and
# decode the bounded response, but it must not carry a second profile keymap that can drift.
if rg -n 'PROFILES|uai=k|ing=;' \
    "$repo_root/platforms/android/java/app/msime/client/keyboard/ShuangpinKeyHintPolicy.java"; then
  echo "Android must not duplicate Engine-owned double-pinyin profile tables" >&2
  exit 1
fi
if ! rg -q 'shuangpinKeyHintsRaw' \
    "$repo_root/platforms/android/java/app/msime/client/core/NativeClient.java" \
    || ! rg -q 'msime_client_shuangpin_key_hints' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android double-pinyin hints must cross the shared Host API through JNI" >&2
  exit 1
fi
# Smart-punctuation repeat/space decisions belong to the shared Host API. Android may hold
# editor-scoped snapshots, but must not reimplement timing or replacement rules locally.
if ! rg -q 'smartPunctuationArmRaw|smartPunctuationDecideRaw' \
    "$repo_root/platforms/android/java/app/msime/client/core/NativeClient.java" \
    || ! rg -q 'msime_client_smart_punctuation_(arm|decide)' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android smart punctuation must cross the shared Host API through JNI" >&2
  exit 1
fi
# The fullwidth state belongs to the runtime, not to a private SharedPreferences file: the Engine
# widens what it commits, and it can only do that if the host has told it the width. The second
# guard is the reason the first one matters - this host used to keep its own latch, and the shared
# settings page's 全角输入 switch did nothing here at all.
if ! rg -q 'setCharacterWidthRaw' \
    "$repo_root/platforms/android/java/app/msime/client/core/NativeClient.java" \
  || ! rg -q 'msime_client_set_character_width' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android fullwidth input must cross the shared Host API through JNI" >&2
  exit 1
fi
if rg -n 'full-width-input|keyboardLayoutPreferences' \
    "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java"; then
  echo "Android must read the fullwidth state from the shared preference, not a private store" >&2
  exit 1
fi
# 以词定字 belongs to the Engine: it picks the Han character and decides whether the candidate has
# one at all. This host may route the key and commit the fallback the source's host commits, but it
# must not grow its own idea of which candidates qualify.
if ! rg -q 'selectEdgeRaw' \
    "$repo_root/platforms/android/java/app/msime/client/core/NativeClient.java" \
  || ! rg -q 'msime_client_select_edge' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android word-to-character must cross the shared Host API through JNI" >&2
  exit 1
fi
# Candidate paging keys are a user preference, not a fixed table: the source lets the pair be
# chosen and this host reads the same shared `navigation` document the desktop hosts do. Keeping
# the routing in one named policy is what stops a second, drifting key table growing in the service.
if ! rg -q 'CandidateNavigationPolicy\.commandFor' \
    "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java"; then
  echo "Android candidate paging must route through CandidateNavigationPolicy" >&2
  exit 1
fi
# A key code written as a bare number reads correctly to everybody and is only ever falsified by
# pressing the key. One of them was wrong for as long as this host has had hardware chords: 简繁 sat
# on 33, which is KEYCODE_E, while the preference is named ..._ctrl_shift_f and the settings page
# promises Ctrl+Shift+F. Comparisons in these files name their key.
for policy in HardwareShortcutPolicy HardwareKeyPolicy NumberRowSelectionPolicy CandidateNavigationPolicy; do
  source_file="$repo_root/platforms/android/java/app/msime/client/policy/$policy.java"
  [[ -f "$source_file" ]] || source_file="$repo_root/platforms/android/java/app/msime/client/keyboard/$policy.java"
  if rg -q '(key|keyCode|keycode)\s*(==|>=|<=|>|<)\s*[0-9]+' "$source_file"; then
    echo "Android $policy compares a key code against a bare number; name it with KeyEvent" >&2
    exit 1
  fi
done
# Both ends of the first/last-candidate commands, so neither side can drift alone: the shared header
# owns the numbers and this host must reach them by name rather than by repeating them at a call site.
if ! rg -q 'MSIME_FIRST_CANDIDATE = 104, MSIME_LAST_CANDIDATE = 105' \
    "$repo_root/crates/host-api/include/msime_client.h" \
  || ! rg -q 'FIRST_CANDIDATE = 104' \
    "$repo_root/platforms/android/java/app/msime/client/policy/CandidateNavigationPolicy.java" \
  || ! rg -q 'LAST_CANDIDATE = 105' \
    "$repo_root/platforms/android/java/app/msime/client/policy/CandidateNavigationPolicy.java"; then
  echo "Android Home/End must map to the shared first/last candidate commands" >&2
  exit 1
fi
# 「候选栏预编辑」 governs the spelling only. The chosen part of a phrase is held out of the
# document at this host's request, so it must keep being drawn whatever the setting says - that is
# the same half-state scripts/test-phrase-preedit-hosts.py guards from the other side.
if ! rg -q 'CandidatePreeditStylePolicy\.composedText' \
    "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java" \
  || rg -q 'CandidatePreeditStylePolicy[^;]*phrase_prefix' \
    "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java"; then
  echo "Android candidate preedit style must gate the spelling, never the phrase prefix" >&2
  exit 1
fi
# The polish presets carry their own prompt-injection wording and already exist in four places in
# this repository. This host reads the shared table through JNI rather than adding a fifth copy,
# and the transcript travels inside the tags that wording refers to.
if ! rg -q 'msime::windows::polish_prompt_for' \
    "$repo_root/platforms/android/native/client_jni.cpp" \
  || rg -q '语音转写整理助手' "$repo_root/platforms/android/java"; then
  echo "Android must read the polish presets from the shared table, not a copy" >&2
  exit 1
fi
# Candidate words reach api.msime.app only after an explicit account choice (PRIVACY.md). The policy smoke covers accountSelected itself; this pins the service to it: the fetch, the apply and the reserved rows read candidateTranslationAccount, and both preference paths derive it through the policy.
account_service="$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java"
if ! rg -qU 'void scheduleCandidateTranslations\(\) \{\s*if \(!candidateTranslationAccount ' "$account_service" \
  || ! rg -qU 'void applyCandidateTranslations\(long generation\) \{\s*if \(!candidateTranslationAccount ' "$account_service" \
  || ! rg -qU 'glossLines\(\s*candidateTranslationTargets, candidateEnglishGloss, candidateTranslationAccount,\s*candidateOfflineTargets\(\)\)' "$account_service" \
  || [ "$(rg -c '= candidateTranslationAccountFrom\(preferences\);' "$account_service")" != 2 ]; then
  echo "Android must fetch candidate translations from the account only after an explicit choice" >&2
  exit 1
fi
if ! rg -q '<asr_text>' \
    "$repo_root/platforms/android/java/app/msime/client/voice/VoicePolishPolicy.java"; then
  echo "Android polish must wrap the transcript in the boundary the presets name" >&2
  exit 1
fi
# The streaming protocol's framing and authentication are the shared implementation's; this host
# adds only the transport Android has no platform API for. A frame built here would be a second
# encoder to keep in step with the provider.
if ! rg -q 'NativeClient\.doubaoStartFrame|NativeClient\.doubaoAudioFrame' \
    "$repo_root/platforms/android/java/app/msime/client/voice/DoubaoRecognizer.java" \
  || ! rg -q 'msime_client_doubao_(start|audio)_frame' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android streaming recognition must build its frames through the shared Host API" >&2
  exit 1
fi
# Reject an oversized response before JNI obtains a native view of the Java byte array. The shared
# decoder has the same one-megabyte wire bound, but checking after GetByteArrayElements can briefly
# duplicate an untrusted oversized WebSocket message.
if ! rg -q 'if \(length > 1024 \* 1024\)' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android Doubao decoding must bound the Java frame before copying" >&2
  exit 1
fi
# The Engine decides what a punctuation key produces, so the Chinese/English state has to reach it.
# A toggle that only changed this keyboard's key faces would show one mark and commit the other.
if ! rg -q 'setChinesePunctuationRaw' \
    "$repo_root/platforms/android/java/app/msime/client/core/NativeClient.java" \
  || ! rg -q 'msime_client_set_chinese_punctuation' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android punctuation switching must cross the shared Host API through JNI" >&2
  exit 1
fi
# The clipboard history is one file every mobile host shares, and the settings page reads it. A
# second implementation here is what made this keyboard and that page disagree about what the
# history contained, so the host may render entries but must not keep its own.
if ! rg -q 'NativeClient\.mobileClipboardHistory' \
    "$repo_root/platforms/android/java/app/msime/client/clipboard/ClipboardHistoryStore.java" \
  || ! rg -q 'msime_client_mobile_clipboard_history' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android clipboard history must go through the shared mobile store" >&2
  exit 1
fi
if rg -q 'putString\(ITEMS_KEY' \
    "$repo_root/platforms/android/java/app/msime/client/clipboard/ClipboardHistoryStore.java"; then
  echo "Android must not write clipboard entries to its own private document" >&2
  exit 1
fi
# Dropping the history when the preference is off is housekeeping, and `onCreateInputView` does it
# on every open. A store that cannot be written is not a reason to refuse to draw a keyboard: when
# that clear threw, it threw out of the framework's showWindow and the input method died, so
# Android fell back to another keyboard and the user never saw this one. The 清空 button keeps
# `clear()` - there the user asked, and silence would be a lie.
if ! rg -q 'clearQuietly' \
    "$repo_root/platforms/android/java/app/msime/client/clipboard/ClipboardHistoryStore.java"; then
  echo "Android clipboard housekeeping needs a clear that cannot stop the caller" >&2
  exit 1
fi
for site in onCreateInputView applyClipboardPreference; do
  if rg -A 40 "$site\([^)]*\) \{" \
      "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java" \
      | rg -q 'clipboardHistory\.clear\(\)'; then
    echo "Android clipboard housekeeping in $site must use clearQuietly" >&2
    exit 1
  fi
done
# Both maintenance chords are Ctrl+Shift+Alt, and the modifier branch in onKeyDown hands every
# such combination to the application. Routing them through one named policy, ahead of that branch,
# is what keeps them reachable at all on a keyboard that has no long press.
if ! rg -q 'HardwareMaintenancePolicy\.action' \
    "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java" \
  || ! rg -q 'msime_client_reset_cache' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android maintenance chords must route through HardwareMaintenancePolicy and the shared API" >&2
  exit 1
fi
# Simplified/Traditional output is the shared OpenCC s2t tables, phrase-level, and the same
# conversion Windows and Linux use. This host converted one character at a time through
# android.icu.Transliterator, which turns 头发 into 頭發 and is silently unavailable below API 29.
if ! rg -q 'NativeClient::simplifiedToTraditional' \
    "$repo_root/platforms/android/java/app/msime/client/core/AndroidChineseTextConversion.java" \
  || rg -q '^import android\.icu\.text\.Transliterator' "$repo_root/platforms/android/java"; then
  echo "Android Simplified/Traditional output must use the shared converter" >&2
  exit 1
fi
# The keyboard has its own voice entry and never goes through the settings app, so it asks the
# shared resolution the same question rather than launching the platform recogniser regardless of
# what the user configured. A second copy of the provider rules in Java is how the two entries
# would start transcribing with different services on the same device.
if ! rg -q 'VoiceConfiguration\.read' \
    "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java" \
  || ! rg -q 'msime_client_mobile_voice_configuration' \
    "$repo_root/platforms/android/native/client_jni.cpp"; then
  echo "Android keyboard voice must read the shared provider resolution" >&2
  exit 1
fi
# The JNI translation unit is the one place a Java declaration and a shared FFI
# signature have to agree, and nothing else in this script reads it: a method
# declared native in Java compiles whether or not the C++ side exists. Compiling
# it for the real target catches that without the full native build, which needs
# vcpkg and the Engine. A machine without the pinned NDK skips it and says so.
ndk=${MSIME_ANDROID_NDK:-${android_sdk}/ndk/28.2.13676358}
case $(uname -s) in
  Darwin) host_tag=darwin-x86_64 ;;
  Linux) host_tag=linux-x86_64 ;;
  *) host_tag="" ;;
esac
jni_compiler="$ndk/toolchains/llvm/prebuilt/$host_tag/bin/aarch64-linux-android28-clang++"
if [[ -n "$host_tag" && -x "$jni_compiler" ]]; then
  "$jni_compiler" -std=c++20 -fsyntax-only -Wall -Werror \
    -I"$repo_root/crates/host-api/include" -I"$repo_root/shared" \
    "$repo_root/platforms/android/native/client_jni.cpp"
  echo "client_jni.cpp: aarch64-linux-android compile against the shared header passed"
else
  echo "client_jni.cpp: skipped (pinned NDK 28.2.13676358 not installed)"
fi
# This script compiles against API 35 while the manifest declares minSdk 28, so a
# newer java.nio API passes here and only fails in the real APK build. These two
# arrived in API 34 and are the ones that actually got in; neither has a runtime
# version guard anywhere in this host. This is a targeted guard, not a general
# API-level check - Gradle lint is what covers the rest.
if rg -n 'Files\.(readString|writeString)\(' "$repo_root/platforms/android/java" --glob '*.java'; then
  echo "Files.readString/writeString need API 34; this host declares minSdk 28" >&2
  exit 1
fi
if [[ ! -f "$repo_root/platforms/android/java/app/msime/client/handwriting/MlKitHandwritingRecognizer.java" \
      || ! -f "$repo_root/platforms/android/java/app/msime/client/handwriting/MlKitImeInitProvider.java" ]]; then
  echo "Android handwriting implementation must live with the native host sources" >&2
  exit 1
fi
if ! rg -Uq 'android:name="app\.msime\.client\.MlKitImeInitProvider"[[:space:]]+android:authorities="\$\{applicationId\}\.mlkit-ime-init"[[:space:]]+android:exported="false"[[:space:]]+android:process=":ime"' \
    "$repo_root/platforms/android/AndroidManifest.xml"; then
  echo "Android native host must initialize ML Kit inside the isolated IME process" >&2
  exit 1
fi
# The host compiles against AndroidX and Material now, and those are AARs that only Gradle resolves,
# so this script no longer compiles the whole source set -- `platforms/android/gradle-app` does, and
# build-apk.sh drives it. What stays here is the part that is worth having without a Gradle daemon:
# the pure-Java models and their smokes, which have no Android dependency at all and run in a second.
#
# Match the launcher activities by their path *inside the repository*. The absolute pattern this
# started as, `*/home/*`, also matches every source on a GitHub runner, where the checkout itself
# lives under /home/runner: the list came out empty, javac was handed nothing but the test files,
# and the gate failed with 1069 "cannot find symbol" errors on every pull request while passing on
# any developer machine whose checkout is not under /home.
client_sources=()
while IFS= read -r source; do
  case "${source#"$repo_root/"}" in
    platforms/android/java/app/msime/client/home/*) continue ;;
  esac
  if rg -q '^import (androidx|com\.google)\.' "$source"; then continue; fi
  client_sources+=("$source")
done < <(find "$repo_root/platforms/android/java/app/msime/client" -name "*.java" -print)
# An empty list means the filter above ate everything; javac would then fail on the test files with
# a wall of missing symbols rather than saying so.
if [[ ${#client_sources[@]} -eq 0 ]]; then
  echo "No Android client sources selected for compilation; the source filter is wrong" >&2
  exit 1
fi
javac --release 17 -Xlint:all -Werror -cp "$android_jar" -d "$output_dir" \
  "${client_sources[@]}" \
  "$repo_root/platforms/android/tests/core/EditorSmoke.java" \
  "$repo_root/platforms/android/tests/core/PhrasePreeditSmoke.java" \
  "$repo_root/platforms/android/tests/core/InputViewRefreshPolicySmoke.java" \
  "$repo_root/platforms/android/tests/core/EditorContextSnapshotSmoke.java" \
  "$repo_root/platforms/android/tests/settings/PreferencesSmoke.java" \
  "$repo_root/platforms/android/tests/settings/InputModeStoreSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardLayoutSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/LetterKeyFacePolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/ReturnKeyActionSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/SpaceCursorMovementSmoke.java" \
  "$repo_root/platforms/android/tests/settings/EnglishCapitalizationPolicySmoke.java" \
  "$repo_root/platforms/android/tests/settings/EnglishLetterCaseStateSmoke.java" \
  "$repo_root/platforms/android/tests/dictionary/ChineseHelpcodePolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/MicrosoftShuangpinKeyPolicySmoke.java" \
  "$repo_root/platforms/android/tests/dictionary/ChineseOutputPolicySmoke.java" \
  "$repo_root/platforms/android/tests/core/FullWidthInputPolicySmoke.java" \
  "$repo_root/platforms/android/tests/core/CharacterWidthPolicySmoke.java" \
  "$repo_root/platforms/android/tests/core/DeclinedKeyPolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardInputContextSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardGeometrySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardFormFactorPolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardLayoutAdjustPolicySmoke.java" \
  "$repo_root/platforms/android/tests/voice/VoiceResultStoreSmoke.java" \
  "$repo_root/platforms/android/tests/voice/AiPolishClientSmoke.java" \
  "$repo_root/platforms/android/tests/voice/HttpAsrPolicySmoke.java" \
  "$repo_root/platforms/android/tests/voice/WebSocketFramesSmoke.java" \
  "$repo_root/platforms/android/tests/voice/DoubaoAsrPolicySmoke.java" \
  "$repo_root/platforms/android/tests/voice/VoicePolishPolicySmoke.java" \
  "$repo_root/platforms/android/tests/voice/VoicePolisherSmoke.java" \
  "$repo_root/platforms/android/tests/voice/LocalAsrPolicySmoke.java" \
  "$repo_root/platforms/android/tests/candidate/ReplyKeyboardSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardSkinSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardFeedbackSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardFeedbackStoreSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardShortcutIconPolicySmoke.java" \
  "$repo_root/platforms/android/tests/voice/TypingSourceSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/EmojiCatalogModelSmoke.java" \
  "$repo_root/platforms/android/tests/core/MoreToolsLayoutSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/LocalInputModeSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardSchemeSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/NineKeyLayoutSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/KeyboardActionRowSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/JapaneseNineKeyLayoutSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/JapaneseNineKeyActionsSmoke.java" \
  "$repo_root/platforms/android/tests/core/JapaneseVariantPolicySmoke.java" \
  "$repo_root/platforms/android/tests/core/JapaneseSpacePolicySmoke.java" \
  "$repo_root/platforms/android/tests/voice/HandwritingContractSmoke.java" \
  "$repo_root/platforms/android/tests/candidate/CandidateAppearanceSmoke.java" \
  "$repo_root/platforms/android/tests/candidate/CandidateGlossModelSmoke.java" \
  "$repo_root/platforms/android/tests/candidate/CandidateTranslationPolicySmoke.java" \
  "$repo_root/platforms/android/tests/candidate/CandidateTranslationStoreSmoke.java" \
  "$repo_root/platforms/android/tests/candidate/OnlineCandidatePolicySmoke.java" \
  "$repo_root/platforms/android/tests/dictionary/WubiCodeHintPolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/ChineseSymbolFacesSmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/ShuangpinKeyHintPolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/EnglishSuggestionPolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/EnglishSuggestionModelSmoke.java" \
  "$repo_root/platforms/android/tests/candidate/CandidatePanelSmoke.java" \
  "$repo_root/platforms/android/tests/candidate/CandidateManagementSmoke.java" \
  "$repo_root/platforms/android/tests/candidate/CandidateScrollPolicySmoke.java" \
  "$repo_root/platforms/android/tests/dictionary/ClipboardHistoryPolicySmoke.java" \
  "$repo_root/platforms/android/tests/dictionary/DictionarySnapshotQueueSmoke.java" \
  "$repo_root/platforms/android/tests/settings/DiagnosticPolicySmoke.java" \
  "$repo_root/platforms/android/tests/settings/TypingStatisticsModelSmoke.java" \
  "$repo_root/platforms/android/tests/settings/VocabularyReviewModelSmoke.java" \
  "$repo_root/platforms/android/tests/settings/InputFeatureToggleSmoke.java" \
  "$repo_root/platforms/android/tests/community/CommunityRequestSmoke.java" \
  "$repo_root/platforms/android/tests/settings/AppIconStyleSmoke.java" \
  "$repo_root/platforms/android/tests/settings/CloudClipboardTextPolicySmoke.java" \
  "$repo_root/platforms/android/tests/settings/SmartPunctuationContextSmoke.java" \
  "$repo_root/platforms/android/tests/settings/HardwareKeyPolicySmoke.java" \
  "$repo_root/platforms/android/tests/settings/HardwareShortcutPolicySmoke.java" \
  "$repo_root/platforms/android/tests/settings/HardwareMaintenancePolicySmoke.java" \
  "$repo_root/platforms/android/tests/settings/NumberRowSelectionPolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/WordCharacterPolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/CandidateNavigationPolicySmoke.java" \
  "$repo_root/platforms/android/tests/candidate/CandidateTextPolicySmoke.java" \
  "$repo_root/platforms/android/tests/candidate/CandidatePreeditStylePolicySmoke.java" \
  "$repo_root/platforms/android/tests/keyboard/SymbolPanelModelSmoke.java"
java -cp "$output_dir" EditorSmoke
java -cp "$output_dir" PhrasePreeditSmoke
java -cp "$output_dir" InputViewRefreshPolicySmoke
java -cp "$output_dir" EditorContextSnapshotSmoke
java -cp "$output_dir" PreferencesSmoke
java -cp "$output_dir" app.msime.client.InputModeStoreSmoke
java -cp "$output_dir" KeyboardLayoutSmoke
java -cp "$output_dir" LetterKeyFacePolicySmoke
java -cp "$output_dir" ReturnKeyActionSmoke
java -cp "$output_dir" SpaceCursorMovementSmoke
java -cp "$output_dir" EnglishCapitalizationPolicySmoke
java -cp "$output_dir" EnglishLetterCaseStateSmoke
java -cp "$output_dir" app.msime.client.test.ChineseHelpcodePolicySmoke
java -cp "$output_dir" MicrosoftShuangpinKeyPolicySmoke
java -cp "$output_dir" ChineseOutputPolicySmoke
java -cp "$output_dir" FullWidthInputPolicySmoke
java -cp "$output_dir" CharacterWidthPolicySmoke
java -cp "$output_dir" DeclinedKeyPolicySmoke
java -cp "$output_dir" KeyboardInputContextSmoke
java -cp "$output_dir" KeyboardGeometrySmoke
java -cp "$output_dir" KeyboardFormFactorPolicySmoke
java -cp "$output_dir" KeyboardLayoutAdjustPolicySmoke
java -cp "$output_dir" VoiceResultStoreSmoke
java -cp "$output_dir" AiPolishClientSmoke
java -cp "$output_dir" HttpAsrPolicySmoke
java -cp "$output_dir" WebSocketFramesSmoke
java -cp "$output_dir" DoubaoAsrPolicySmoke
java -cp "$output_dir" VoicePolishPolicySmoke
java -cp "$output_dir:$android_jar" VoicePolisherSmoke
java -cp "$output_dir" LocalAsrPolicySmoke
java -cp "$output_dir" ReplyKeyboardSmoke
java -cp "$output_dir" app.msime.client.KeyboardSkinSmoke
java -cp "$output_dir" CloudClipboardTextPolicySmoke
java -cp "$output_dir" KeyboardFeedbackSmoke
java -cp "$output_dir" app.msime.client.KeyboardFeedbackStoreSmoke
java -cp "$output_dir" KeyboardShortcutIconPolicySmoke
java -cp "$output_dir" TypingSourceSmoke
java -cp "$output_dir" EmojiCatalogModelSmoke
java -cp "$output_dir" MoreToolsLayoutSmoke
java -cp "$output_dir" LocalInputModeSmoke
java -cp "$output_dir" KeyboardSchemeSmoke
java -cp "$output_dir" NineKeyLayoutSmoke
java -cp "$output_dir" KeyboardActionRowSmoke
java -cp "$output_dir" JapaneseNineKeyLayoutSmoke
java -cp "$output_dir" JapaneseNineKeyActionsSmoke
java -cp "$output_dir" JapaneseVariantPolicySmoke
java -cp "$output_dir" JapaneseSpacePolicySmoke
java -cp "$output_dir" HandwritingContractSmoke
java -cp "$output_dir" CandidateAppearanceSmoke
java -cp "$output_dir" CandidateGlossModelSmoke
java -cp "$output_dir" CandidateTranslationPolicySmoke
java -cp "$output_dir" app.msime.client.CandidateTranslationStoreSmoke
java -cp "$output_dir" OnlineCandidatePolicySmoke
java -cp "$output_dir" WubiCodeHintPolicySmoke
java -cp "$output_dir" ChineseSymbolFacesSmoke
java -cp "$output_dir" ShuangpinKeyHintPolicySmoke
java -cp "$output_dir" EnglishSuggestionPolicySmoke
java -cp "$output_dir" EnglishSuggestionModelSmoke
java -cp "$output_dir" CandidatePanelSmoke
java -cp "$output_dir" CandidateManagementSmoke
java -cp "$output_dir" CandidateScrollPolicySmoke
java -cp "$output_dir" ClipboardHistoryPolicySmoke
java -cp "$output_dir" DictionarySnapshotQueueSmoke
java -cp "$output_dir" DiagnosticPolicySmoke
java -cp "$output_dir" TypingStatisticsModelSmoke
java -cp "$output_dir" VocabularyReviewModelSmoke
java -cp "$output_dir" InputFeatureToggleSmoke
java -cp "$output_dir" CommunityRequestSmoke
java -cp "$output_dir" AppIconStyleSmoke
java -cp "$output_dir" SmartPunctuationContextSmoke
java -cp "$output_dir" HardwareKeyPolicySmoke
java -cp "$output_dir" app.msime.client.HardwareShortcutPolicySmoke
java -cp "$output_dir" HardwareMaintenancePolicySmoke
java -cp "$output_dir" NumberRowSelectionPolicySmoke
java -cp "$output_dir" WordCharacterPolicySmoke
java -cp "$output_dir" CandidateNavigationPolicySmoke
java -cp "$output_dir" CandidateTextPolicySmoke
java -cp "$output_dir" CandidatePreeditStylePolicySmoke
java -cp "$output_dir" SymbolPanelModelSmoke
# Resources are compiled but not linked here: they reference Material's theme attributes, and linking
# those needs the library's own resources, which is Gradle's job. Compiling still catches a malformed
# drawable, layout or values file, which is what this step was for.
"$android_sdk/build-tools/35.0.0/aapt2" compile --dir "$repo_root/platforms/android/res" -o "$output_dir/resources.zip"
for alias in MainActivityForest MainActivitySky MainActivityDusk MainActivityVermilion; do
  if ! rg -q "android:name=\"\\.${alias}\"" "$repo_root/platforms/android/AndroidManifest.xml"; then
    echo "Android app icon alias missing: $alias" >&2
    exit 1
  fi
done
# A disabled tool card swallows the press and the 工具 section draws no state text, so the only
# thing left to say it is unavailable is how it looks.
if ! rg -q 'card\.setAlpha\(enabled \?' \
    "$repo_root/platforms/android/java/app/msime/client/core/MSIMEInputService.java"; then
  echo "Android tool cards must look disabled when they are" >&2
  exit 1
fi
# The layout bar's buttons carry the keyboard's action-key fill, so their text has to be the colour
# that fill is paired with. `accent` is that same fill in the shipped skins, and using it here made
# 恢复默认 and 完成 invisible - dark green on dark green, three blank tiles where the controls are.
if rg -q 'button\.setTextColor\(accent\)' \
    "$repo_root/platforms/android/java/app/msime/client/keyboard/KeyboardLayoutAdjustView.java"; then
  echo "Android layout bar buttons must take actionForeground, not the accent they sit on" >&2
  exit 1
fi
# The JVM smokes cannot load org.json, so nothing else here can reach the one place where the
# shared runtime's JSON nulls meet this host's reads of them.
python3 "$repo_root/scripts/test-android-json-null-reads.py" || exit 1
echo "Android service Java/API and manifest/resource checks passed; no installable/native APK produced"
