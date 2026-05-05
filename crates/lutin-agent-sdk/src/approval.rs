use std::borrow::Cow;

use async_trait::async_trait;

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Approval {
    Allow,
    Deny(Cow<'static, str>),
}

/// Per-call gate invoked before tool dispatch; `Deny` short-circuits execution.
#[async_trait]
pub trait ApprovalPolicy: Send + Sync {
    async fn decide(&self, call: &lutin_llm::ToolCall) -> Approval;
}

pub struct AllowAll;

#[async_trait]
impl ApprovalPolicy for AllowAll {
    async fn decide(&self, _call: &lutin_llm::ToolCall) -> Approval {
        Approval::Allow
    }
}

pub struct DenyAll;

#[async_trait]
impl ApprovalPolicy for DenyAll {
    async fn decide(&self, _call: &lutin_llm::ToolCall) -> Approval {
        Approval::Deny(Cow::Borrowed("denied by policy"))
    }
}
