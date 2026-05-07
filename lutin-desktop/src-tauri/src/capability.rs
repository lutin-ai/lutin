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
