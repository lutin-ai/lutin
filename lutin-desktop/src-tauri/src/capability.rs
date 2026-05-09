//! Well-known plugin manifest capability names.
//!
//! Centralized so the literal lives in exactly one place. The
//! shim (`shim/lutin.js`) and the chat manifest
//! (`workflows/chat/ui/public/lutin.workflow.json`) repeat these
//! strings — by spec, since they're the wire format — but they
//! reference this module by comment so a rename here surfaces them.

/// Workflow opts in to receiving PTT / open-mic transcription
/// deliveries from chrome's hotkey routing. Match must be exact.
pub const RECEIVE_TRANSCRIPTION: &str = "receive_transcription";

/// Workflow opts in to driving TTS — `lutin.tts.{ensureBackend,
/// openStream, speak, cancel, closeStream}`. Without it, the shim
/// doesn't expose `lutin.tts` and chrome rejects any `tts-call`
/// envelope before invoking the Tauri command. The gate is enforced
/// chrome-side in `PluginIframe.tsx` (each iframe knows its own
/// manifest's capability set), mirroring the way the shim hides
/// `onTranscription` for workflows without `receive_transcription`.
/// `#[allow(dead_code)]` because the live-string check is in JS;
/// kept here as the Rust-side anchor for `grep capability::TTS`.
#[allow(dead_code)]
pub const TTS: &str = "tts";

/// Workflow opts in to publishing a sub-agent hierarchy that chrome
/// renders in its sidebar (chat workflow today). Without this,
/// the shim doesn't expose `lutin.publishSubAgents` /
/// `onSelectSubAgent` and chrome ignores `sub-agents-update`
/// envelopes — same shape as `TTS`/`RECEIVE_TRANSCRIPTION`.
#[allow(dead_code)]
pub const SUB_AGENTS: &str = "sub_agents";

#[cfg(test)]
mod tests {
    use super::*;

    /// Defensive — the strings show up as bare literals in
    /// `shim/lutin.js`, `PluginIframe.tsx`, and workflow manifests.
    /// If we ever rename a constant here we want the test to flag
    /// the JS side that didn't move.
    #[test]
    fn capability_string_values_are_stable() {
        assert_eq!(RECEIVE_TRANSCRIPTION, "receive_transcription");
        assert_eq!(TTS, "tts");
        assert_eq!(SUB_AGENTS, "sub_agents");
    }
}
