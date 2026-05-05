use crate::loop_control::FinishReason;

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub last_assistant: Option<lutin_llm::Message>,
    pub usage: lutin_llm::Usage,
    pub rounds: u32,
    /// Terminal reason. The error (if any) is carried inside
    /// [`FinishReason::Error`] so the two cannot disagree.
    pub finish_reason: FinishReason,
}
