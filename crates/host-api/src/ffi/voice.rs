//! Voice provider wire formats and the streaming recognition exchange.
//!
//! Part of the C ABI; see the parent module for what these shims guarantee.

use crate::*;

/// Decode one Doubao v1 response frame for Apple hosts. The returned payload
/// is UTF-8 JSON text; no frame bytes or credentials are retained.
///
/// # Safety
/// `frame` must reference a readable buffer for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn msime_client_doubao_decode_frame(
    frame: *const u8,
    frame_length: usize,
) -> *mut c_char {
    response(|| {
        if frame.is_null() || frame_length == 0 || frame_length > 1_048_576 {
            return Err("invalid Doubao frame buffer".into());
        }
        let bytes = unsafe { std::slice::from_raw_parts(frame, frame_length) };
        if let Some((last, _sequence, payload)) = decode_json_frame(bytes) {
            let text = String::from_utf8(payload).map_err(|_| "Doubao payload is not UTF-8")?;
            return Ok(json!({ "last": last, "payload": text }));
        }
        if let Some(code) = decode_error_code(bytes) {
            return Ok(json!({ "error_code": code }));
        }
        Err("invalid Doubao response frame".into())
    })
}

unsafe fn write_doubao_frame(
    frame: Vec<u8>,
    output: *mut u8,
    output_capacity: usize,
    output_length: *mut usize,
) -> bool {
    // A null, zero-capacity output is the sizing probe used by mobile bindings. Report the
    // required length before returning false so the caller can allocate exactly one frame. A null
    // output with nonzero capacity is still an invalid destination and must not be accepted.
    if output_length.is_null() || (output.is_null() && output_capacity != 0) {
        return false;
    }
    *output_length = frame.len();
    if output_capacity < frame.len() {
        return false;
    }
    std::ptr::copy_nonoverlapping(frame.as_ptr(), output, frame.len());
    true
}

/// Build a Doubao start request into caller-owned storage.
///
/// # Safety
/// `boosting_table_id` must point to `boosting_table_id_length` readable bytes when the length is
/// nonzero. `output` must point to `output_capacity` writable bytes and `output_length` must point
/// to a writable `usize`.
#[no_mangle]
pub unsafe extern "C" fn msime_client_doubao_start_frame(
    enable_itn: bool,
    enable_punc: bool,
    enable_ddc: bool,
    boosting_table_id: *const u8,
    boosting_table_id_length: usize,
    output: *mut u8,
    output_capacity: usize,
    output_length: *mut usize,
) -> bool {
    if boosting_table_id_length > 4096
        || (boosting_table_id.is_null() && boosting_table_id_length != 0)
    {
        return false;
    }
    let boosting = if boosting_table_id_length == 0 {
        ""
    } else {
        let bytes =
            unsafe { std::slice::from_raw_parts(boosting_table_id, boosting_table_id_length) };
        match std::str::from_utf8(bytes) {
            Ok(value) => value,
            Err(_) => return false,
        }
    };
    unsafe {
        write_doubao_frame(
            start_frame(enable_itn, enable_punc, enable_ddc, boosting),
            output,
            output_capacity,
            output_length,
        )
    }
}

/// Build a Doubao PCM or final audio frame into caller-owned storage.
///
/// # Safety
/// `pcm` must point to `pcm_length` readable bytes when the length is nonzero. `output` must point
/// to `output_capacity` writable bytes and `output_length` must point to a writable `usize`.
#[no_mangle]
pub unsafe extern "C" fn msime_client_doubao_audio_frame(
    sequence: i32,
    pcm: *const u8,
    pcm_length: usize,
    final_chunk: bool,
    output: *mut u8,
    output_capacity: usize,
    output_length: *mut usize,
) -> bool {
    if pcm_length > 1_048_576 || (pcm.is_null() && pcm_length != 0) {
        return false;
    }
    let bytes = if pcm_length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pcm, pcm_length) }
    };
    unsafe {
        write_doubao_frame(
            audio_frame(sequence, bytes, final_chunk),
            output,
            output_capacity,
            output_length,
        )
    }
}

/// Run one bounded voice capture/ASR request through a user-owned Unix socket.
/// The socket service owns microphone access, credentials and network policy.
/// The query is a bounded JSON object containing `language`, `generation`,
/// and optional non-sensitive voice behavior `options`.
///
/// # Safety
/// All pointers must reference readable buffers of the stated lengths for
/// the duration of this call; the buffers are not retained.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_provider_request(
    query: *const u8,
    query_length: usize,
    socket_path: *const u8,
    socket_length: usize,
) -> *mut c_char {
    response(|| {
        if query.is_null() || socket_path.is_null() || query_length > 16_384 || socket_length > 4096
        {
            return Err("invalid voice provider buffer".into());
        }
        #[derive(Deserialize)]
        struct VoiceQuery {
            language: String,
            generation: u64,
            #[serde(default)]
            options: Value,
        }
        let query = serde_json::from_slice::<VoiceQuery>(unsafe {
            std::slice::from_raw_parts(query, query_length)
        })
        .map_err(|_| "invalid voice query document")?;
        let path =
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(socket_path, socket_length) })
                .map_err(|_| "socket path is not UTF-8")?;
        if !std::path::Path::new(path).is_absolute() {
            return Err("socket path must be absolute".into());
        }
        Ok(UnixSocketProvider::new(path)
            .voice_with_options(&query.language, query.generation, &query.options)
            .map(|text| json!({"text": text}))
            .unwrap_or(Value::Null))
    })
}

/// Stream bounded interim/final voice provider updates from a user-owned
/// Unix socket. The callback is invoked synchronously on the calling thread.
///
/// # Safety
/// Buffers must remain readable for the duration of this call. The callback
/// must remain valid and must copy the text before returning.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_provider_stream(
    query: *const u8,
    query_length: usize,
    socket_path: *const u8,
    socket_length: usize,
    callback: Option<unsafe extern "C" fn(*const u8, usize, bool, *mut c_void)>,
    context: *mut c_void,
) -> *mut c_char {
    unsafe {
        msime_client_voice_provider_stream_events(
            query,
            query_length,
            socket_path,
            socket_length,
            callback,
            None,
            context,
        )
    }
}

/// Stream voice text and optional phase notifications (0 recording, 1 recognizing, 2 polishing).
///
/// # Safety
/// Buffers and callbacks must remain valid for this synchronous call. Callbacks must not unwind.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_provider_stream_events(
    query: *const u8,
    query_length: usize,
    socket_path: *const u8,
    socket_length: usize,
    callback: Option<unsafe extern "C" fn(*const u8, usize, bool, *mut c_void)>,
    status_callback: Option<unsafe extern "C" fn(u8, *mut c_void)>,
    context: *mut c_void,
) -> *mut c_char {
    unsafe {
        msime_client_voice_provider_stream_feedback(
            query,
            query_length,
            socket_path,
            socket_length,
            callback,
            status_callback,
            None,
            context,
        )
    }
}

/// Stream voice text, phases and optional normalized microphone levels.
///
/// # Safety
/// Buffers and callbacks must remain valid for this synchronous call. Callbacks must not unwind.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_provider_stream_feedback(
    query: *const u8,
    query_length: usize,
    socket_path: *const u8,
    socket_length: usize,
    callback: Option<unsafe extern "C" fn(*const u8, usize, bool, *mut c_void)>,
    status_callback: Option<unsafe extern "C" fn(u8, *mut c_void)>,
    level_callback: Option<unsafe extern "C" fn(f32, *mut c_void)>,
    context: *mut c_void,
) -> *mut c_char {
    response(|| {
        if query.is_null() || socket_path.is_null() || query_length > 16_384 || socket_length > 4096
        {
            return Err("invalid voice provider buffer".into());
        }
        #[derive(Deserialize)]
        struct VoiceQuery {
            language: String,
            generation: u64,
            #[serde(default)]
            options: Value,
        }
        let query = serde_json::from_slice::<VoiceQuery>(unsafe {
            std::slice::from_raw_parts(query, query_length)
        })
        .map_err(|_| "invalid voice query document")?;
        let path =
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(socket_path, socket_length) })
                .map_err(|_| "socket path is not UTF-8")?;
        if !std::path::Path::new(path).is_absolute() {
            return Err("socket path must be absolute".into());
        }
        let mut update = |text: &str, final_result: bool| {
            if let Some(callback) = callback {
                unsafe {
                    callback(text.as_ptr(), text.len(), final_result, context);
                }
            }
        };
        let mut status = |phase: &str| {
            if let Some(callback) = status_callback {
                let value = match phase {
                    "recording" => 0,
                    "recognizing" => 1,
                    "polishing" => 2,
                    _ => return,
                };
                unsafe {
                    callback(value, context);
                }
            }
        };
        let mut level = |value: f32| {
            if let Some(callback) = level_callback {
                unsafe {
                    callback(value, context);
                }
            }
        };
        let value = UnixSocketProvider::new(path).voice_stream_with_options_diagnosed(
            &query.language,
            query.generation,
            &query.options,
            None,
            &mut update,
            if status_callback.is_some() {
                Some(&mut status)
            } else {
                None
            },
            if level_callback.is_some() {
                Some(&mut level)
            } else {
                None
            },
        );
        match value {
            Ok(text) => Ok(json!({"text": text})),
            // A named missing dependency is the one provider failure reported as an error, so hosts can show what to install; older callers see it as any other failed call.
            Err(Some(detail)) => Err(format!("voice_dependency_missing:{detail}")),
            Err(None) => Ok(Value::Null),
        }
    })
}

/// Ask a user-owned Unix socket to stop voice capture for one generation.
///
/// # Safety
/// `socket_path` must reference a readable UTF-8 buffer for this call.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_provider_cancel(
    socket_path: *const u8,
    socket_length: usize,
    generation: u64,
) -> *mut c_char {
    response(|| {
        if socket_path.is_null() || socket_length > 4096 {
            return Err("invalid voice provider socket buffer".into());
        }
        let path =
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(socket_path, socket_length) })
                .map_err(|_| "socket path is not UTF-8")?;
        if !std::path::Path::new(path).is_absolute() {
            return Err("socket path must be absolute".into());
        }
        Ok(json!(UnixSocketProvider::new(path).voice_cancel(generation)))
    })
}

/// Ask a user-owned voice socket to finish capture and return its final stream
/// result. The streaming connection remains responsible for delivering text.
///
/// # Safety
/// `socket_path` must reference a readable UTF-8 buffer for this call.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_provider_stop(
    socket_path: *const u8,
    socket_length: usize,
    generation: u64,
) -> *mut c_char {
    response(|| {
        if socket_path.is_null() || socket_length > 4096 {
            return Err("invalid voice provider socket buffer".into());
        }
        let path =
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(socket_path, socket_length) })
                .map_err(|_| "socket path is not UTF-8")?;
        if !std::path::Path::new(path).is_absolute() {
            return Err("socket path must be absolute".into());
        }
        Ok(json!(UnixSocketProvider::new(path).voice_stop(generation)))
    })
}

// ---- on-device models and hotwords ----

/// Largest request any of the local-model and hotword calls accepts. The hotword correction request carries a transcript and up to a few hundred hotwords.
const LOCAL_VOICE_REQUEST_LIMIT: usize = 1 << 20;

fn local_voice_request<T: serde::de::DeserializeOwned>(
    request: *const u8,
    length: usize,
    limit: usize,
) -> Result<T, String> {
    if request.is_null() || length == 0 || length > limit {
        return Err("invalid voice request buffer".into());
    }
    // SAFETY: the caller contract of every entry point using this guarantees `length` readable bytes; null and size are checked above.
    let bytes = unsafe { std::slice::from_raw_parts(request, length) };
    serde_json::from_slice(bytes).map_err(|_| "invalid voice request".to_owned())
}

fn local_model_root(root: &str) -> Result<&Path, String> {
    if root.is_empty() || root.len() > 4096 || root.chars().any(char::is_control) {
        return Err("invalid local model root".into());
    }
    let path = Path::new(root);
    if !path.is_absolute() {
        return Err("invalid local model root".into());
    }
    Ok(path)
}

/// Cancellation flags of the installs running in this process, by model id.
fn local_model_installs() -> &'static Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>> {
    static INSTALLS: OnceLock<Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>> =
        OnceLock::new();
    INSTALLS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Hotwords for on-device recognition from the user's own pinyin dictionary words.
///
/// Request `{"options": HostOptions, "limit": 200}`, the same HostOptions `msime_client_dictionary` takes. Response `{"hotwords":[{"text","pinyin"}]}`, highest-weighted words first. Reads through the dictionary list action, so it fails with "dictionary maintenance busy" while a maintenance writer holds the store.
///
/// # Safety
/// `request` must point to `length` readable bytes. Null is rejected.
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_hotwords(
    request: *const u8,
    length: usize,
) -> *mut c_char {
    response(|| {
        #[derive(Deserialize)]
        struct HotwordsRequest {
            options: Value,
            #[serde(default)]
            limit: Option<usize>,
        }
        let request: HotwordsRequest =
            local_voice_request(request, length, HOST_OPTIONS_DOCUMENT_LIMIT)?;
        let limit = request
            .limit
            .unwrap_or(msime_client_core::voice::hotwords::DEFAULT_HOTWORD_LIMIT)
            .min(1_000);
        // Enough rows that the heaviest words can be picked even from a large dictionary, without reading the whole store for every voice session.
        const PAGE: usize = 1_000;
        const MAX_ROWS: usize = 5_000;
        let mut rows: Vec<(String, String, i64)> = Vec::new();
        let mut offset = 0;
        while limit > 0 && offset < MAX_ROWS {
            let page = crate::dictionary_request_json(
                &serde_json::to_vec(&json!({
                    "options": request.options,
                    "action": {"operation": "list", "offset": offset, "limit": PAGE, "kind": "pinyin", "user_only": true},
                }))
                .map_err(|_| "invalid voice request")?,
            )?;
            let entries = page["entries"].as_array().cloned().unwrap_or_default();
            rows.extend(entries.iter().filter_map(|entry| {
                Some((
                    entry["value"].as_str()?.to_owned(),
                    entry["key"].as_str()?.to_owned(),
                    entry["weight"].as_i64().unwrap_or(0),
                ))
            }));
            offset += entries.len();
            if entries.is_empty() || page["has_more"].as_bool() != Some(true) {
                break;
            }
        }
        // Stable, so words of equal weight keep the dictionary's order.
        rows.sort_by_key(|(_, _, weight)| std::cmp::Reverse(*weight));
        let hotwords = msime_client_core::voice::hotwords::hotwords_from_entries(
            rows.iter()
                .map(|(text, pinyin, _)| (text.as_str(), pinyin.as_str())),
            limit,
        );
        Ok(json!({ "hotwords": hotwords }))
    })
}

/// Apply hotwords to a final transcript by pinyin similarity, for models whose manifest says `"hotwords": "pinyin"`.
///
/// Request `{"text": "...", "hotwords": [{"text","pinyin"}]}` (at most 1 MiB); response `{"text": "..."}`. Pure; no state is read.
///
/// # Safety
/// `request` must point to `length` readable bytes. Null is rejected.
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_hotword_correct(
    request: *const u8,
    length: usize,
) -> *mut c_char {
    response(|| {
        #[derive(Deserialize)]
        struct CorrectRequest {
            text: String,
            #[serde(default)]
            hotwords: Vec<msime_client_core::voice::hotwords::Hotword>,
        }
        let request: CorrectRequest =
            local_voice_request(request, length, LOCAL_VOICE_REQUEST_LIMIT)?;
        Ok(json!({
            "text": msime_client_core::voice::hotwords::correct(&request.text, &request.hotwords),
        }))
    })
}

/// The on-device model catalog with what is installed under a root.
///
/// Request `{"root": "<absolute dir>"}`; response `{"models": [LocalModelStatus], "default": "<id>"}`. A model's `path` is what `voice_input.asr_model_path` is set to when the user picks it.
///
/// # Safety
/// `request` must point to `length` readable bytes. Null is rejected.
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_local_models(
    request: *const u8,
    length: usize,
) -> *mut c_char {
    response(|| {
        #[derive(Deserialize)]
        struct ModelsRequest {
            root: String,
        }
        let request: ModelsRequest = local_voice_request(request, length, 16_384)?;
        let root = local_model_root(&request.root)?;
        Ok(json!({
            "models": msime_client_core::voice::local_models::list(root),
            "default": msime_client_core::voice::local_models::default_model_id(),
        }))
    })
}

/// Download, verify and install one catalog model. Blocks until done: call it on a worker thread.
///
/// Request `{"root": "<absolute dir>", "id": "<catalog id>", "mirror": ""}`; response `{"path": "<root>/<id>"}`. `progress` (may be null) is called on the calling thread with `{"id","stage","downloaded","total"}` JSON, stage one of download, verify, extract, done; the buffer is only valid during the call. `msime_client_voice_local_model_cancel` stops it from any thread, and the call then fails with "local_model_cancelled". One install per id at a time; a second fails with "local_model_install_running". Other failures are "local_model_*" codes (network, http_status, size_mismatch, checksum_mismatch, unsafe_archive, missing_file, io, invalid_mirror, unknown).
///
/// # Safety
/// `request` must point to `length` readable bytes. `progress` must stay valid for the call, must copy the buffer before returning and must not unwind.
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_local_model_install(
    request: *const u8,
    length: usize,
    progress: Option<unsafe extern "C" fn(*const u8, usize, *mut std::ffi::c_void)>,
    context: *mut std::ffi::c_void,
) -> *mut c_char {
    response(|| {
        #[derive(Deserialize)]
        struct InstallRequest {
            root: String,
            id: String,
            #[serde(default)]
            mirror: String,
        }
        let request: InstallRequest = local_voice_request(request, length, 16_384)?;
        let root = local_model_root(&request.root)?;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut installs = local_model_installs()
                .lock()
                .map_err(|_| "internal runtime failure")?;
            if installs.contains_key(&request.id) {
                return Err("local_model_install_running".into());
            }
            installs.insert(request.id.clone(), cancel.clone());
        }
        struct Registered<'a>(&'a str);
        impl Drop for Registered<'_> {
            fn drop(&mut self) {
                if let Ok(mut installs) = local_model_installs().lock() {
                    installs.remove(self.0);
                }
            }
        }
        let _registered = Registered(&request.id);
        let mut report = |event: msime_client_core::voice::local_models::InstallProgress| {
            if let Some(callback) = progress {
                let text = json!({
                    "id": request.id,
                    "stage": event.stage,
                    "downloaded": event.downloaded,
                    "total": event.total,
                })
                .to_string();
                unsafe {
                    callback(text.as_ptr(), text.len(), context);
                }
            }
        };
        let path = msime_client_core::voice::local_models::install(
            root,
            &request.id,
            &request.mirror,
            &mut report,
            &cancel,
        )
        .map_err(|error| error.to_string())?;
        Ok(json!({ "path": path.to_string_lossy() }))
    })
}

/// Stop a running install. Request `{"id": "<catalog id>"}` cancels that model's install; a null request, or one without `id`, cancels every install in this process. Response value: whether anything was running. The install call itself returns once it notices, between chunks.
///
/// # Safety
/// `request` must be null or point to `length` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_local_model_cancel(
    request: *const u8,
    length: usize,
) -> *mut c_char {
    response(|| {
        #[derive(Deserialize)]
        struct CancelRequest {
            #[serde(default)]
            id: Option<String>,
        }
        let id = if request.is_null() || length == 0 {
            None
        } else {
            local_voice_request::<CancelRequest>(request, length, 16_384)?.id
        };
        let installs = local_model_installs()
            .lock()
            .map_err(|_| "internal runtime failure")?;
        let mut cancelled = false;
        for (running, flag) in installs.iter() {
            if id.as_ref().is_none_or(|id| id == running) {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                cancelled = true;
            }
        }
        Ok(json!(cancelled))
    })
}

/// Delete an installed model. Request `{"root": "<absolute dir>", "id": "<catalog id>"}`; only catalog ids are accepted. Removing a model that is not installed succeeds. Response value: null.
///
/// # Safety
/// `request` must point to `length` readable bytes. Null is rejected.
#[no_mangle]
pub unsafe extern "C" fn msime_client_voice_local_model_remove(
    request: *const u8,
    length: usize,
) -> *mut c_char {
    response(|| {
        #[derive(Deserialize)]
        struct RemoveRequest {
            root: String,
            id: String,
        }
        let request: RemoveRequest = local_voice_request(request, length, 16_384)?;
        let root = local_model_root(&request.root)?;
        // Hold the registry across the removal: releasing it after the check would let an
        // install of the same id register and have its staging directory swept mid-download.
        let installs = local_model_installs()
            .lock()
            .map_err(|_| "internal runtime failure")?;
        if installs.contains_key(&request.id) {
            return Err("local_model_install_running".into());
        }
        let removed = msime_client_core::voice::local_models::remove(root, &request.id);
        drop(installs);
        removed.map_err(|error| error.to_string())?;
        Ok(Value::Null)
    })
}
