//! Host-independent voice session lifecycle. Audio and ASR transports are injected.
//!
//! [`controller`] drives one recognition session over an injected transport;
//! [`doubao_frame`] is the bounded decoder for one provider's wire format.
//! [`local_models`] installs the on-device models the `local` provider runs, and [`hotwords`] turns the user dictionary into their hotword list.

pub mod controller;
pub mod doubao_frame;
pub mod hotwords;
pub mod local_models;
pub mod provider;

/// Platform-injected streaming voice transport.
pub trait VoiceTransport {
    type Error;
    fn start(&mut self, generation: u64) -> Result<(), Self::Error>;
    fn send_audio(&mut self, generation: u64, pcm16_mono_16khz: &[u8]) -> Result<(), Self::Error>;
    fn finish(&mut self, generation: u64) -> Result<(), Self::Error>;
    fn cancel(&mut self, generation: u64) -> Result<(), Self::Error>;
}

#[derive(Debug, Default)]
pub struct VoiceSessionState {
    generation: u64,
    active: bool,
}

impl VoiceSessionState {
    pub fn start(&mut self) -> u64 {
        // Generation zero is the inactive sentinel used by the provider lease
        // protocol. Once the identity space is exhausted, refuse to start a
        // new session instead of wrapping and accepting an old result.
        if self.generation == u64::MAX {
            self.active = false;
            return 0;
        }
        self.generation += 1;
        self.active = true;
        self.generation
    }

    pub fn cancel(&mut self) {
        // Keep the exhausted generation at its terminal value. Wrapping to
        // zero and then starting at one would eventually reuse an old token.
        self.generation = self.generation.saturating_add(1);
        self.active = false;
    }

    pub fn apply(&mut self, generation: u64, text: &str) -> Option<String> {
        if !self.active || generation != self.generation || text.is_empty() {
            return None;
        }
        self.active = false;
        Some(text.to_owned())
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_voice_results_are_rejected() {
        let mut state = VoiceSessionState::default();
        let old = state.start();
        state.cancel();
        let current = state.start();
        assert!(state.apply(old, "旧结果").is_none());
        assert_eq!(state.apply(current, "新结果"), Some("新结果".into()));
        assert!(!state.is_active());
    }

    #[test]
    fn generation_exhaustion_does_not_reuse_voice_ids() {
        let mut state = VoiceSessionState {
            generation: u64::MAX - 1,
            active: false,
        };
        let last = state.start();
        assert_eq!(last, u64::MAX);
        assert!(state.is_active());

        state.cancel();
        assert_eq!(state.start(), 0);
        assert!(!state.is_active());
        assert!(state.apply(last, "过期").is_none());
    }
}
