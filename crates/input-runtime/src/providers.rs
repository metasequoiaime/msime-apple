//! Talking to the provider processes the host runs: one bounded request/response
//! exchange over a Unix socket, and the worker that keeps online queries off the
//! keystroke path.

use super::*;

/// Build the default cloud request for an eligible online query. Hosts perform
/// the actual network I/O through their injected transport and then submit the
/// result to `Runtime::apply_online_candidate`.
pub fn cloud_request_url(query: &OnlineQuery) -> Option<String> {
    if !query.cloud_eligible || !query.cloud_candidates {
        return None;
    }
    msime_client_core::cloud::candidates::build_google_url(&query.query_text, query.scheme == 3)
}

/// Convert a host-fetched Google response into a bounded online result.
pub fn cloud_candidate_from_response(
    query: OnlineQuery,
    response: &[u8],
) -> Option<OnlineCandidate> {
    if !query.cloud_eligible || !query.cloud_candidates {
        return None;
    }
    let text = msime_client_core::cloud::candidates::parse_google_response(response)?;
    Some(OnlineCandidate {
        query,
        text,
        source: 0,
    })
}

/// Linux adapter for a user-owned provider over a local Unix socket.
/// Credentials and network policy remain in the socket service; only a
/// copied, bounded query crosses this boundary.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub struct UnixSocketProvider {
    path: PathBuf,
}

// The request and its terminating newline go out as one write. Sent separately
// they can arrive as two segments, and a provider that reads only the first and
// then closes leaves data unread in its own receive queue - which on a unix
// socket makes the kernel set ECONNRESET on this side, losing a reply it had
// already queued. Every provider here is line-framed, so there is never a reason
// to split the line.
#[cfg(unix)]
fn with_terminator(request: &str) -> String {
    let mut line = String::with_capacity(request.len() + 1);
    line.push_str(request);
    line.push('\n');
    line
}

// One-shot panel providers have a fixed transfer deadline, including writes.
// Check the response envelope before appending bytes, not after allocating it.
#[cfg(unix)]
fn exchange_panel_request(
    stream: &mut UnixStream,
    request: &str,
    response_limit: usize,
    timeout: std::time::Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + timeout;
    let line = with_terminator(request);
    let mut bytes = line.as_bytes();
    while !bytes.is_empty() {
        let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
        if remaining.is_zero() {
            return None;
        }
        stream.set_write_timeout(Some(remaining)).ok()?;
        match stream.write(bytes) {
            Ok(0) => return None,
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
        if remaining.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(remaining)).ok()?;
        let mut chunk = [0_u8; 1024];
        let count = match stream.read(&mut chunk) {
            Ok(0) => return String::from_utf8(bytes).ok(),
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        let end = chunk[..count].iter().position(|byte| *byte == b'\n');
        let consumed = end.map_or(count, |index| index + 1);
        if bytes.len() + consumed > response_limit {
            return None;
        }
        bytes.extend_from_slice(&chunk[..consumed]);
        if end.is_some() {
            return String::from_utf8(bytes).ok();
        }
    }
}

// Retain incomplete UTF-8/JSON lines across polling timeouts. Bound the
// buffer while reading, rather than after read_line has allocated the payload.
#[cfg(unix)]
fn read_voice_provider_line(
    stream: &mut UnixStream,
    pending: &mut Vec<u8>,
    deadline: std::time::Instant,
    cancelled: Option<&AtomicBool>,
) -> Option<String> {
    loop {
        if cancelled.is_some_and(|value| value.load(Ordering::Relaxed)) {
            return None;
        }
        let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
        if remaining.is_zero() {
            return None;
        }
        if let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            if end >= 16_384 {
                return None;
            }
            return String::from_utf8(pending.drain(..=end).collect()).ok();
        }
        if pending.len() >= 16_384 {
            return None;
        }
        stream
            .set_read_timeout(Some(remaining.min(std::time::Duration::from_millis(100))))
            .ok()?;
        let mut chunk = [0_u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(count) => pending.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return None,
        }
    }
}

#[cfg(unix)]
impl UnixSocketProvider {
    /// Provider services are user-owned processes reached through an
    /// owner-only runtime directory. Check the filesystem endpoint before
    /// connecting so a configuration cannot redirect requests to a symlink,
    /// a non-socket path, or a socket owned by a different user. Comparing
    /// the socket owner with its private parent matches the provider startup
    /// contract without a second platform-specific uid API.
    pub(crate) fn connect(&self) -> Option<UnixStream> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let parent = self.path.parent()?;
        let parent_metadata = std::fs::symlink_metadata(parent).ok()?;
        let socket_metadata = std::fs::symlink_metadata(&self.path).ok()?;
        let effective_uid = rustix::process::geteuid().as_raw();
        if parent_metadata.uid() != effective_uid
            || !parent_metadata.file_type().is_dir()
            || parent_metadata.mode() & 0o077 != 0
            || !socket_metadata.file_type().is_socket()
            || socket_metadata.uid() != parent_metadata.uid()
        {
            return None;
        }
        UnixStream::connect(&self.path).ok()
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn query(&self, query: OnlineQuery) -> Option<(String, u8)> {
        self.query_candidates(query)?.into_iter().next()
    }

    /// Accept one cloud and up to the configured number of AI suggestions.
    pub fn query_candidates(&self, mut query: OnlineQuery) -> Option<Vec<(String, u8)>> {
        if query.ai_context.len() > 1024 {
            return None;
        }
        if !query.ai_eligible || !query.ai_assistant.as_ref().is_some_and(|ai| ai.enabled) {
            query.ai_context.clear();
        }
        if query.query_text.len() > 4096 || query.identity.len() > 4096 {
            return None;
        }
        let timeout = if query.ai_eligible
            && !query.ai_cache_only
            && query.ai_assistant.as_ref().is_some_and(|ai| ai.enabled)
        {
            // Windows ai_assistant.cpp permits eight seconds for model inference, and the provider holds that budget itself. The extra second covers its worker start-up so a reply it accepted at the deadline is not dropped here; a cache probe never waits on the network.
            std::time::Duration::from_secs(9)
        } else {
            std::time::Duration::from_millis(500)
        };
        let mut stream = self.connect()?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_millis(500)))
            .ok()?;
        let request = json!({"version": 1, "kind": "online", "query": query}).to_string();
        if request.len() > 16384
            || stream
                .write_all(with_terminator(&request).as_bytes())
                .is_err()
        {
            return None;
        }
        // One response deadline: partial writes by the provider must not
        // restart the inference timeout or grow an unbounded line buffer.
        let deadline = std::time::Instant::now() + timeout;
        let mut bytes = Vec::new();
        loop {
            let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
            if remaining.is_zero() {
                return None;
            }
            stream.set_read_timeout(Some(remaining)).ok()?;
            let mut chunk = [0_u8; 1024];
            let count = stream.read(&mut chunk).ok()?;
            if count == 0 {
                return None;
            }
            let end = chunk[..count].iter().position(|byte| *byte == b'\n');
            bytes.extend_from_slice(&chunk[..end.map_or(count, |index| index + 1)]);
            if bytes.len() > 16384 {
                return None;
            }
            if end.is_some() {
                break;
            }
        }
        let line = String::from_utf8(bytes).ok()?;
        #[derive(Deserialize)]
        struct Reply {
            text: String,
            source: u8,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Response {
            Batch { candidates: Vec<Reply> },
            Single(Reply),
        }
        let replies = match serde_json::from_str::<Response>(&line).ok()? {
            Response::Batch { candidates } => candidates,
            Response::Single(reply) => vec![reply],
        };
        if replies.len() > 11 {
            return None;
        }
        let ai_limit = query
            .ai_assistant
            .as_ref()
            .filter(|ai| ai.enabled)
            .map_or(0, |ai| usize::from(ai.candidate_limit.clamp(1, 10)));
        let limits = [1, ai_limit];
        let mut source_counts = [0; 2];
        let mut candidates = Vec::new();
        for reply in replies {
            if reply.text.is_empty()
                || reply.text.len() > 4096
                || reply.source > 1
                || reply.text.chars().any(char::is_control)
            {
                return None;
            }
            if (reply.source == 0 && (!query.cloud_candidates || !query.cloud_eligible))
                || (reply.source == 1 && !query.ai_eligible)
            {
                continue;
            }
            // A provider may repeat a candidate while merging multiple backends. Duplicates do
            // not occupy a slot in the runtime, so discard them before enforcing the per-source
            // quota; otherwise one repeated value can hide a distinct candidate that still fits.
            if candidates.iter().any(|(text, _)| text == &reply.text) {
                continue;
            }
            let source = usize::from(reply.source);
            source_counts[source] += 1;
            if source_counts[source] > limits[source] {
                return None;
            }
            candidates.push((reply.text, reply.source));
        }
        Some(candidates)
    }

    pub fn translate(&self, query: TranslationQuery) -> Option<Vec<TranslationResult>> {
        const MAX_SENTENCE_CHARS: usize = 512;
        let candidate_limit = if query.sentence { 1 } else { 9 };
        if query.candidates.is_empty()
            || query.candidates.len() > candidate_limit
            || query.candidates.iter().any(|text| {
                text.is_empty()
                    || text.len() > 4096
                    || (query.sentence && text.chars().count() > MAX_SENTENCE_CHARS)
                    || text.chars().any(char::is_control)
            })
        {
            return None;
        }
        // Translation switched off: no candidate text leaves the host, not even to the local provider.
        if query.provider == Some(TranslationService::Off) {
            return Some(Vec::new());
        }
        let mut stream = self.connect()?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_millis(500)))
            .ok()?;
        let request = json!({"version": 1, "kind": "translation", "query": query}).to_string();
        if request.len() > 16384
            || stream
                .write_all(with_terminator(&request).as_bytes())
                .is_err()
        {
            return None;
        }
        // Leave room for the provider's six-second translation batch budget.
        // A partial response cannot renew this deadline or grow without bound.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut bytes = Vec::new();
        loop {
            let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
            if remaining.is_zero() {
                return None;
            }
            stream.set_read_timeout(Some(remaining)).ok()?;
            let mut chunk = [0_u8; 1024];
            let count = stream.read(&mut chunk).ok()?;
            if count == 0 {
                return None;
            }
            let end = chunk[..count].iter().position(|byte| *byte == b'\n');
            bytes.extend_from_slice(&chunk[..end.map_or(count, |index| index + 1)]);
            if bytes.len() > 131_072 {
                return None;
            }
            if end.is_some() {
                break;
            }
        }
        let line = String::from_utf8(bytes).ok()?;
        #[derive(Deserialize)]
        struct Reply {
            translations: Vec<TranslationResult>,
        }
        let reply: Reply = serde_json::from_str(&line).ok()?;
        if reply.translations.len() > candidate_limit
            || reply.translations.iter().any(|item| {
                item.text.len() > 4096
                    || item.translation.is_empty()
                    || item.translation.len() > 4096
                    || item.text.chars().any(char::is_control)
                    || item.translation.chars().any(char::is_control)
                    || !query.candidates.contains(&item.text)
            })
        {
            return None;
        }
        Some(reply.translations)
    }

    /// Ask a user-owned Linux provider to verify one service configuration.
    /// Private provider credentials never cross this socket boundary.
    pub fn test_credential(&self, service: &str, config: &Value) -> Option<CredentialTestResult> {
        if !matches!(
            service,
            "translation.tencent"
                | "translation.niutrans"
                | "translation.custom"
                | "voice.asr"
                | "voice.polish"
                | "ai.assistant"
        ) || !config.is_object()
        {
            return None;
        }
        let request = json!({
            "version": 1,
            "kind": "credential_test",
            "query": { "service": service, "config": config },
        })
        .to_string();
        if request.len() > 16_384 {
            return None;
        }
        let mut stream = self.connect()?;
        let line = exchange_panel_request(
            &mut stream,
            &request,
            4096,
            std::time::Duration::from_secs(45),
        )?;
        let result = serde_json::from_str::<CredentialTestResult>(&line).ok()?;
        (!result.message.is_empty()
            && result.message.len() <= 1024
            && !result.message.chars().any(char::is_control))
        .then_some(result)
    }

    /// List the models the user's AI service offers, through the same provider
    /// that holds its credential.
    ///
    /// The hosts that keep the token themselves fetch this catalogue directly.
    /// This one cannot: on Linux the token lives in the provider's owner-only
    /// file by design, so the provider is the only thing that can authenticate
    /// the request. `provider` and `endpoint` come from the settings page and the
    /// provider refuses unless its private configuration names the same two.
    pub fn ai_models(&self, provider: &str, endpoint: &str) -> Option<Vec<String>> {
        if provider.is_empty()
            || provider.len() > 64
            || endpoint.is_empty()
            || endpoint.len() > 2048
            || provider.chars().any(char::is_control)
            || endpoint.chars().any(char::is_control)
        {
            return None;
        }
        let request = json!({
            "version": 1,
            "kind": "ai_models",
            "query": { "provider": provider, "endpoint": endpoint },
        })
        .to_string();
        let mut stream = self.connect()?;
        let line = exchange_panel_request(
            &mut stream,
            &request,
            16_384,
            std::time::Duration::from_secs(15),
        )?;
        #[derive(Deserialize)]
        struct Reply {
            models: Vec<String>,
        }
        let reply: Reply = serde_json::from_str(&line).ok()?;
        (reply.models.len() <= 128
            && !reply.models.is_empty()
            && reply.models.iter().all(|model| {
                !model.is_empty() && model.len() <= 256 && !model.chars().any(char::is_control)
            }))
        .then_some(reply.models)
    }

    /// Run one polish request through the user's AI service and return its text.
    ///
    /// Same reason as `ai_models` for going through the provider, and the same
    /// agreement check plus the model, which a polish request actually runs on.
    /// The prompt and the sample text are the user's; they are not logged here and
    /// the provider does not cache them.
    pub fn ai_test(
        &self,
        provider: &str,
        endpoint: &str,
        model: &str,
        prompt: &str,
        text: &str,
    ) -> Option<String> {
        if provider.is_empty()
            || provider.len() > 64
            || endpoint.is_empty()
            || endpoint.len() > 2048
            || model.is_empty()
            || model.len() > 256
            || text.trim().is_empty()
            || text.len() > 8192
            || prompt.len() > 8192
            || [provider, endpoint, model]
                .iter()
                .any(|value| value.chars().any(char::is_control))
        {
            return None;
        }
        let request = json!({
            "version": 1,
            "kind": "ai_test",
            "query": {
                "provider": provider,
                "endpoint": endpoint,
                "model": model,
                "prompt": prompt,
                "text": text,
            },
        })
        .to_string();
        if request.len() > 32_768 {
            return None;
        }
        let mut stream = self.connect()?;
        let line = exchange_panel_request(
            &mut stream,
            &request,
            32_768,
            std::time::Duration::from_secs(20),
        )?;
        #[derive(Deserialize)]
        struct Reply {
            text: String,
        }
        let reply: Reply = serde_json::from_str(&line).ok()?;
        let polished = reply.text.trim();
        (!polished.is_empty()
            && polished.len() <= 16_384
            && !polished
                .chars()
                .any(|character| character.is_control() && character != '\n'))
        .then(|| polished.to_owned())
    }

    /// Ask the user-owned handwriting recognizer for up to twelve candidates.
    /// The Linux panel owns ink capture and presentation; this service owns
    /// model selection and any platform-specific recognizer integration.
    pub fn handwriting(&self, query: HandwritingQuery) -> Option<Vec<String>> {
        if query.language.len() > 64
            || query.strokes.is_empty()
            || query.strokes.len() > 32
            || query
                .strokes
                .iter()
                .any(|stroke| stroke.is_empty() || stroke.len() > 512)
        {
            return None;
        }
        let request = json!({"version": 1, "kind": "handwriting", "query": query}).to_string();
        if request.len() > 262_144 {
            return None;
        }
        let mut stream = self.connect()?;
        let line = exchange_panel_request(
            &mut stream,
            &request,
            524_288,
            std::time::Duration::from_millis(500),
        )?;
        #[derive(Deserialize)]
        struct Reply {
            candidates: Vec<String>,
        }
        let reply: Reply = serde_json::from_str(&line).ok()?;
        if reply.candidates.len() > 12
            || reply.candidates.iter().any(|candidate| {
                candidate.is_empty()
                    || candidate.len() > 4096
                    || candidate.chars().any(char::is_control)
            })
        {
            return None;
        }
        msime_engine_bridge::handwriting_order_candidates(&reply.candidates).ok()
    }

    /// Search the user-owned emoji catalog. Results stay outside the IBus
    /// session and can be rendered by any desktop panel toolkit.
    pub fn emoji(&self, query: EmojiPanelQuery) -> Option<Vec<EmojiPanelItem>> {
        if query.search.len() > 256
            || query.category.len() > 128
            || !(1..=96).contains(&query.limit)
        {
            return None;
        }
        let request = json!({"version": 1, "kind": "emoji", "query": query}).to_string();
        if request.len() > 16_384 {
            return None;
        }
        let mut stream = self.connect()?;
        let line = exchange_panel_request(
            &mut stream,
            &request,
            262_144,
            std::time::Duration::from_millis(500),
        )?;
        #[derive(Deserialize)]
        struct Reply {
            items: Vec<EmojiPanelItem>,
        }
        let reply: Reply = serde_json::from_str(&line).ok()?;
        if reply.items.len() > 96
            || reply.items.iter().any(|item| {
                item.text.is_empty() || item.text.len() > 64 || item.annotation.len() > 256
            })
        {
            return None;
        }
        Some(reply.items)
    }

    /// Run one bounded voice capture/ASR request through the user-owned
    /// service. The service owns PipeWire/ALSA access, credentials and the
    /// recognizer; the input host only receives bounded UTF-8 text.
    pub fn voice(&self, language: &str, generation: u64) -> Option<String> {
        self.voice_with_options(language, generation, &Value::Null)
    }

    /// Run a voice request with non-sensitive behavior options. Credentials
    /// are deliberately not accepted here; the provider owns authentication
    /// and may ignore options it does not understand.
    pub fn voice_with_options(
        &self,
        language: &str,
        generation: u64,
        options: &Value,
    ) -> Option<String> {
        self.voice_with_options_cancelled(language, generation, options, None)
    }

    /// Cancellable variant used by the IBus worker. The provider may still
    /// take up to the socket read timeout to answer, but cancellation never
    /// waits for recording or ASR completion.
    pub fn voice_with_options_cancelled(
        &self,
        language: &str,
        generation: u64,
        options: &Value,
        cancelled: Option<&AtomicBool>,
    ) -> Option<String> {
        self.voice_stream_with_options_cancelled(
            language,
            generation,
            options,
            cancelled,
            &mut |_, _| {},
        )
        .filter(|text| !text.is_empty())
    }

    /// Run a newline-delimited voice provider stream. Provider updates use
    /// `{text, type:"partial"}` (or `interim`) and the terminal update uses
    /// `{text, type:"final"}`. A legacy single `{text}` response is treated
    /// as final. Only bounded UTF-8 text crosses the host boundary.
    #[cfg(unix)]
    pub fn voice_stream_with_options_cancelled(
        &self,
        language: &str,
        generation: u64,
        options: &Value,
        cancelled: Option<&AtomicBool>,
        update: &mut dyn FnMut(&str, bool),
    ) -> Option<String> {
        self.voice_stream_with_options_events(
            language, generation, options, cancelled, update, None,
        )
    }

    /// Optionally negotiate recording/recognizing/polishing status events.
    /// Status callbacks never carry transcript text and are never final results.
    #[cfg(unix)]
    pub fn voice_stream_with_options_events(
        &self,
        language: &str,
        generation: u64,
        options: &Value,
        cancelled: Option<&AtomicBool>,
        update: &mut dyn FnMut(&str, bool),
        status: Option<&mut dyn FnMut(&str)>,
    ) -> Option<String> {
        self.voice_stream_with_options_feedback(
            language, generation, options, cancelled, update, status, None,
        )
    }

    /// Negotiate optional normalized microphone levels separately from transcript text.
    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    pub fn voice_stream_with_options_feedback(
        &self,
        language: &str,
        generation: u64,
        options: &Value,
        cancelled: Option<&AtomicBool>,
        update: &mut dyn FnMut(&str, bool),
        status: Option<&mut dyn FnMut(&str)>,
        level: Option<&mut dyn FnMut(f32)>,
    ) -> Option<String> {
        self.voice_stream_with_options_diagnosed(
            language, generation, options, cancelled, update, status, level,
        )
        .ok()
    }

    /// Same stream as `voice_stream_with_options_feedback`, but a provider that refuses the recording with `voice_dependency_missing` and a known `detail` (`"websockets"`, `"recorder"` or `"local_asr"`) comes back as `Err(Some(detail))`, so hosts can tell the user what to install. Every other failure, including an unknown detail, is `Err(None)`; provider-supplied text never crosses this boundary.
    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    pub fn voice_stream_with_options_diagnosed(
        &self,
        language: &str,
        generation: u64,
        options: &Value,
        cancelled: Option<&AtomicBool>,
        update: &mut dyn FnMut(&str, bool),
        status: Option<&mut dyn FnMut(&str)>,
        level: Option<&mut dyn FnMut(f32)>,
    ) -> Result<String, Option<&'static str>> {
        let mut missing_dependency = None;
        self.voice_stream_session(
            language,
            generation,
            options,
            cancelled,
            update,
            status,
            level,
            &mut missing_dependency,
        )
        .ok_or(missing_dependency)
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    fn voice_stream_session(
        &self,
        language: &str,
        generation: u64,
        options: &Value,
        cancelled: Option<&AtomicBool>,
        update: &mut dyn FnMut(&str, bool),
        mut status: Option<&mut dyn FnMut(&str)>,
        mut level: Option<&mut dyn FnMut(f32)>,
        missing_dependency: &mut Option<&'static str>,
    ) -> Option<String> {
        if generation == 0
            || language.len() > 64
            || cancelled.is_some_and(|value| value.load(Ordering::Relaxed))
        {
            return None;
        }
        let mut stream = self.connect()?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_millis(500)))
            .ok()?;
        let mut request = json!({
            "version": 1,
            "kind": "voice",
            "query": {"language": language, "generation": generation, "stream": true}
        });
        if let Some(query) = request.get_mut("query").and_then(Value::as_object_mut) {
            if options.is_object() && !options.as_object().is_some_and(|value| value.is_empty()) {
                query.insert("options".to_owned(), options.clone());
            }
        }
        let mut events = Vec::new();
        if status.is_some() {
            events.push("status");
        }
        if level.is_some() {
            events.push("level");
        }
        if !events.is_empty() {
            request["query"]["events"] = json!(events);
        }
        let request = request.to_string();
        if request.len() > 16_384
            || stream
                .write_all(with_terminator(&request).as_bytes())
                .is_err()
        {
            return None;
        }
        // Up to ten minutes of capture, two sixty-second ASR attempts and
        // optional polishing. Cancellation is checked at least every 100ms.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(730);
        let mut pending = Vec::new();
        loop {
            let line = read_voice_provider_line(&mut stream, &mut pending, deadline, cancelled)?;
            let value = serde_json::from_str::<Value>(line.trim_end()).ok()?;
            // Explicit stream events belong to the request generation. Keep
            // the documented bare terminal response from pre-stream
            // providers, but do not let a typed event omit its binding.
            let event_generation = value.get("generation").and_then(Value::as_u64);
            if event_generation != Some(generation) {
                // Keep compatibility with pre-stream providers, which return
                // a bare {"text": ...} terminal object, but require a binding
                // for every explicitly typed stream event.
                let typed_event = value.get("type").is_some() || value.get("event").is_some();
                if typed_event || event_generation.is_some() {
                    return None;
                }
            }
            if value.get("ok").and_then(Value::as_bool) == Some(false) {
                if value.get("error").and_then(Value::as_str) == Some("voice_dependency_missing") {
                    *missing_dependency = match value.get("detail").and_then(Value::as_str) {
                        Some("websockets") => Some("websockets"),
                        Some("recorder") => Some("recorder"),
                        Some("local_asr") => Some("local_asr"),
                        _ => None,
                    };
                }
                return None;
            }
            let text = value.get("text").and_then(Value::as_str).unwrap_or("");
            if text.len() > 4096 {
                return None;
            }
            let kind = value
                .get("type")
                .or_else(|| value.get("event"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if kind == "level" {
                let value = value.get("level").and_then(Value::as_f64)?;
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    return None;
                }
                if let Some(callback) = level.as_mut() {
                    callback(value as f32);
                }
                continue;
            }
            if kind == "status" {
                let phase = value.get("phase").and_then(Value::as_str)?;
                if !matches!(phase, "recording" | "recognizing" | "polishing") {
                    return None;
                }
                if let Some(callback) = status.as_mut() {
                    callback(phase);
                }
                continue;
            }
            let is_final = match kind {
                "partial" | "interim" | "update" => false,
                "final" | "done" | "commit" => true,
                _ => value.get("final").and_then(Value::as_bool).unwrap_or(true),
            };
            if text.is_empty() && !is_final {
                continue;
            }
            update(text, is_final);
            if is_final {
                return Some(text.to_owned());
            }
        }
    }

    /// Ask a user-owned voice provider to stop the active capture session.
    /// The generation is included so a provider cannot cancel a newer session.
    #[cfg(unix)]
    pub fn voice_cancel(&self, generation: u64) -> bool {
        if generation == 0 {
            return false;
        }
        let mut stream = match self.connect() {
            Some(stream) => stream,
            None => return false,
        };
        if stream
            .set_write_timeout(Some(std::time::Duration::from_millis(250)))
            .is_err()
        {
            return false;
        }
        let request = json!({
            "version": 1,
            "kind": "voice_cancel",
            "query": {"generation": generation}
        })
        .to_string();
        request.len() <= 4096
            && stream
                .write_all(with_terminator(&request).as_bytes())
                .is_ok()
    }

    /// Ask a user-owned voice provider to finish the active capture session.
    /// Unlike cancellation, a stop lets the streaming connection deliver its
    /// final transcription back to the caller.
    #[cfg(unix)]
    pub fn voice_stop(&self, generation: u64) -> bool {
        if generation == 0 {
            return false;
        }
        let mut stream = match self.connect() {
            Some(stream) => stream,
            None => return false,
        };
        if stream
            .set_write_timeout(Some(std::time::Duration::from_millis(250)))
            .is_err()
        {
            return false;
        }
        let request = json!({
            "version": 1,
            "kind": "voice_stop",
            "query": {"generation": generation}
        })
        .to_string();
        request.len() <= 4096
            && stream
                .write_all(with_terminator(&request).as_bytes())
                .is_ok()
    }

    /// Forward one validated account-backed dictionary operation to the
    /// user-owned service. The provider owns authentication, synchronization,
    /// and network policy; this adapter only carries bounded JSON.
    pub fn cloud_dictionary(&self, request: Value) -> Option<Value> {
        let encoded = json!({
            "version": 1,
            "kind": "cloud_dictionary",
            "request": request,
        })
        .to_string();
        if encoded.len() > 65_536 {
            return None;
        }
        let mut stream = self.connect()?;
        let line = exchange_panel_request(
            &mut stream,
            &encoded,
            65_536,
            std::time::Duration::from_secs(30),
        )?;
        let response = serde_json::from_str::<Value>(&line).ok()?;
        response.is_object().then_some(response)
    }

    /// Forward one account-backed cloud clipboard operation to the
    /// user-owned service. The provider owns authentication and retention.
    pub fn cloud_clipboard(&self, request: Value) -> Option<Value> {
        let encoded = json!({
            "version": 1,
            "kind": "cloud_clipboard",
            "request": request,
        })
        .to_string();
        if encoded.len() > 65_536 {
            return None;
        }
        let mut stream = self.connect()?;
        let line = exchange_panel_request(
            &mut stream,
            &encoded,
            65_536,
            std::time::Duration::from_secs(30),
        )?;
        let response = serde_json::from_str::<Value>(&line).ok()?;
        response.is_object().then_some(response)
    }
}

/// Bounded provider worker. Provider code runs off the host/IBus thread and
/// receives only copied query data. Results remain inert until the owner
/// applies them through Runtime::apply_online_candidate, which revalidates
/// session identity and Engine generation.
///
/// Only the newest query waits: a submit overwrites whatever is pending, so a
/// provider that is busy for seconds still answers the text the user has now,
/// not the first intermediate one that happened to fit a queue.
pub struct OnlineProviderWorker {
    pending: Arc<Mutex<Option<OnlineQuery>>>,
    wake: Option<mpsc::SyncSender<()>>,
    /// A latest-value slot keeps completed provider responses bounded too.
    /// An unbounded channel here would let a host that stopped polling grow
    /// memory once for every completed query, even though only the newest
    /// generation can ever be applied.
    results: Arc<Mutex<Option<OnlineCandidate>>>,
    join: Option<JoinHandle<()>>,
}

impl OnlineProviderWorker {
    pub fn spawn<F>(capacity: usize, provider: F) -> Result<Self, &'static str>
    where
        F: Fn(OnlineQuery) -> Option<(String, u8)> + Send + 'static,
    {
        Self::spawn_with_debounce(capacity, std::time::Duration::ZERO, provider)
    }

    /// `capacity` is kept for API stability and must be positive; the worker
    /// holds a single latest-value slot regardless.
    pub fn spawn_with_debounce<F>(
        capacity: usize,
        debounce: std::time::Duration,
        provider: F,
    ) -> Result<Self, &'static str>
    where
        F: Fn(OnlineQuery) -> Option<(String, u8)> + Send + 'static,
    {
        if capacity == 0 {
            return Err("provider queue capacity must be positive");
        }
        let pending = Arc::new(Mutex::new(None::<OnlineQuery>));
        let slot = Arc::clone(&pending);
        let results = Arc::new(Mutex::new(None::<OnlineCandidate>));
        let result_slot = Arc::clone(&results);
        // A wake-up signal only; the query itself lives in the slot. A full
        // signal channel already promises the worker will look again.
        let (wake, incoming) = mpsc::sync_channel::<()>(1);
        let join = thread::Builder::new()
            .name("msime-online-provider".into())
            .spawn(move || {
                while incoming.recv().is_ok() {
                    // Windows waits for input to settle instead of querying
                    // every intermediate text; a submit during the wait just
                    // replaces the query read when it ends.
                    if !debounce.is_zero() {
                        let deadline = std::time::Instant::now() + debounce;
                        loop {
                            let remaining =
                                deadline.saturating_duration_since(std::time::Instant::now());
                            if remaining.is_zero() {
                                break;
                            }
                            match incoming.recv_timeout(remaining) {
                                Ok(()) => {}
                                Err(mpsc::RecvTimeoutError::Timeout) => break,
                                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            }
                        }
                    }
                    // Hold the lock only for the swap, never across the provider.
                    let query = match slot.lock() {
                        Ok(mut pending) => pending.take(),
                        Err(poisoned) => poisoned.into_inner().take(),
                    };
                    let Some(query) = query else {
                        continue;
                    };
                    if let Some((text, source)) = provider(query.clone()) {
                        if text.is_empty() || source > 1 {
                            continue;
                        }
                        let candidate = OnlineCandidate {
                            query,
                            text,
                            source,
                        };
                        // Replacing a queued answer is safe: Runtime checks
                        // the query's session and generation before applying
                        // it, and only the newest answer can still be useful.
                        match result_slot.lock() {
                            Ok(mut result) => *result = Some(candidate),
                            Err(poisoned) => *poisoned.into_inner() = Some(candidate),
                        }
                    }
                }
            })
            .map_err(|_| "could not spawn provider worker")?;
        Ok(Self {
            pending,
            wake: Some(wake),
            results,
            join: Some(join),
        })
    }

    /// Replace the pending query with this one. False only after shutdown or
    /// when the worker is gone.
    pub fn submit(&self, query: OnlineQuery) -> bool {
        let Some(wake) = self.wake.as_ref() else {
            return false;
        };
        match self.pending.lock() {
            Ok(mut pending) => *pending = Some(query),
            Err(_) => return false,
        }
        !matches!(wake.try_send(()), Err(mpsc::TrySendError::Disconnected(_)))
    }

    pub fn try_recv(&self) -> Option<OnlineCandidate> {
        match self.results.lock() {
            Ok(mut result) => result.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    pub fn shutdown(mut self) {
        self.wake.take();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
