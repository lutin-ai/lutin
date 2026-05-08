//! `%placeholder%` substitution for persona system prompts.
//!
//! Mirrors v1's `engine/src/prompt.rs` so personas authored against
//! the v1 syntax port across without edits. Resolution is single-pass
//! per token: replacements are not re-scanned, so `%file:%` includes
//! cannot smuggle further placeholders.
//!
//! `%file:relative/path%` reads from the workspace root passed in
//! `PromptContext::cwd` (i.e. the sandbox root). The resolved path is
//! canonicalised and rejected if it escapes that root, so traversal
//! tokens like `%file:../etc/passwd%` produce an `[Error …]` marker
//! rather than reading outside the workspace.
use std::path::Path;

use chrono::{Datelike, Weekday};

use lutin_entities::Persona;

/// Persona metadata exposed to placeholders. Built from a [`Persona`]
/// inside [`resolve`]; callers don't construct this directly.
struct PersonaCtx<'a> {
    name: &'a str,
    display_name: &'a str,
    description: &'a str,
}

/// Optional sub-agent metadata. Set by sub-agent workflows; `None`
/// for top-level chats so `%agent:name%` / `%agent:status%` collapse
/// to empty strings.
#[derive(Debug, Clone, Default)]
pub struct AgentMeta {
    pub name: String,
    pub status: String,
}

/// One row in the `%agents:attached%` markdown list.
#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub name: String,
    pub status: String,
}

/// One row in the `%personas:all%` markdown list.
#[derive(Debug, Clone)]
pub struct PersonaEntry {
    pub name: String,
    pub display_name: String,
    pub description: String,
}

/// One entry in the `%skills%` comma-list.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub name: String,
    pub display_name: String,
    pub description: String,
}

/// Workflow-supplied chat state. The SDK fills persona / cwd from
/// build inputs; everything else is workflow-specific so it lives
/// here. Empty defaults mean every field is optional — placeholders
/// for unset state collapse to empty strings.
#[derive(Debug, Clone, Default)]
pub struct PromptExtras {
    pub message_count: usize,
    pub user_message: Option<String>,
    pub latest_response: Option<String>,
    pub variables: Vec<(String, String)>,
    pub agent: Option<AgentMeta>,
    pub attached_agents: Vec<AgentEntry>,
    pub skills: Vec<SkillEntry>,
    pub personas: Vec<PersonaEntry>,
    pub chat_kind: String,
    pub chat_title: Option<String>,
}

/// Inputs for [`resolve`]. Built by the SDK from a [`Persona`], a
/// sandbox root, and the workflow's [`PromptExtras`].
pub struct PromptContext<'a> {
    persona: Option<PersonaCtx<'a>>,
    cwd: &'a str,
    extras: &'a PromptExtras,
}

impl<'a> PromptContext<'a> {
    /// Standard construction: persona metadata is borrowed from the
    /// persona TOML and `cwd` from the sandbox root. Pass an empty
    /// `PromptExtras` if the workflow has no chat state to expose.
    pub fn from_parts(persona: &'a Persona, cwd: &'a str, extras: &'a PromptExtras) -> Self {
        Self {
            persona: Some(PersonaCtx {
                name: &persona.name,
                display_name: &persona.display_name,
                description: &persona.description,
            }),
            cwd,
            extras,
        }
    }
}

/// Resolve every `%placeholder%` in `template` using `ctx`.
///
/// Supported tokens (unset fields collapse to empty strings; unknown
/// tokens are left untouched so personas that pre-date a placeholder
/// keep working):
///
/// - `%time%` `%date%` `%datetime%` `%day%` — local clock
/// - `%cwd%` — workspace root (sandbox)
/// - `%message_count%` `%user_message%` `%latest_response%`
/// - `%var:name%` — workflow-supplied variables
/// - `%file:rel/path%` — file under `cwd`, traversal-guarded
/// - `%persona:name%` / `:display_name%` / `:description%`
/// - `%agent:name%` / `:status%` — sub-agent metadata
/// - `%skills%` — comma-joined skill display names
/// - `%agents:attached%` — markdown list of `- name (status)`
/// - `%personas:all%` — markdown list of `- name — description`
/// - `%chat:kind%` `%chat:title%`
pub fn resolve(template: &str, ctx: &PromptContext<'_>) -> String {
    let local: chrono::DateTime<chrono::Local> = chrono::Utc::now().into();

    let mut out = template.to_string();

    out = out.replace("%time%", &local.format("%I:%M %p").to_string());
    out = out.replace("%date%", &local.format("%Y-%m-%d").to_string());
    out = out.replace("%datetime%", &local.format("%Y-%m-%d %I:%M %p").to_string());
    out = out.replace("%day%", weekday_name(local.weekday()));
    out = out.replace("%cwd%", ctx.cwd);

    let extras = ctx.extras;
    out = out.replace("%message_count%", &extras.message_count.to_string());
    out = out.replace(
        "%user_message%",
        extras.user_message.as_deref().unwrap_or(""),
    );
    out = out.replace(
        "%latest_response%",
        extras.latest_response.as_deref().unwrap_or(""),
    );

    if let Some(p) = &ctx.persona {
        out = out.replace("%persona:name%", p.name);
        out = out.replace("%persona:display_name%", p.display_name);
        out = out.replace("%persona:description%", p.description);
    } else {
        out = out.replace("%persona:name%", "");
        out = out.replace("%persona:display_name%", "");
        out = out.replace("%persona:description%", "");
    }

    if let Some(a) = &extras.agent {
        out = out.replace("%agent:name%", &a.name);
        out = out.replace("%agent:status%", &a.status);
    } else {
        out = out.replace("%agent:name%", "");
        out = out.replace("%agent:status%", "");
    }

    let skill_list: String = extras
        .skills
        .iter()
        .map(|s| s.display_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    out = out.replace("%skills%", &skill_list);

    out = out.replace("%agents:attached%", &format_agent_list(&extras.attached_agents));
    out = out.replace("%personas:all%", &format_persona_list(&extras.personas));

    out = out.replace("%chat:kind%", &extras.chat_kind);
    out = out.replace("%chat:title%", extras.chat_title.as_deref().unwrap_or(""));

    for (key, value) in &extras.variables {
        let token = format!("%var:{key}%");
        out = out.replace(&token, value);
    }

    // File includes last so inserted text isn't re-scanned for tokens.
    resolve_file_placeholders(&mut out, Path::new(ctx.cwd));

    out
}

fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

fn format_agent_list(agents: &[AgentEntry]) -> String {
    agents
        .iter()
        .map(|a| format!("- {} ({})", a.name, a.status))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_persona_list(personas: &[PersonaEntry]) -> String {
    personas
        .iter()
        .map(|p| {
            if p.description.is_empty() {
                format!("- `{}` ({})", p.name, p.display_name)
            } else {
                format!("- `{}` — {}", p.name, p.description)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn resolve_file_placeholders(text: &mut String, base: &Path) {
    let prefix = "%file:";
    // Track scan position so a `[Error …]` replacement (which contains
    // no `%file:` token) doesn't trigger re-scanning earlier text.
    let mut search_from = 0;
    loop {
        let Some(rel_start) = text[search_from..].find(prefix) else {
            break;
        };
        let start = search_from + rel_start;
        let after_prefix = start + prefix.len();
        let Some(rel_end) = text[after_prefix..].find('%') else {
            break;
        };
        let end = after_prefix + rel_end;
        let relative: String = text[after_prefix..end].to_string();
        let replacement = read_file_safely(base, Path::new(&relative));
        text.replace_range(start..=end, &replacement);
        // Advance past the inserted replacement.
        search_from = start + replacement.len();
    }
}

fn read_file_safely(base: &Path, relative: &Path) -> String {
    if relative.is_absolute() {
        return format!(
            "[Error: file '{}' must be relative to the workspace]",
            relative.display()
        );
    }
    let full = base.join(relative);
    let canonical = match full.canonicalize() {
        Ok(p) => p,
        Err(e) => return format!("[Error reading file '{}': {e}]", relative.display()),
    };
    let canonical_base = match base.canonicalize() {
        Ok(p) => p,
        Err(e) => return format!("[Error: workspace root not accessible: {e}]"),
    };
    if !canonical.starts_with(&canonical_base) {
        return format!(
            "[Error: file '{}' is outside the workspace]",
            relative.display()
        );
    }
    match std::fs::read_to_string(&canonical) {
        Ok(contents) => contents,
        Err(e) => format!("[Error reading file '{}': {e}]", relative.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona_for_tests() -> Persona {
        Persona {
            name: "tester".into(),
            display_name: "Tester".into(),
            description: "Test persona".into(),
            icon: String::new(),
            icon_color: [0, 0, 0],
            system_prompt: String::new(),
            default_skills: Vec::new(),
            model: None,
            provider: None,
            temperature: None,
            presence_penalty: None,
            context_limit: None,
            sliding_window_messages: None,
            compaction_threshold_messages: None,
            compaction_keep_recent_user_turns: None,
            thinking_enabled: false,
            reasoning_effort: Default::default(),
            reasoning_max_tokens: None,
            tool_filter_mode: lutin_entities::ToolFilterMode::Blacklist,
            tool_filter_list: Vec::new(),
            category: String::new(),
        }
    }

    #[test]
    fn replaces_persona_and_chat_state() {
        let persona = persona_for_tests();
        let extras = PromptExtras {
            message_count: 7,
            user_message: Some("hi".into()),
            latest_response: Some("hello".into()),
            chat_kind: "main".into(),
            chat_title: Some("Test".into()),
            ..Default::default()
        };
        let ctx = PromptContext::from_parts(&persona, "/tmp", &extras);
        let r = resolve(
            "%persona:display_name% n=%message_count% u=%user_message% a=%latest_response% k=%chat:kind% t=%chat:title%",
            &ctx,
        );
        assert_eq!(r, "Tester n=7 u=hi a=hello k=main t=Test");
    }

    #[test]
    fn replaces_lists() {
        let persona = persona_for_tests();
        let extras = PromptExtras {
            attached_agents: vec![AgentEntry {
                name: "a1".into(),
                status: "running".into(),
            }],
            personas: vec![
                PersonaEntry {
                    name: "p1".into(),
                    display_name: "P1".into(),
                    description: "Does p1 stuff".into(),
                },
                PersonaEntry {
                    name: "p2".into(),
                    display_name: "P2".into(),
                    description: String::new(),
                },
            ],
            skills: vec![SkillEntry {
                name: "web".into(),
                display_name: "Web".into(),
                description: String::new(),
            }],
            ..Default::default()
        };
        let ctx = PromptContext::from_parts(&persona, "/tmp", &extras);
        let r = resolve("%agents:attached%|%personas:all%|%skills%", &ctx);
        assert_eq!(
            r,
            "- a1 (running)|- `p1` — Does p1 stuff\n- `p2` (P2)|Web"
        );
    }

    #[test]
    fn unknown_left_untouched() {
        let persona = persona_for_tests();
        let extras = PromptExtras::default();
        let ctx = PromptContext::from_parts(&persona, "/tmp", &extras);
        assert_eq!(resolve("hi %unknown%", &ctx), "hi %unknown%");
    }

    #[test]
    fn variables_substitute() {
        let persona = persona_for_tests();
        let extras = PromptExtras {
            variables: vec![("name".into(), "Alice".into())],
            ..Default::default()
        };
        let ctx = PromptContext::from_parts(&persona, "/tmp", &extras);
        assert_eq!(resolve("hi %var:name%", &ctx), "hi Alice");
    }

    #[test]
    fn file_traversal_blocked() {
        let persona = persona_for_tests();
        let extras = PromptExtras::default();
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().to_string_lossy().to_string();
        let ctx = PromptContext::from_parts(&persona, &cwd, &extras);
        let r = resolve("%file:../../etc/passwd%", &ctx);
        assert!(r.starts_with("[Error"), "got: {r}");
    }

    #[test]
    fn file_reads_workspace_relative() {
        let persona = persona_for_tests();
        let extras = PromptExtras::default();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("note.md"), "hello world").unwrap();
        let cwd = tmp.path().to_string_lossy().to_string();
        let ctx = PromptContext::from_parts(&persona, &cwd, &extras);
        assert_eq!(resolve("%file:note.md%", &ctx), "hello world");
    }
}
