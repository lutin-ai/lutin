use lutin_llm::LlmError;

use crate::loop_control::RecoveryPolicy;

pub struct RetryBudget {
    policy: RecoveryPolicy,
    attempts: u32,
}

impl RetryBudget {
    pub fn new(policy: RecoveryPolicy) -> Self {
        Self { policy, attempts: 0 }
    }

    pub fn should_retry(&mut self, err: &LlmError) -> bool {
        match self.policy {
            RecoveryPolicy::FailFast => false,
            RecoveryPolicy::RetryTransient { max_attempts } => {
                if !is_transient(err) {
                    return false;
                }
                self.attempts += 1;
                self.attempts < max_attempts
            }
        }
    }
}

fn is_transient(err: &LlmError) -> bool {
    // Why: `LlmError::Timeout` was dropped in the new lutin-llm; transient
    // network/timeout failures now surface as `Http` (reqwest::Error) or `Stream`.
    matches!(
        err,
        LlmError::Http(_) | LlmError::RateLimited { .. } | LlmError::Stream(_)
    ) || matches!(err, LlmError::Api { status, .. } if *status >= 500)
}
