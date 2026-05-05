use lutin_llm::ReasoningEffort;

#[derive(Debug, Clone, Default)]
pub struct SamplingParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    pub penalties: Option<PenaltyParams>,
    pub seed: Option<u64>,
    pub reasoning: Option<ReasoningParams>,
    pub thinking_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct PenaltyParams {
    pub presence: f32,
    pub frequency: f32,
}

#[derive(Debug, Clone, Default)]
pub struct ReasoningParams {
    pub effort: ReasoningEffort,
    pub max_tokens: Option<u32>,
}
