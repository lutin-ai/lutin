//! Pre-turn transcript compaction.
//!
//! When the persona opts in and the transcript has crossed the
//! threshold, fold the older prefix into a single
//! [`lutin_llm::Message::Summary`] and archive the originals to a
//! sidecar (`compaction_archive.json`) so the user can still inspect
//! what was dropped.

use std::path::Path;

use lutin_agent_sdk::Agent;
use lutin_workflow_sdk::agent::build_provider as sdk_build_provider;
use lutin_workflow_sdk::compaction::{CompactionConfig, CompactionOutcome, maybe_compact};
use principled::ChatEvent;
use serde::Serialize;
use tracing::{info, warn};

use crate::agent_build::ResolvedArgs;
use crate::projection::{build_summary_updated, project_history, project_metrics, write_summary};
use crate::runner::RunnerCtx;
use crate::store::{self, Entry, MessageMetrics, now_rfc3339};

/// Run pre-turn compaction when the persona enables it. On a successful
/// compaction the agent's `messages` are spliced in place by
/// [`maybe_compact`]; we mirror the splice into `entries` (so metrics
/// stay aligned), append a snapshot of the dropped messages to a
/// per-session archive sidecar, persist the new transcript, and
/// broadcast `HistoryReplaced` + `MetricsReplaced` so the UI can
/// rerender.
pub(crate) async fn run_compaction(
    ctx: &RunnerCtx,
    agent: &mut Agent,
    resolved: &ResolvedArgs,
    entries: &mut Vec<Entry>,
) {
    let Some(cfg) = CompactionConfig::from_persona(&resolved.persona) else {
        return;
    };
    let Some(provider_name) = resolved.persona.provider.as_deref() else {
        return;
    };
    let Some(provider_cfg) = resolved
        .settings
        .providers
        .iter()
        .find(|p| p.name == provider_name)
    else {
        warn!(provider = %provider_name, "compaction skipped: provider not configured");
        return;
    };
    let provider = match sdk_build_provider(provider_cfg) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "compaction skipped: provider build failed");
            return;
        }
    };
    let Some(model) = resolved
        .model_override
        .clone()
        .or_else(|| resolved.persona.model.clone())
    else {
        warn!("compaction skipped: persona has no model");
        return;
    };
    let model_id = lutin_llm::ModelId::new(model);

    let outcome = match maybe_compact(agent, &*provider, &model_id, &cfg).await {
        Ok(Some(o)) => o,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, "compaction failed; continuing with full transcript");
            return;
        }
    };

    apply_compaction_to_entries(entries, &outcome);
    if let Err(e) = append_compaction_archive(&ctx.state_dir, &outcome) {
        warn!(error = %e, "compaction archive write failed");
    }
    if let Err(e) = store::save(&ctx.state_dir, entries) {
        warn!(error = %e, "save transcript after compaction failed");
    }
    write_summary(&ctx.state_dir, &ctx.resolver, entries);
    let _ = ctx
        .events
        .send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = ctx
        .events
        .send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = ctx.events.send(build_summary_updated(entries));
    info!(
        kept = outcome.kept,
        archived = outcome.archived_prefix.len(),
        "compaction applied"
    );
}

/// Mirror the agent-side splice into `entries` so metrics align. The
/// summary entry gets a fresh timestamp and otherwise-empty metrics.
fn apply_compaction_to_entries(entries: &mut Vec<Entry>, outcome: &CompactionOutcome) {
    let start = outcome.summarize_range_start;
    let end = start + outcome.archived_prefix.len();
    if end > entries.len() {
        warn!(
            entries_len = entries.len(),
            end, "compaction range exceeds entries — skipping mirror splice"
        );
        return;
    }
    let summary_entry = Entry {
        message: lutin_llm::Message::Summary { text: outcome.summary.clone() },
        metrics: MessageMetrics {
            timestamp: Some(now_rfc3339()),
            ..Default::default()
        },
    };
    entries.splice(start..end, std::iter::once(summary_entry));
}

/// Append one compaction event to `<state_dir>/compaction_archive.json`.
fn append_compaction_archive(
    state_dir: &Path,
    outcome: &CompactionOutcome,
) -> std::io::Result<()> {
    #[derive(Serialize, serde::Deserialize)]
    struct Archived {
        at: String,
        summary: String,
        messages: Vec<lutin_llm::Message>,
    }
    let path = state_dir.join("compaction_archive.json");
    let mut all: Vec<Archived> = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                let aside = state_dir.join(format!(
                    "compaction_archive.corrupt-{}.json",
                    now_rfc3339().replace(':', "-")
                ));
                warn!(error = %e, original = %path.display(), preserved_at = %aside.display(),
                      "compaction archive unreadable; rotated aside");
                std::fs::rename(&path, &aside)?;
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    all.push(Archived {
        at: now_rfc3339(),
        summary: outcome.summary.clone(),
        messages: outcome.archived_prefix.clone(),
    });
    let body = serde_json::to_vec_pretty(&all)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = state_dir.join("compaction_archive.json.tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
