//! High-level constructors for the standard tool set.
//!
//! Workflows that want the full portable toolset call [`default_tools`] and
//! pass the result to [`Toolbox::new`]. [`filter_by_name`] applies a
//! persona-style whitelist/blacklist before composition. The two-step
//! shape (collect, then filter, then compose) means callers can splice in
//! workflow-specific tools at either point.

use std::sync::Arc;

use crate::context::ToolContext;
use crate::Tool;

/// Persona-style filter mode. Mirrors `lutin_entities::ToolFilterMode` but
/// is duplicated here so `lutin-tools` doesn't depend on `lutin-entities`
/// (which would pull `lutin-storage`, `lutin-ids`, etc. into the tool
/// crate's compile graph).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// Keep only tools whose name appears in `list`.
    Whitelist,
    /// Drop tools whose name appears in `list`; keep everything else.
    Blacklist,
}

/// Build the full set of portable tools wired to one shared
/// [`ToolContext`]. `web_search_api_key`, when `Some`, is forwarded to
/// the `web_search` tool instead of the global `BRAVE_SEARCH_API_KEY`
/// env var — letting workflow embedders feed the key from settings
/// without mutating process env.
pub fn default_tools(
    ctx: Arc<ToolContext>,
    web_search_api_key: Option<String>,
) -> Vec<Box<dyn Tool>> {
    let web_search: Box<dyn Tool> = match web_search_api_key {
        Some(k) => Box::new(crate::web_search::WebSearch::with_api_key(ctx.clone(), k)),
        None => Box::new(crate::web_search::WebSearch::new(ctx.clone())),
    };
    vec![
        Box::new(crate::shell::Shell::new(ctx.clone())),
        Box::new(crate::file_read::FileRead::new(ctx.clone())),
        Box::new(crate::file_write::FileWrite::new(ctx.clone())),
        Box::new(crate::file_edit::FileEdit::new(ctx.clone())),
        Box::new(crate::file_edit_lines::FileEditLines::new(ctx.clone())),
        Box::new(crate::file_list::FileList::new(ctx.clone())),
        Box::new(crate::file_glob::FileGlob::new(ctx.clone())),
        Box::new(crate::file_grep::FileGrep::new(ctx.clone())),
        Box::new(crate::file_tree::FileTree::new(ctx.clone())),
        Box::new(crate::http_request::HttpRequest::new(ctx.clone())),
        web_search,
        Box::new(crate::image_view::ImageView::new(ctx.clone())),
        Box::new(crate::wait::Wait::new()),
    ]
}

/// Apply a persona-style filter to a tool list. Names are matched against
/// each tool's `definition().name`; unknown names in `list` are silently
/// ignored (a missing tool isn't an error — personas are forward-
/// compatible with future tool additions).
pub fn filter_by_name(
    tools: Vec<Box<dyn Tool>>,
    mode: FilterMode,
    list: &[String],
) -> Vec<Box<dyn Tool>> {
    if list.is_empty() {
        return match mode {
            // Empty whitelist = nothing allowed.
            FilterMode::Whitelist => Vec::new(),
            // Empty blacklist = everything allowed.
            FilterMode::Blacklist => tools,
        };
    }
    let in_list = |t: &dyn Tool| list.iter().any(|n| n == t.definition().name.as_str());
    tools
        .into_iter()
        .filter(|t| match mode {
            FilterMode::Whitelist => in_list(t.as_ref()),
            FilterMode::Blacklist => !in_list(t.as_ref()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_state::ReadState;
    use std::path::PathBuf;

    fn ctx() -> Arc<ToolContext> {
        Arc::new(ToolContext {
            root: PathBuf::from("/tmp"),
            env: Arc::from([]),
            http: reqwest::Client::new(),
            read_state: Arc::new(ReadState::new(PathBuf::from("/tmp"))),
        })
    }

    #[test]
    fn default_set_is_nonempty_and_unique() {
        let tools = default_tools(ctx(), None);
        let names: Vec<String> = tools
            .iter()
            .map(|t| t.definition().name.as_str().to_string())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate tool names");
        assert!(names.len() >= 13);
    }

    #[test]
    fn whitelist_keeps_only_listed() {
        let tools = default_tools(ctx(), None);
        let kept = filter_by_name(
            tools,
            FilterMode::Whitelist,
            &["read".into(), "shell".into()],
        );
        let names: Vec<String> = kept
            .iter()
            .map(|t| t.definition().name.as_str().to_string())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"read".into()));
        assert!(names.contains(&"shell".into()));
    }

    #[test]
    fn blacklist_drops_listed() {
        let tools = default_tools(ctx(), None);
        let total = tools.len();
        let kept = filter_by_name(tools, FilterMode::Blacklist, &["shell".into()]);
        assert_eq!(kept.len(), total - 1);
        assert!(
            !kept
                .iter()
                .any(|t| t.definition().name.as_str() == "shell")
        );
    }

    #[test]
    fn empty_whitelist_keeps_nothing() {
        let kept = filter_by_name(default_tools(ctx(), None), FilterMode::Whitelist, &[]);
        assert!(kept.is_empty());
    }

    #[test]
    fn empty_blacklist_keeps_everything() {
        let total = default_tools(ctx(), None).len();
        let kept = filter_by_name(default_tools(ctx(), None), FilterMode::Blacklist, &[]);
        assert_eq!(kept.len(), total);
    }

    #[test]
    fn unknown_names_in_list_are_ignored() {
        let total = default_tools(ctx(), None).len();
        let kept = filter_by_name(
            default_tools(ctx(), None),
            FilterMode::Blacklist,
            &["does_not_exist".into()],
        );
        assert_eq!(kept.len(), total);
    }
}
