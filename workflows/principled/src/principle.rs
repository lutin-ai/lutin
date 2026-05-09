//! Principle DTO + loader.
//!
//! A principle is a single rule evaluated by one reviewer. Stored at
//! `.lutin/principles/<name>.toml` (per-project) or
//! `<global>/principles/<name>.toml`. The filename stem is the
//! canonical id referenced by `SessionState.principles`; the file body
//! never restates it.

use std::path::{Path, PathBuf};

use lutin_storage::{Resolver, Scope};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DIR: &str = "principles";

/// Workflow-level principle order, baked in at compile time from
/// `workflows/principled/principles.toml`. Least-important first —
/// later entries override earlier ones in the review loop's
/// adjustment logic. Forking this workflow into a domain variant
/// (coder, researcher, reviewer) means copying the workflow
/// directory and editing the TOML; the engine code is shared.
///
/// We parse on first use rather than at compile time because `toml`
/// has no const-fn parser; the `OnceLock` keeps it to one parse for
/// the process lifetime.
pub static WORKFLOW_ORDER: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(|| {
        const SRC: &str = include_str!("../principles.toml");
        #[derive(serde::Deserialize)]
        struct Doc {
            order: Vec<String>,
        }
        let doc: Doc =
            toml::from_str(SRC).expect("workflows/principled/principles.toml: parse failed");
        // Leak each name to get a `&'static str` — the list is
        // process-lifetime constant, so the leak is bounded by the
        // file's length and happens exactly once.
        doc.order
            .into_iter()
            .map(|s| Box::leak(s.into_boxed_str()) as &'static str)
            .collect()
    });

#[derive(Debug, Error)]
pub enum PrincipleError {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("principle not found: {0}")]
    NotFound(String),
}

/// On-disk shape. The reviewer LLM receives `title` + `description` as
/// the principle's full text. Every other field shapes how the review
/// loop dispatches and consumes verdicts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Principle {
    /// Filename stem; populated by the loader, not the file.
    #[serde(skip)]
    pub name: String,

    /// One-line label shown in the sidebar.
    pub title: String,
    /// Long-form rule the reviewer reads. Examples, edge cases, etc.
    pub description: String,

    /// Persona stem (file under `personas/`) used to drive the reviewer
    /// LLM. Determines model + system prompt frame.
    pub persona: String,

    /// Tool names that trigger this principle. A tool call only goes
    /// through review if at least one principle has its name in here.
    pub applies_to: Vec<String>,

    /// Which extra context the reviewer sees. `tool_call` is implicit;
    /// list the rest explicitly.
    #[serde(default)]
    pub context: Vec<ContextItem>,

    /// Per-step retry budget. When exceeded, `on_max_retries` decides.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    #[serde(default)]
    pub on_max_retries: OnMaxRetries,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextItem {
    /// The proposed tool call (name + args). Always implicitly included
    /// even if not listed; allowed here so configs can be explicit.
    ToolCall,
    /// Computed result of the tool *without* execution. Currently only
    /// meaningful for Edit/Write (post-edit file content); other tools
    /// have no pre-exec artifact and the reviewer just won't see one.
    ToolArtifact,
    /// Main user-agent conversation so far.
    Chat,
    /// Accepted prior step frames (their tool calls + results).
    PriorSteps,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnMaxRetries {
    /// Mark the principle skipped for this step; loop continues with
    /// whatever other failures remain.
    #[default]
    Continue,
    /// Pause the session and surface the deadlock to the user.
    AskUser,
}

fn default_max_retries() -> u32 {
    3
}

impl Principle {
    /// Load one principle by name. Project tier wins over global.
    pub fn load(resolver: &Resolver, name: &str) -> Result<Self, PrincipleError> {
        let rel = PathBuf::from(DIR).join(format!("{name}.toml"));
        let Some((_scope, path)) = resolver.find_file(&rel) else {
            return Err(PrincipleError::NotFound(name.into()));
        };
        let mut p: Principle = read_toml(&path)?;
        p.name = name.into();
        Ok(p)
    }

    /// List all principles across both tiers; project wins on name
    /// clash. Files that fail to parse are surfaced as errors rather
    /// than silently skipped — a bad principle file is a config bug
    /// the user wants to know about.
    pub fn list(resolver: &Resolver) -> Result<Vec<Self>, PrincipleError> {
        let mut out: Vec<(Scope, Principle)> = Vec::new();
        for (scope, root) in [(Scope::Global, &resolver.global)]
            .into_iter()
            .chain(resolver.project.as_ref().map(|p| (Scope::Project, p)))
        {
            let dir = root.join(DIR);
            if !dir.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&dir).map_err(|source| PrincipleError::Io {
                path: dir.display().to_string(),
                source,
            })? {
                let entry = entry.map_err(|source| PrincipleError::Io {
                    path: dir.display().to_string(),
                    source,
                })?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let mut p: Principle = read_toml(&path)?;
                p.name = name.into();
                out.push((scope, p));
            }
        }
        // Project wins on name clash.
        out.sort_by(|a, b| a.1.name.cmp(&b.1.name).then(b.0.cmp(&a.0)));
        out.dedup_by(|a, b| a.1.name == b.1.name);
        Ok(out.into_iter().map(|(_, p)| p).collect())
    }
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, PrincipleError> {
    let text = std::fs::read_to_string(path).map_err(|source| PrincipleError::Io {
        path: path.display().to_string(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| PrincipleError::Parse {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    const MIN_BODY: &str = r#"
title = "Guard clauses over nesting"
description = "Avoid pyramids. Prefer early returns."
persona = "reviewer-rust"
applies_to = ["edit", "write"]
"#;

    #[test]
    fn workflow_order_parses_and_is_nonempty() {
        // Touch the LazyLock to force the `include_str!` parse and
        // the `expect` in the loader. A panic here points at a
        // malformed `principles.toml`.
        assert!(!WORKFLOW_ORDER.is_empty(), "WORKFLOW_ORDER unexpectedly empty");
        // Names should be unique — duplicates would cause double-fan-out.
        let mut seen = std::collections::HashSet::new();
        for name in WORKFLOW_ORDER.iter() {
            assert!(seen.insert(*name), "duplicate principle in order: {name}");
        }
    }

    #[test]
    fn load_minimal() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        write(&global.join("principles/guard-clauses.toml"), MIN_BODY);
        let resolver = Resolver::new(&global, None::<&Path>);

        let p = Principle::load(&resolver, "guard-clauses").unwrap();
        assert_eq!(p.name, "guard-clauses");
        assert_eq!(p.title, "Guard clauses over nesting");
        assert_eq!(p.applies_to, vec!["edit", "write"]);
        assert!(p.context.is_empty());
        assert_eq!(p.max_retries, 3);
        assert_eq!(p.on_max_retries, OnMaxRetries::Continue);
    }

    #[test]
    fn project_overrides_global() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        let project = dir.path().join("project");
        write(&global.join("principles/guard-clauses.toml"), MIN_BODY);
        write(
            &project.join("principles/guard-clauses.toml"),
            r#"
title = "Project guard clauses"
description = "x"
persona = "reviewer-rust"
applies_to = ["edit"]
max_retries = 5
on_max_retries = "ask_user"
"#,
        );
        let resolver = Resolver::new(&global, Some(&project));
        let p = Principle::load(&resolver, "guard-clauses").unwrap();
        assert_eq!(p.title, "Project guard clauses");
        assert_eq!(p.max_retries, 5);
        assert_eq!(p.on_max_retries, OnMaxRetries::AskUser);
    }

    #[test]
    fn list_dedupes_by_name() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        let project = dir.path().join("project");
        write(&global.join("principles/guard-clauses.toml"), MIN_BODY);
        write(&global.join("principles/use-vec.toml"), MIN_BODY);
        write(
            &project.join("principles/guard-clauses.toml"),
            r#"
title = "Project"
description = "x"
persona = "reviewer-rust"
applies_to = ["edit"]
"#,
        );
        let resolver = Resolver::new(&global, Some(&project));
        let v = Principle::list(&resolver).unwrap();
        assert_eq!(v.len(), 2);
        let g = v.iter().find(|p| p.name == "guard-clauses").unwrap();
        assert_eq!(g.title, "Project");
    }

    #[test]
    fn missing_returns_not_found() {
        let dir = tempdir().unwrap();
        let resolver = Resolver::new(dir.path(), None::<&Path>);
        match Principle::load(&resolver, "ghost") {
            Err(PrincipleError::NotFound(n)) => assert_eq!(n, "ghost"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn context_items_parse() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        write(
            &global.join("principles/p.toml"),
            r#"
title = "P"
description = "x"
persona = "reviewer"
applies_to = ["edit"]
context = ["tool_call", "tool_artifact", "file", "chat", "prior_steps"]
"#,
        );
        let resolver = Resolver::new(&global, None::<&Path>);
        // `file` isn't a defined ContextItem variant — this should fail.
        match Principle::load(&resolver, "p") {
            Err(PrincipleError::Parse { .. }) => (),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }
}
