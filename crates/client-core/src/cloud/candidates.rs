//! Safe, host-independent contract for the Windows cloud-candidate service.

use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const MAX_INPUT: usize = 256;
const MAX_RESPONSE: usize = 256 * 1024;
/// How long a cloud candidate is worth waiting for, connecting and in total.
///
/// The reference gives every phase of the request 2000 ms: its cloud worker fetches over WinHTTP
/// and calls `WinHttpSetTimeouts(2000, 2000, 2000, 2000)` (`cloud/cloud_ime.cpp`). Each host here
/// had picked its own number, which is why these live in one place now even though the current
/// values agree with it - `scripts/test-cloud-request-budget.py` keeps them reading the same ones.
///
/// An earlier revision of this comment said 2000 connecting and 2500 in total, and this file said
/// 2500. That came from `cloud/cloud_request.cpp`, a libcurl path that no longer exists at the
/// pinned reference: it was read out of an older checkout. The hosts had it right.
pub const CONNECT_TIMEOUT_MS: u64 = 2000;
pub const REQUEST_TIMEOUT_MS: u64 = 2000;
const MAX_CANDIDATE: usize = 512;
const MAX_CACHE_ENTRIES: usize = 4096;

#[derive(Debug)]
pub struct TranslationCache {
    positive: HashMap<String, (String, Instant)>,
    negative: HashMap<String, Instant>,
    negative_ttl: Duration,
}

impl TranslationCache {
    pub fn new(negative_ttl: Duration) -> Self {
        Self {
            positive: HashMap::new(),
            negative: HashMap::new(),
            negative_ttl,
        }
    }

    pub fn get(&mut self, key: &str) -> Option<Option<String>> {
        if let Some((value, _)) = self.positive.get(key) {
            return Some(Some(value.clone()));
        }
        if let Some(expires) = self.negative.get(key).copied() {
            if expires > Instant::now() {
                return Some(None);
            }
            self.negative.remove(key);
        }
        None
    }

    pub fn remember(&mut self, key: String, value: Option<String>) {
        if self.positive.len() + self.negative.len() >= MAX_CACHE_ENTRIES {
            self.positive.clear();
            self.negative.clear();
        }
        match value {
            Some(value) => {
                self.positive.insert(key, (value, Instant::now()));
            }
            None => {
                self.negative
                    .insert(key, Instant::now() + self.negative_ttl);
            }
        }
    }
}

/// Host-side orchestration state for debounced cloud requests. Network I/O stays injected.
#[derive(Debug, Default)]
pub struct CloudCandidateState {
    generation: u64,
    input: String,
}

impl CloudCandidateState {
    pub fn update(&mut self, enabled: bool, input: &str) -> Option<(u64, String)> {
        self.input.clear();
        // Once the identity space is exhausted, do not wrap and let an old
        // request become indistinguishable from a newer one.  Clearing the
        // input above also invalidates the request that used the final
        // generation, so no result can be applied after exhaustion.
        if self.generation == u64::MAX {
            return None;
        }
        self.generation += 1;
        if !enabled
            || input.is_empty()
            || input.len() > MAX_INPUT
            || input.chars().any(|c| c.is_control())
        {
            return None;
        }
        self.input.push_str(input);
        Some((self.generation, self.input.clone()))
    }

    pub fn apply(&self, generation: u64, candidate: &str) -> Option<String> {
        if generation != self.generation
            || self.input.is_empty()
            || candidate.is_empty()
            || candidate.len() > MAX_CANDIDATE
            || candidate.chars().any(|c| c.is_control())
        {
            return None;
        }
        Some(candidate.to_owned())
    }
}

pub fn build_google_url(input: &str, japanese: bool) -> Option<String> {
    if input.is_empty() || input.len() > MAX_INPUT || input.chars().any(|c| c.is_control()) {
        return None;
    }
    let scheme = if japanese {
        "ja-t-i0-und"
    } else {
        "zh-t-i0-pinyin"
    };
    Some(format!(
        "https://inputtools.google.com/request?text={}&itc={scheme}&num=1&ie=utf-8&oe=utf-8",
        urlencoding(input)
    ))
}

pub fn parse_google_response(response: &[u8]) -> Option<String> {
    if response.len() > MAX_RESPONSE {
        return None;
    }
    let root: Value = serde_json::from_slice(response).ok()?;
    if root.get(0)?.as_str()? != "SUCCESS" {
        return None;
    }
    let candidate = root.get(1)?.get(0)?.get(1)?.get(0)?.as_str()?;
    let candidate = candidate.trim();
    if candidate.is_empty()
        || candidate.len() > MAX_CANDIDATE
        || candidate.chars().any(|c| c.is_control())
    {
        return None;
    }
    Some(candidate.to_owned())
}

fn urlencoding(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 15) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_and_parse_match_google_contract() {
        assert_eq!(build_google_url("ni hao", false).unwrap(), "https://inputtools.google.com/request?text=ni%20hao&itc=zh-t-i0-pinyin&num=1&ie=utf-8&oe=utf-8");
        let response = serde_json::json!(["SUCCESS", [["ni hao", ["你好", "你号"]]]]).to_string();
        assert_eq!(
            parse_google_response(response.as_bytes()),
            Some("你好".into())
        );
    }
    #[test]
    fn rejects_unsafe_input_and_response() {
        assert!(build_google_url("bad\n", false).is_none());
        assert!(parse_google_response(br#"["ERROR",[]]"#).is_none());
    }

    #[test]
    fn stale_requests_cannot_replace_newer_input() {
        let mut state = CloudCandidateState::default();
        let (old, _) = state.update(true, "ni").unwrap();
        let (new, _) = state.update(true, "ni hao").unwrap();
        assert!(state.apply(old, "你好").is_none());
        assert_eq!(state.apply(new, "你好"), Some("你好".into()));
        assert!(state.apply(new, &"字".repeat(513)).is_none());
        assert!(state.apply(new, "好\n").is_none());
    }

    #[test]
    fn generation_exhaustion_does_not_reuse_request_ids() {
        let mut state = CloudCandidateState {
            generation: u64::MAX - 1,
            input: "old".into(),
        };
        let (last, _) = state.update(true, "last").unwrap();
        assert_eq!(last, u64::MAX);
        assert_eq!(state.apply(last, "结果"), Some("结果".into()));

        assert!(state.update(true, "new").is_none());
        assert!(state.apply(last, "过期").is_none());
    }

    #[test]
    fn translation_cache_tracks_positive_and_negative_results() {
        let mut cache = TranslationCache::new(Duration::from_secs(60));
        cache.remember("positive".into(), Some("译文".into()));
        cache.remember("negative".into(), None);
        assert_eq!(cache.get("positive"), Some(Some("译文".into())));
        assert_eq!(cache.get("negative"), Some(None));
        cache
            .negative
            .insert("expired".into(), Instant::now() - Duration::from_millis(1));
        assert_eq!(cache.get("expired"), None);
    }
}
