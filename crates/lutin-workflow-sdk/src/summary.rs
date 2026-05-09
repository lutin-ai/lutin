//! Aggregating per-entry token stats into a session-wide summary.
//!
//! Both chat and principled engines need the same arithmetic: walk a
//! transcript's per-message text-token counts, sum the prompt /
//! completion totals, and pluck the last prompt count as a proxy for
//! current context-window fill. Each engine projects its own `Entry`
//! shape into the iterator this module consumes; the wire-level
//! `ChatEvent::SummaryUpdated` is workflow-specific (different
//! variant tags) so each engine wraps the returned [`SummaryTotals`]
//! into its own event variant at the call site.

/// One entry's text-token stats, projected from a workflow's
/// per-message metrics. `None` means "no usage on this entry"
/// (e.g. a user message, or an assistant entry that pre-dated the
/// metrics sidecar). Both fields independently optional because some
/// providers report only one side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryTokens {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

/// Aggregated counters for a session. `context_tokens` is the most
/// recent entry's `prompt_tokens` — proxy for current context-window
/// fill; the totals are cumulative across every entry seen so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SummaryTotals {
    pub context_tokens: Option<u32>,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
}

/// Walk every entry's projected token counts and aggregate into a
/// [`SummaryTotals`]. `context_tokens` tracks the *last* entry that
/// reported a `prompt_tokens` value — same convention as the
/// `summary.json::context_tokens` field that the desktop sidebar
/// renders.
pub fn aggregate(entries: impl IntoIterator<Item = EntryTokens>) -> SummaryTotals {
    let mut out = SummaryTotals::default();
    for e in entries {
        if let Some(p) = e.prompt_tokens {
            out.total_prompt_tokens = out.total_prompt_tokens.saturating_add(p as u64);
            out.context_tokens = Some(p);
        }
        if let Some(c) = e.completion_tokens {
            out.total_completion_tokens = out.total_completion_tokens.saturating_add(c as u64);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_iter_yields_default() {
        let s = aggregate(std::iter::empty());
        assert_eq!(s, SummaryTotals::default());
    }

    #[test]
    fn context_tokens_tracks_last_prompt() {
        let s = aggregate([
            EntryTokens { prompt_tokens: Some(10), completion_tokens: Some(2) },
            EntryTokens { prompt_tokens: None, completion_tokens: None },
            EntryTokens { prompt_tokens: Some(25), completion_tokens: Some(7) },
        ]);
        assert_eq!(s.context_tokens, Some(25));
        assert_eq!(s.total_prompt_tokens, 35);
        assert_eq!(s.total_completion_tokens, 9);
    }

    #[test]
    fn entries_without_prompt_dont_reset_context() {
        // A trailing user-only entry must not blank the context-token
        // hint set by the prior assistant entry.
        let s = aggregate([
            EntryTokens { prompt_tokens: Some(40), completion_tokens: Some(5) },
            EntryTokens { prompt_tokens: None, completion_tokens: None },
        ]);
        assert_eq!(s.context_tokens, Some(40));
    }
}
