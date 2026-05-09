//! Chat-workflow-local tools the LLM uses to drive the parent's
//! sub-agent registry. Mounted via [`super::agents`]'s command channel
//! so the tools, the engine's completion handler, and the registry
//! actor all share one source of truth (no `Arc<Mutex>` in sight).

pub mod agent;

pub use agent::make_subagent_tools;
