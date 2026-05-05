//! Persona DTO.
//!
//! Stored at `.lutin/personas/<name>.toml`. The filename stem is the
//! canonical identifier; the file body never restates it. References
//! to skills are by their on-disk name (string), not UUID.

use lutin_storage::{Resolver, Scope};
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

fn deserialize_non_empty_opt<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    Ok(raw.filter(|s| !s.is_empty()))
}

use crate::{read_toml, read_toml_if_exists, EntityError, EntityLocation};

lutin_ids::identifier!(PersonaName, PersonaNameError, 64, "persona name");

const DIR: &str = "personas";

/// Persona file body. The name comes from the filename and is attached
/// post-deserialize when [`Persona::load`] / [`Persona::list`] return.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Persona {
    /// Filename stem; populated by the loader, not the file.
    #[serde(skip)]
    pub name: String,

    pub display_name: String,
    pub description: String,

    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default = "default_icon_color")]
    pub icon_color: [u8; 3],

    pub system_prompt: String,

    /// Skill names auto-loaded with this persona.
    #[serde(default)]
    pub default_skills: Vec<String>,

    /// LLM model id (provider-specific). `None` = let the workflow /
    /// settings fall through to whatever default exists. An empty string
    /// in the on-disk file collapses to `None` at the boundary so callers
    /// never see a blank-but-present model.
    #[serde(default, deserialize_with = "deserialize_non_empty_opt")]
    pub model: Option<String>,
    /// Provider name (matches a `[[providers]]` entry in settings.toml).
    /// `None` = unset; empty strings on disk collapse to `None`.
    #[serde(default, deserialize_with = "deserialize_non_empty_opt")]
    pub provider: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Maximum context window tokens. `None` = no compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<u32>,

    /// Enable extended thinking / reasoning mode.
    #[serde(default)]
    pub thinking_enabled: bool,

    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,

    /// Hard cap on reasoning tokens. `None` = let the provider decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_max_tokens: Option<u32>,

    #[serde(default)]
    pub tool_filter_mode: ToolFilterMode,
    #[serde(default)]
    pub tool_filter_list: Vec<String>,

    #[serde(default)]
    pub category: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    #[default]
    Medium,
    High,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolFilterMode {
    /// Listed tools are the *only* ones the persona may use.
    Whitelist,
    /// Listed tools are disabled; everything else is available.
    #[default]
    Blacklist,
}

fn default_icon() -> String {
    "\u{f0d3}".into()
}
fn default_icon_color() -> [u8; 3] {
    [0x3b, 0x82, 0xf6]
}

impl Persona {
    /// Load one persona by name. Project tier wins over global. Returns
    /// `EntityError::NotFound` if neither tier has it.
    pub fn load(resolver: &Resolver, name: &str) -> Result<Self, EntityError> {
        let _name = PersonaName::parse(name).map_err(|e| EntityError::InvalidName(e.to_string()))?;
        let rel = std::path::PathBuf::from(DIR).join(format!("{name}.toml"));
        let Some((_scope, path)) = resolver.find_file(&rel) else {
            return Err(EntityError::NotFound {
                kind: "persona",
                name: name.into(),
            });
        };
        let mut persona: Persona = read_toml(&path)?;
        persona.name = name.into();
        Ok(persona)
    }

    /// List all personas across both tiers; project wins on name clash.
    /// Names that don't parse as [`PersonaName`] are skipped silently —
    /// they're stray files (e.g. `.swp`), not personas.
    pub fn list(resolver: &Resolver) -> Result<Vec<Self>, EntityError> {
        let mut out: Vec<(Scope, Persona)> = Vec::new();
        // Walk both tiers; collect raw scope+persona pairs first.
        for loc in entity_locations(resolver)? {
            let Ok(_) = PersonaName::parse(&loc.name) else { continue };
            let Some(mut persona) = read_toml_if_exists::<Persona>(&loc.path)? else {
                continue;
            };
            persona.name = loc.name;
            out.push((loc.scope, persona));
        }
        // Project wins: sort by (name asc, scope desc) so Project sorts
        // before Global, then dedup_by-name keeps the first (= Project).
        out.sort_by(|a, b| a.1.name.cmp(&b.1.name).then(b.0.cmp(&a.0)));
        out.dedup_by(|a, b| a.1.name == b.1.name);
        Ok(out.into_iter().map(|(_, p)| p).collect())
    }
}

fn entity_locations(resolver: &Resolver) -> Result<Vec<EntityLocation>, EntityError> {
    let mut out = Vec::new();
    for (scope, root) in [(Scope::Global, &resolver.global)]
        .into_iter()
        .chain(resolver.project.as_ref().map(|p| (Scope::Project, p)))
    {
        let dir = root.join(DIR);
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&dir).map_err(|source| EntityError::Io {
            path: dir.display().to_string(),
            source,
        })? {
            let entry = entry.map_err(|source| EntityError::Io {
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
            out.push(EntityLocation {
                scope,
                name: name.into(),
                path,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    const MIN_BODY: &str = r#"
display_name = "Coder"
description = "Pair-programming assistant"
system_prompt = "be helpful"
"#;

    #[test]
    fn load_minimal() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        write(&global.join("personas/coder.toml"), MIN_BODY);
        let resolver = Resolver::new(&global, None::<&Path>);

        let p = Persona::load(&resolver, "coder").unwrap();
        assert_eq!(p.name, "coder");
        assert_eq!(p.display_name, "Coder");
        assert_eq!(p.icon, "\u{f0d3}");
        assert_eq!(p.tool_filter_mode, ToolFilterMode::Blacklist);
        assert!(p.default_skills.is_empty());
    }

    #[test]
    fn project_overrides_global() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        let project = dir.path().join("project");
        write(&global.join("personas/coder.toml"), MIN_BODY);
        write(
            &project.join("personas/coder.toml"),
            r#"
display_name = "Project Coder"
description = "Custom"
system_prompt = "..."
"#,
        );
        let resolver = Resolver::new(&global, Some(&project));
        let p = Persona::load(&resolver, "coder").unwrap();
        assert_eq!(p.display_name, "Project Coder");
    }

    #[test]
    fn list_dedupes_by_name() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        let project = dir.path().join("project");
        write(&global.join("personas/coder.toml"), MIN_BODY);
        write(&global.join("personas/writer.toml"), MIN_BODY);
        write(
            &project.join("personas/coder.toml"),
            r#"
display_name = "Project Coder"
description = "x"
system_prompt = "x"
"#,
        );
        let resolver = Resolver::new(&global, Some(&project));
        let v = Persona::list(&resolver).unwrap();
        assert_eq!(v.len(), 2);
        let coder = v.iter().find(|p| p.name == "coder").unwrap();
        assert_eq!(coder.display_name, "Project Coder");
    }

    #[test]
    fn missing_returns_not_found() {
        let dir = tempdir().unwrap();
        let resolver = Resolver::new(dir.path(), None::<&Path>);
        match Persona::load(&resolver, "ghost") {
            Err(EntityError::NotFound { name, .. }) => assert_eq!(name, "ghost"),
            other => panic!("{other:?}"),
        }
    }
}
