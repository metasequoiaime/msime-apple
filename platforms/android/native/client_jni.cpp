#include <jni.h>
#include "msime_client.h"
// The polish presets carry their own prompt-injection wording, and there are already four copies
// of that text in this repository. This host reads the shared one rather than adding a fifth.
#include "voice/PolishPrompt.h"
// On-device recognition is the shared sherpa-onnx recognizer every desktop host uses, compiled into this library; the runtime itself (libsherpa-onnx-c-api.so and libonnxruntime.so from the pinned .aar) is packaged beside it and loaded by name on first use.
#include "voice/LocalAsr.h"
#include <atomic>
#include <chrono>
#include <cstring>
#include <exception>
#include <functional>
#include <memory>
#include <mutex>
#include <vector>
#include <fstream>
#include <limits>
#include <string>

struct SnapshotReader {
    explicit SnapshotReader(const std::string &path) : input(path, std::ios::in | std::ios::binary) {}
    std::ifstream input;
};

static intptr_t snapshotNext(void *context, uint8_t *buffer, size_t capacity) noexcept {
    auto *reader = static_cast<SnapshotReader *>(context);
    if (!reader || !buffer || capacity == 0) return -1;
    for (;;) {
        size_t length = 0;
        bool ended = false;
        // Reserve one byte for the NUL terminator used by the lightweight record discriminator.
        // A full buffer is still a malformed overlong line, never a reason to write past it.
        while (length + 1 < capacity) {
            const int value = reader->input.get();
            if (value == EOF) {
                if (!reader->input.eof()) return -1;
                ended = true;
                break;
            }
            if (value == '\n') {
                ended = true;
                break;
            }
            if (value == '\r' && reader->input.peek() == '\n') {
                reader->input.get();
                ended = true;
                break;
            }
            buffer[length++] = static_cast<uint8_t>(value);
        }
        if (!ended || length == 0) return ended && length == 0 ? 0 : -1;
        buffer[length] = 0;
        const char *type = std::strstr(reinterpret_cast<const char *>(buffer), "\"type\":\"");
        if (!type) return -1;
        type += 8;
        const bool engine_record = std::strncmp(type, "overlay\"", 8) == 0
            || std::strncmp(type, "position\"", 9) == 0
            || std::strncmp(type, "selection\"", 10) == 0;
        if (engine_record) return static_cast<intptr_t>(length);
        if (std::strncmp(type, "header\"", 7) == 0
                || std::strncmp(type, "entry\"", 6) == 0
                || std::strncmp(type, "footer\"", 7) == 0) continue;
        return -1;
    }
}

// Use UTF-8 byte arrays, not JNI modified UTF-8: supplementary characters in
// candidates and resource paths must survive the Java/native boundary unchanged.
static jbyteArray response(JNIEnv *env, char *value) {
    if (!value) return nullptr;
    size_t length = std::strlen(value);
    if (length > static_cast<size_t>(std::numeric_limits<jsize>::max())) {
        msime_client_string_free(value);
        env->ThrowNew(env->FindClass("java/lang/IllegalStateException"), "Native response too large");
        return nullptr;
    }
    jbyteArray output = env->NewByteArray(static_cast<jsize>(length));
    if (output) env->SetByteArrayRegion(output, 0, static_cast<jsize>(length), reinterpret_cast<const jbyte *>(value));
    msime_client_string_free(value);
    return output;
}

extern "C" {
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_loadPreferencesRaw(JNIEnv *env, jclass, jbyteArray directory) {
    if (!directory) return response(env, msime_client_load_preferences(nullptr, 0));
    jsize length = env->GetArrayLength(directory);
    jbyte *bytes = env->GetByteArrayElements(directory, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_load_preferences(reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(directory, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_typingStatisticsRaw(JNIEnv *env, jclass, jbyteArray request) {
    if (!request) return response(env, msime_client_typing_statistics(nullptr, 0));
    jsize length = env->GetArrayLength(request);
    jbyte *bytes = env->GetByteArrayElements(request, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_typing_statistics(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(request, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_vocabularyReviewRaw(JNIEnv *env, jclass, jbyteArray request) {
    if (!request) return response(env, msime_client_vocabulary_review(nullptr, 0));
    jsize length = env->GetArrayLength(request);
    jbyte *bytes = env->GetByteArrayElements(request, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_vocabulary_review(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(request, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_emojiCatalogRaw(JNIEnv *env, jclass, jbyteArray query, jbyteArray resources) {
    if (!query || !resources) {
        return response(env, msime_client_emoji_catalog_request(nullptr, 0, nullptr, 0));
    }
    jsize query_length = env->GetArrayLength(query);
    jbyte *query_bytes = env->GetByteArrayElements(query, nullptr);
    if (!query_bytes) return nullptr;
    jsize resources_length = env->GetArrayLength(resources);
    jbyte *resources_bytes = env->GetByteArrayElements(resources, nullptr);
    if (!resources_bytes) {
        env->ReleaseByteArrayElements(query, query_bytes, JNI_ABORT);
        return nullptr;
    }
    char *result = msime_client_emoji_catalog_request(
        reinterpret_cast<const uint8_t *>(query_bytes), static_cast<size_t>(query_length),
        reinterpret_cast<const uint8_t *>(resources_bytes), static_cast<size_t>(resources_length));
    env->ReleaseByteArrayElements(resources, resources_bytes, JNI_ABORT);
    env->ReleaseByteArrayElements(query, query_bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_candidateGlossesRaw(JNIEnv *env, jclass, jbyteArray request, jbyteArray resources) {
    if (!request || !resources) {
        return response(env, msime_client_candidate_gloss_request(nullptr, 0, nullptr, 0));
    }
    jsize request_length = env->GetArrayLength(request);
    jbyte *request_bytes = env->GetByteArrayElements(request, nullptr);
    if (!request_bytes) return nullptr;
    jsize resources_length = env->GetArrayLength(resources);
    jbyte *resources_bytes = env->GetByteArrayElements(resources, nullptr);
    if (!resources_bytes) {
        env->ReleaseByteArrayElements(request, request_bytes, JNI_ABORT);
        return nullptr;
    }
    char *result = msime_client_candidate_gloss_request(
        reinterpret_cast<const uint8_t *>(request_bytes), static_cast<size_t>(request_length),
        reinterpret_cast<const uint8_t *>(resources_bytes), static_cast<size_t>(resources_length));
    env->ReleaseByteArrayElements(resources, resources_bytes, JNI_ABORT);
    env->ReleaseByteArrayElements(request, request_bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_englishCompletionsRaw(JNIEnv *env, jclass, jbyteArray request, jbyteArray resources) {
    if (!request || !resources) {
        return response(env, msime_client_english_completions_request(nullptr, 0, nullptr, 0));
    }
    jsize request_length = env->GetArrayLength(request);
    jbyte *request_bytes = env->GetByteArrayElements(request, nullptr);
    if (!request_bytes) return nullptr;
    jsize resources_length = env->GetArrayLength(resources);
    jbyte *resources_bytes = env->GetByteArrayElements(resources, nullptr);
    if (!resources_bytes) {
        env->ReleaseByteArrayElements(request, request_bytes, JNI_ABORT);
        return nullptr;
    }
    char *result = msime_client_english_completions_request(
        reinterpret_cast<const uint8_t *>(request_bytes), static_cast<size_t>(request_length),
        reinterpret_cast<const uint8_t *>(resources_bytes), static_cast<size_t>(resources_length));
    env->ReleaseByteArrayElements(resources, resources_bytes, JNI_ABORT);
    env->ReleaseByteArrayElements(request, request_bytes, JNI_ABORT);
    return response(env, result);
}
static std::string utf8(JNIEnv *env, jbyteArray value) {
    if (!value) return {};
    jsize length = env->GetArrayLength(value);
    if (length <= 0) return {};
    jbyte *bytes = env->GetByteArrayElements(value, nullptr);
    if (!bytes) return {};
    std::string text(reinterpret_cast<const char *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(value, bytes, JNI_ABORT);
    return text;
}
// Which prompt the selected slot resolves to, decided by the shared header rather than here: the
// slot/legacy precedence has been wrong on individual hosts before, and it is one rule.
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_polishPromptRaw(JNIEnv *env, jclass, jbyteArray id, jbyteArray legacy, jbyteArray custom1, jbyteArray custom2, jbyteArray custom3) {
    msime::windows::PolishPromptSlots slots;
    slots.id = utf8(env, id);
    slots.legacy = utf8(env, legacy);
    slots.custom_1 = utf8(env, custom1);
    slots.custom_2 = utf8(env, custom2);
    slots.custom_3 = utf8(env, custom3);
    const std::string prompt = msime::windows::polish_prompt_for(slots);
    if (prompt.size() > static_cast<size_t>(std::numeric_limits<jsize>::max())) return nullptr;
    jbyteArray out = env->NewByteArray(static_cast<jsize>(prompt.size()));
    if (out) {
        env->SetByteArrayRegion(out, 0, static_cast<jsize>(prompt.size()),
                                reinterpret_cast<const jbyte *>(prompt.data()));
    }
    return out;
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_mobileClipboardHistoryRaw(JNIEnv *env, jclass, jbyteArray request) {
    if (!request) return response(env, msime_client_mobile_clipboard_history(nullptr, 0));
    jsize length = env->GetArrayLength(request);
    jbyte *bytes = env->GetByteArrayElements(request, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_mobile_clipboard_history(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(request, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_doubaoDecodeFrameRaw(JNIEnv *env, jclass, jbyteArray frame) {
    if (!frame) return response(env, msime_client_doubao_decode_frame(nullptr, 0));
    jsize length = env->GetArrayLength(frame);
    // The shared decoder rejects frames above one MiB. Check before asking JNI for a native view:
    // GetByteArrayElements may copy the entire Java array, so doing this after the call briefly
    // doubles an attacker-controlled oversized response and defeats the decoder's allocation
    // bound.
    if (length > 1024 * 1024) {
        return response(env, msime_client_doubao_decode_frame(nullptr, 0));
    }
    jbyte *bytes = env->GetByteArrayElements(frame, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_doubao_decode_frame(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(frame, bytes, JNI_ABORT);
    return response(env, result);
}
// The two frame builders write into a caller-owned buffer and report the size they need when it is
// too small. Ask first, then allocate exactly that: the alternative is a guessed ceiling that is
// either wasteful for a start frame or silently truncating for a long PCM chunk.
static jbyteArray build_frame(JNIEnv *env, const std::function<bool(uint8_t *, size_t, size_t *)> &build) {
    size_t needed = 0;
    if (!build(nullptr, 0, &needed) && needed == 0) return nullptr;
    if (needed == 0 || needed > static_cast<size_t>(std::numeric_limits<jsize>::max())) return nullptr;
    std::vector<uint8_t> buffer(needed);
    size_t written = 0;
    if (!build(buffer.data(), buffer.size(), &written) || written == 0 || written > buffer.size()) {
        return nullptr;
    }
    jbyteArray out = env->NewByteArray(static_cast<jsize>(written));
    if (out) {
        env->SetByteArrayRegion(out, 0, static_cast<jsize>(written),
                                reinterpret_cast<const jbyte *>(buffer.data()));
    }
    return out;
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_doubaoStartFrameRaw(JNIEnv *env, jclass, jboolean itn, jboolean punc, jboolean ddc, jbyteArray boosting) {
    std::string table = utf8(env, boosting);
    return build_frame(env, [&](uint8_t *out, size_t capacity, size_t *length) {
        return msime_client_doubao_start_frame(
            itn == JNI_TRUE, punc == JNI_TRUE, ddc == JNI_TRUE,
            table.empty() ? nullptr : reinterpret_cast<const uint8_t *>(table.data()),
            table.size(), out, capacity, length);
    });
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_doubaoAudioFrameRaw(JNIEnv *env, jclass, jint sequence, jbyteArray pcm, jint pcmLength, jboolean finalChunk) {
    jsize available = pcm ? env->GetArrayLength(pcm) : 0;
    if (pcmLength < 0 || pcmLength > available) return nullptr;
    jbyte *bytes = pcm && pcmLength > 0 ? env->GetByteArrayElements(pcm, nullptr) : nullptr;
    if (pcmLength > 0 && !bytes) return nullptr;
    jbyteArray out = build_frame(env, [&](uint8_t *buffer, size_t capacity, size_t *length) {
        return msime_client_doubao_audio_frame(
            static_cast<int32_t>(sequence), reinterpret_cast<const uint8_t *>(bytes),
            static_cast<size_t>(pcmLength), finalChunk == JNI_TRUE, buffer, capacity, length);
    });
    if (bytes) env->ReleaseByteArrayElements(pcm, bytes, JNI_ABORT);
    return out;
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_shuangpinKeyHintsRaw(JNIEnv *env, jclass, jbyteArray profile) {
    if (!profile) return response(env, msime_client_shuangpin_key_hints(nullptr, 0));
    jsize length = env->GetArrayLength(profile);
    jbyte *bytes = env->GetByteArrayElements(profile, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_shuangpin_key_hints(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(profile, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_savePreferencesRaw(JNIEnv *env, jclass, jbyteArray directory, jlong expected_revision, jbyteArray snapshot) {
    if (!directory || !snapshot || expected_revision < 0) {
        return response(env, msime_client_save_preferences(nullptr, 0, 0, nullptr, 0));
    }
    jsize directory_length = env->GetArrayLength(directory);
    jbyte *directory_bytes = env->GetByteArrayElements(directory, nullptr);
    if (!directory_bytes) return nullptr;
    jsize snapshot_length = env->GetArrayLength(snapshot);
    jbyte *snapshot_bytes = env->GetByteArrayElements(snapshot, nullptr);
    if (!snapshot_bytes) {
        env->ReleaseByteArrayElements(directory, directory_bytes, JNI_ABORT);
        return nullptr;
    }
    char *result = msime_client_save_preferences(
        reinterpret_cast<const uint8_t *>(directory_bytes), static_cast<size_t>(directory_length),
        static_cast<uint64_t>(expected_revision),
        reinterpret_cast<const uint8_t *>(snapshot_bytes), static_cast<size_t>(snapshot_length));
    env->ReleaseByteArrayElements(snapshot, snapshot_bytes, JNI_ABORT);
    env->ReleaseByteArrayElements(directory, directory_bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_personalDictionarySyncRaw(JNIEnv *env, jclass, jbyteArray options) {
    if (!options) return response(env, msime_client_personal_dictionary_sync(nullptr, 0));
    jsize length = env->GetArrayLength(options);
    jbyte *bytes = env->GetByteArrayElements(options, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_personal_dictionary_sync(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(options, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_prepareHostRaw(JNIEnv *env, jclass, jbyteArray options) {
    if (!options) return response(env, msime_client_prepare_host(nullptr, 0));
    jsize length = env->GetArrayLength(options);
    jbyte *bytes = env->GetByteArrayElements(options, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_prepare_host(reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(options, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_snapshotVersionRaw(JNIEnv *env, jclass, jbyteArray options) {
    if (!options) return response(env, msime_client_snapshot_version(nullptr, 0));
    jsize length = env->GetArrayLength(options);
    jbyte *bytes = env->GetByteArrayElements(options, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_snapshot_version(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(options, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_snapshotPrepareRaw(JNIEnv *env, jclass, jbyteArray request, jbyteArray file) {
    if (!request || !file) return response(env, msime_client_snapshot_prepare(nullptr, 0, nullptr, nullptr));
    jsize request_length = env->GetArrayLength(request);
    jbyte *request_bytes = env->GetByteArrayElements(request, nullptr);
    if (!request_bytes) return nullptr;
    jsize file_length = env->GetArrayLength(file);
    jbyte *file_bytes = env->GetByteArrayElements(file, nullptr);
    if (!file_bytes) {
        env->ReleaseByteArrayElements(request, request_bytes, JNI_ABORT);
        return nullptr;
    }
    std::string path(reinterpret_cast<const char *>(file_bytes), static_cast<size_t>(file_length));
    SnapshotReader reader(path);
    char *result = msime_client_snapshot_prepare(
        reinterpret_cast<const uint8_t *>(request_bytes), static_cast<size_t>(request_length),
        snapshotNext, &reader);
    env->ReleaseByteArrayElements(file, file_bytes, JNI_ABORT);
    env->ReleaseByteArrayElements(request, request_bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_snapshotDiscardRaw(JNIEnv *env, jclass, jlong handle) {
    if (handle <= 0) return response(env, msime_client_snapshot_discard(0));
    return response(env, msime_client_snapshot_discard(static_cast<uint64_t>(handle)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_snapshotActivateRaw(JNIEnv *env, jclass, jlong handle, jbyteArray expected) {
    if (!expected || handle <= 0) return response(env, msime_client_snapshot_activate(0, nullptr, 0));
    jsize length = env->GetArrayLength(expected);
    jbyte *bytes = env->GetByteArrayElements(expected, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_snapshot_activate(
        static_cast<uint64_t>(handle), reinterpret_cast<const uint8_t *>(bytes),
        static_cast<size_t>(length));
    env->ReleaseByteArrayElements(expected, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_updatePreferencesRaw(JNIEnv *env, jclass, jlong handle, jbyteArray snapshot) {
    if (!snapshot) return response(env, msime_client_update_preferences(static_cast<uint64_t>(handle), nullptr, 0));
    jsize length = env->GetArrayLength(snapshot);
    jbyte *bytes = env->GetByteArrayElements(snapshot, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_update_preferences(static_cast<uint64_t>(handle), reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(snapshot, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_createRaw(JNIEnv *env, jclass, jbyteArray options) {
    if (!options) return response(env, msime_client_create(nullptr, 0));
    jsize length = env->GetArrayLength(options);
    jbyte *bytes = env->GetByteArrayElements(options, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_create(reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(options, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_focusRaw(JNIEnv *env, jclass, jlong handle, jboolean focused) {
    return response(env, msime_client_focus(static_cast<uint64_t>(handle), focused == JNI_TRUE));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_setNineKeyModeRaw(JNIEnv *env, jclass, jlong handle, jboolean enabled) {
    return response(env, msime_client_set_nine_key_mode(static_cast<uint64_t>(handle), enabled == JNI_TRUE));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_setEnglishModeRaw(JNIEnv *env, jclass, jlong handle, jboolean enabled) {
    return response(env, msime_client_set_english_mode(static_cast<uint64_t>(handle), enabled == JNI_TRUE));
}
// Returns the converted text directly rather than a JSON envelope, which is why it reuses the
// same response helper: both are NUL-terminated strings this side must free. A null answer means
// the text was not convertible and the caller keeps the original.
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_mobileVoiceConfigurationRaw(JNIEnv *env, jclass, jbyteArray directory) {
    if (!directory) return response(env, msime_client_mobile_voice_configuration(nullptr, 0));
    jsize length = env->GetArrayLength(directory);
    jbyte *bytes = env->GetByteArrayElements(directory, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_mobile_voice_configuration(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(directory, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_simplifiedToTraditionalRaw(JNIEnv *env, jclass, jbyteArray text) {
    if (!text) return nullptr;
    jsize length = env->GetArrayLength(text);
    if (length <= 0) return nullptr;
    jbyte *bytes = env->GetByteArrayElements(text, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_simplified_to_traditional(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(text, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_resetCacheRaw(JNIEnv *env, jclass, jlong handle) {
    return response(env, msime_client_reset_cache(static_cast<uint64_t>(handle)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_setChinesePunctuationRaw(JNIEnv *env, jclass, jlong handle, jboolean enabled) {
    return response(env, msime_client_set_chinese_punctuation(static_cast<uint64_t>(handle), enabled == JNI_TRUE));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_setCharacterWidthRaw(JNIEnv *env, jclass, jlong handle, jboolean fullwidth) {
    return response(env, msime_client_set_character_width(static_cast<uint64_t>(handle), fullwidth == JNI_TRUE));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_characterRaw(JNIEnv *env, jclass, jlong handle, jint ascii, jboolean shift) {
    if (ascii < 0 || ascii > 127) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Engine character must be ASCII");
        return nullptr;
    }
    return response(env, msime_client_character(static_cast<uint64_t>(handle), static_cast<uint8_t>(ascii), shift == JNI_TRUE));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_punctuationWithContextRaw(JNIEnv *env, jclass, jlong handle, jint ascii, jint preceding) {
    if (ascii < 0 || ascii > 127 || preceding < 0) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid punctuation context");
        return nullptr;
    }
    return response(env, msime_client_punctuation_with_context(static_cast<uint64_t>(handle), static_cast<uint8_t>(ascii), static_cast<uint32_t>(preceding)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_smartPunctuationArmRaw(JNIEnv *env, jclass, jlong handle, jbyteArray request) {
    if (!request) return response(env, msime_client_smart_punctuation_arm(static_cast<uint64_t>(handle), nullptr, 0));
    jsize length = env->GetArrayLength(request);
    jbyte *bytes = env->GetByteArrayElements(request, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_smart_punctuation_arm(static_cast<uint64_t>(handle),
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(request, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_smartPunctuationDecideRaw(JNIEnv *env, jclass, jlong handle, jbyteArray request) {
    if (!request) return response(env, msime_client_smart_punctuation_decide(static_cast<uint64_t>(handle), nullptr, 0));
    jsize length = env->GetArrayLength(request);
    jbyte *bytes = env->GetByteArrayElements(request, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_smart_punctuation_decide(static_cast<uint64_t>(handle),
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(request, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_commandRaw(JNIEnv *env, jclass, jlong handle, jint command) {
    return response(env, msime_client_command(static_cast<uint64_t>(handle), static_cast<uint32_t>(command)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_selectRaw(JNIEnv *env, jclass, jlong handle, jlong generation, jlong index) {
    if (index < 0 || static_cast<uint64_t>(index) > std::numeric_limits<size_t>::max()) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid candidate index");
        return nullptr;
    }
    return response(env, msime_client_select(static_cast<uint64_t>(handle), static_cast<uint64_t>(generation), static_cast<size_t>(index)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_selectAnyCandidateRaw(JNIEnv *env, jclass, jlong handle, jlong generation, jlong index) {
    if (index < 0 || static_cast<uint64_t>(index) > std::numeric_limits<size_t>::max()) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid candidate index");
        return nullptr;
    }
    return response(env, msime_client_select_any_candidate(static_cast<uint64_t>(handle), static_cast<uint64_t>(generation), static_cast<size_t>(index)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_pinCandidateRaw(JNIEnv *env, jclass, jlong handle, jlong generation, jlong index) {
    if (index < 0 || static_cast<uint64_t>(index) > std::numeric_limits<size_t>::max()) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid candidate index");
        return nullptr;
    }
    return response(env, msime_client_pin_candidate(static_cast<uint64_t>(handle), static_cast<uint64_t>(generation), static_cast<size_t>(index)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_fixCandidatePositionRaw(JNIEnv *env, jclass, jlong handle, jlong generation, jlong index, jint position) {
    if (index < 0 || static_cast<uint64_t>(index) > std::numeric_limits<size_t>::max()) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid candidate index");
        return nullptr;
    }
    if (position < 1 || position > 5) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Candidate position must be between 1 and 5");
        return nullptr;
    }
    return response(env, msime_client_fix_candidate_position(static_cast<uint64_t>(handle), static_cast<uint64_t>(generation), static_cast<size_t>(index), static_cast<uint8_t>(position)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_clearCandidatePositionRaw(JNIEnv *env, jclass, jlong handle, jlong generation, jlong index) {
    if (index < 0 || static_cast<uint64_t>(index) > std::numeric_limits<size_t>::max()) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid candidate index");
        return nullptr;
    }
    return response(env, msime_client_clear_candidate_position(static_cast<uint64_t>(handle), static_cast<uint64_t>(generation), static_cast<size_t>(index)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_removeCandidateRaw(JNIEnv *env, jclass, jlong handle, jlong generation, jlong index) {
    if (index < 0 || static_cast<uint64_t>(index) > std::numeric_limits<size_t>::max()) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid candidate index");
        return nullptr;
    }
    return response(env, msime_client_remove_candidate(static_cast<uint64_t>(handle), static_cast<uint64_t>(generation), static_cast<size_t>(index)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_selectEdgeRaw(JNIEnv *env, jclass, jlong handle, jlong generation, jlong index, jint edge) {
    if (index < 0 || static_cast<uint64_t>(index) > std::numeric_limits<size_t>::max()) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid candidate index");
        return nullptr;
    }
    if (edge != MSIME_FIRST_HAN && edge != MSIME_LAST_HAN) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid candidate edge");
        return nullptr;
    }
    return response(env, msime_client_select_edge(static_cast<uint64_t>(handle), static_cast<uint64_t>(generation), static_cast<size_t>(index), static_cast<uint8_t>(edge)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_chooseNineKeySpellingRaw(JNIEnv *env, jclass, jlong handle, jlong generation, jlong index) {
    if (index < 0 || static_cast<uint64_t>(index) > std::numeric_limits<size_t>::max()) {
        env->ThrowNew(env->FindClass("java/lang/IllegalArgumentException"), "Invalid nine-key spelling index");
        return nullptr;
    }
    return response(env, msime_client_choose_nine_key_spelling(static_cast<uint64_t>(handle), static_cast<uint64_t>(generation), static_cast<size_t>(index)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_viewRaw(JNIEnv *env, jclass, jlong handle) {
    return response(env, msime_client_view(static_cast<uint64_t>(handle)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_allCandidatesRaw(JNIEnv *env, jclass, jlong handle) {
    return response(env, msime_client_all_candidates(static_cast<uint64_t>(handle)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_applyTranslationsRaw(JNIEnv *env, jclass, jlong handle, jlong generation, jbyteArray translations) {
    if (!translations || generation < 0) {
        return response(env, msime_client_apply_translations(static_cast<uint64_t>(handle),
            static_cast<uint64_t>(generation), nullptr, 0));
    }
    jsize length = env->GetArrayLength(translations);
    jbyte *bytes = env->GetByteArrayElements(translations, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_apply_translations(static_cast<uint64_t>(handle),
        static_cast<uint64_t>(generation), reinterpret_cast<const uint8_t *>(bytes),
        static_cast<size_t>(length));
    env->ReleaseByteArrayElements(translations, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_onlineQueryRaw(JNIEnv *env, jclass, jlong handle) {
    return response(env, msime_client_online_query(static_cast<uint64_t>(handle)));
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_cloudRequestUrlRaw(JNIEnv *env, jclass, jbyteArray query) {
    if (!query) return response(env, msime_client_cloud_request_url(nullptr, 0));
    jsize length = env->GetArrayLength(query);
    jbyte *bytes = env->GetByteArrayElements(query, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_cloud_request_url(
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(query, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_aiRequestForQueryRaw(JNIEnv *env, jclass, jlong handle, jbyteArray query) {
    if (!query) {
        return response(env, msime_client_ai_request_for_query(
            static_cast<uint64_t>(handle), nullptr, 0));
    }
    jsize length = env->GetArrayLength(query);
    jbyte *bytes = env->GetByteArrayElements(query, nullptr);
    if (!bytes) return nullptr;
    char *result = msime_client_ai_request_for_query(static_cast<uint64_t>(handle),
        reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(query, bytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_applyCloudResponseRaw(JNIEnv *env, jclass, jlong handle, jbyteArray query, jbyteArray body) {
    if (!query || !body) {
        return response(env, msime_client_apply_cloud_response(
            static_cast<uint64_t>(handle), nullptr, 0, nullptr, 0));
    }
    jsize queryLength = env->GetArrayLength(query);
    jsize bodyLength = env->GetArrayLength(body);
    jbyte *queryBytes = env->GetByteArrayElements(query, nullptr);
    if (!queryBytes) return nullptr;
    jbyte *bodyBytes = env->GetByteArrayElements(body, nullptr);
    if (!bodyBytes) {
        env->ReleaseByteArrayElements(query, queryBytes, JNI_ABORT);
        return nullptr;
    }
    char *result = msime_client_apply_cloud_response(static_cast<uint64_t>(handle),
        reinterpret_cast<const uint8_t *>(queryBytes), static_cast<size_t>(queryLength),
        reinterpret_cast<const uint8_t *>(bodyBytes), static_cast<size_t>(bodyLength));
    env->ReleaseByteArrayElements(body, bodyBytes, JNI_ABORT);
    env->ReleaseByteArrayElements(query, queryBytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_applyOnlineCandidatesRaw(JNIEnv *env, jclass, jlong handle, jbyteArray query, jbyteArray candidates, jint source) {
    if (!query || !candidates || source < 0 || source > 255) {
        return response(env, msime_client_apply_online_candidates(
            static_cast<uint64_t>(handle), nullptr, 0, nullptr, 0, 0));
    }
    jsize queryLength = env->GetArrayLength(query);
    jsize candidatesLength = env->GetArrayLength(candidates);
    jbyte *queryBytes = env->GetByteArrayElements(query, nullptr);
    if (!queryBytes) return nullptr;
    jbyte *candidateBytes = env->GetByteArrayElements(candidates, nullptr);
    if (!candidateBytes) {
        env->ReleaseByteArrayElements(query, queryBytes, JNI_ABORT);
        return nullptr;
    }
    char *result = msime_client_apply_online_candidates(static_cast<uint64_t>(handle),
        reinterpret_cast<const uint8_t *>(queryBytes), static_cast<size_t>(queryLength),
        reinterpret_cast<const uint8_t *>(candidateBytes), static_cast<size_t>(candidatesLength),
        static_cast<uint8_t>(source));
    env->ReleaseByteArrayElements(candidates, candidateBytes, JNI_ABORT);
    env->ReleaseByteArrayElements(query, queryBytes, JNI_ABORT);
    return response(env, result);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_destroyRaw(JNIEnv *env, jclass, jlong handle) {
    return response(env, msime_client_destroy(static_cast<uint64_t>(handle)));
}
}

// ---- on-device speech recognition ----

namespace {
// One dictation. The cancel flag is shared with the recognizer so a cancel from the window's thread stops a decode running on the worker; everything else is touched only by the worker that created the session.
struct LocalSpeech {
    std::shared_ptr<std::atomic_bool> cancelled = std::make_shared<std::atomic_bool>(false);
    std::unique_ptr<msime::voice::LocalAsrSession> session;
    std::mutex partial_mutex;
    std::string partial;
    bool partial_changed = false;
};

jbyteArray bytes_of(JNIEnv *env, const std::string &text) {
    if (text.size() > static_cast<size_t>(std::numeric_limits<jsize>::max())) return nullptr;
    jbyteArray out = env->NewByteArray(static_cast<jsize>(text.size()));
    if (out) env->SetByteArrayRegion(out, 0, static_cast<jsize>(text.size()), reinterpret_cast<const jbyte *>(text.data()));
    return out;
}

void throw_state(JNIEnv *env, const char *message) {
    jclass type = env->FindClass("java/lang/IllegalStateException");
    if (type) env->ThrowNew(type, message);
}

LocalSpeech *speech(jlong handle) { return reinterpret_cast<LocalSpeech *>(static_cast<intptr_t>(handle)); }

// Bytes of one request/response host call; null input goes to the host so its own validation answers.
template <typename Call> jbyteArray host_request(JNIEnv *env, jbyteArray request, Call call) {
    if (!request) return response(env, call(nullptr, 0));
    jsize length = env->GetArrayLength(request);
    jbyte *bytes = env->GetByteArrayElements(request, nullptr);
    if (!bytes) return nullptr;
    char *result = call(reinterpret_cast<const uint8_t *>(bytes), static_cast<size_t>(length));
    env->ReleaseByteArrayElements(request, bytes, JNI_ABORT);
    return response(env, result);
}
} // namespace

extern "C" {
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_voiceHotwordsRaw(JNIEnv *env, jclass, jbyteArray request) {
    return host_request(env, request, msime_client_voice_hotwords);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_voiceHotwordCorrectRaw(JNIEnv *env, jclass, jbyteArray request) {
    return host_request(env, request, msime_client_voice_hotword_correct);
}
JNIEXPORT jboolean JNICALL Java_app_msime_client_NativeClient_localSpeechAvailableRaw(JNIEnv *, jclass) {
    return msime::voice::sherpa_runtime_available() ? JNI_TRUE : JNI_FALSE;
}
JNIEXPORT jlong JNICALL Java_app_msime_client_NativeClient_localSpeechCreateRaw(JNIEnv *, jclass) {
    return static_cast<jlong>(reinterpret_cast<intptr_t>(new LocalSpeech()));
}
// Loads the model (seconds on a phone the first time; cached afterwards) and opens the session. Hotwords arrive newline-separated. Returns null on success, else a message for logs.
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_localSpeechStartRaw(JNIEnv *env, jclass, jlong handle, jbyteArray model, jbyteArray language, jbyteArray hotwords, jint threads) {
    LocalSpeech *state = speech(handle);
    if (!state || state->session) return bytes_of(env, "invalid local speech session");
    msime::voice::LocalAsrOptions options;
    options.model_dir = utf8(env, model);
    options.language = utf8(env, language);
    options.threads = threads < 0 ? 0 : threads;
    const std::string words = utf8(env, hotwords);
    for (size_t start = 0; start < words.size();) {
        size_t end = words.find('\n', start);
        if (end == std::string::npos) end = words.size();
        if (end > start) options.hotwords.emplace_back(words.substr(start, end - start));
        start = end + 1;
    }
    try {
        state->session = std::make_unique<msime::voice::LocalAsrSession>(
            options,
            [state](const std::string &text) {
                std::lock_guard<std::mutex> lock(state->partial_mutex);
                state->partial = text;
                state->partial_changed = true;
            },
            state->cancelled);
        return nullptr;
    } catch (const std::exception &error) {
        return bytes_of(env, error.what());
    }
}
// Feeds 16 kHz mono PCM16. Returns the whole transcript so far when it changed, else null. Throws IllegalStateException when the session was cancelled or the recognizer failed.
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_localSpeechAcceptRaw(JNIEnv *env, jclass, jlong handle, jshortArray pcm, jint count) {
    LocalSpeech *state = speech(handle);
    if (!state || !state->session || !pcm || count < 0 || count > env->GetArrayLength(pcm)) {
        throw_state(env, "invalid local speech input");
        return nullptr;
    }
    std::vector<jshort> samples(static_cast<size_t>(count));
    env->GetShortArrayRegion(pcm, 0, count, samples.data());
    std::vector<float> floats(samples.size());
    for (size_t index = 0; index < samples.size(); index++) floats[index] = static_cast<float>(samples[index]) / 32768.0f;
    try {
        state->session->accept(floats.data(), floats.size());
    } catch (const std::exception &error) {
        throw_state(env, error.what());
        return nullptr;
    }
    std::lock_guard<std::mutex> lock(state->partial_mutex);
    if (!state->partial_changed) return nullptr;
    state->partial_changed = false;
    return bytes_of(env, state->partial);
}
JNIEXPORT jbyteArray JNICALL Java_app_msime_client_NativeClient_localSpeechFinishRaw(JNIEnv *env, jclass, jlong handle) {
    LocalSpeech *state = speech(handle);
    if (!state || !state->session) {
        throw_state(env, "invalid local speech session");
        return nullptr;
    }
    try {
        return bytes_of(env, state->session->finish());
    } catch (const std::exception &error) {
        throw_state(env, error.what());
        return nullptr;
    }
}
// Any thread: a decode in progress on the worker stops at its next check.
JNIEXPORT void JNICALL Java_app_msime_client_NativeClient_localSpeechCancelRaw(JNIEnv *, jclass, jlong handle) {
    if (LocalSpeech *state = speech(handle)) state->cancelled->store(true);
}
JNIEXPORT void JNICALL Java_app_msime_client_NativeClient_localSpeechDestroyRaw(JNIEnv *, jclass, jlong handle) {
    delete speech(handle);
}
// Drops models no session has used for `idleMillis`; 0 drops every model not in use. Returns how many were dropped.
JNIEXPORT jint JNICALL Java_app_msime_client_NativeClient_localSpeechReleaseRaw(JNIEnv *, jclass, jlong idleMillis) {
    const size_t released = idleMillis <= 0
        ? msime::voice::release_local_models()
        : msime::voice::release_idle_local_models(std::chrono::milliseconds(idleMillis));
    return static_cast<jint>(released);
}
}
