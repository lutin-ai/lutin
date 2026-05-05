//! Skill DTO.
//!
//! Stored at `.lutin/skills/<name>/skill.toml`. The directory name is
//! the canonical identifier; the body never restates it. The directory
//! layout (rather than a flat `<name>.toml` file) leaves room for future
//! per-skill assets without a schema change — e.g. attached prompts or
//! tool scripts a later milestone may add.

use lutin_storage::{Resolver, Scope};
use serde::{Deserialize, Serialize};

use crate::{read_toml, read_toml_if_exists, EntityError, EntityLocation};

lutin_ids::identifier!(SkillName, SkillNameError, 64, "skill name");

const DIR: &str = "skills";
const FILE: &str = "skill.toml";

/// Skill file body. The name comes from the directory and is attached
/// post-deserialize.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    /// Directory name; populated by the loader, not the file.
    #[serde(skip)]
    pub name: String,

    pub display_name: String,
    pub description: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Text injected into the system prompt when this skill is loaded.
    #[serde(default)]
    pub prompt_inject: String,

    /// Tool names enabled by this skill.
    #[serde(default)]
    pub tools_add: Vec<String>,
    /// Tool names disabled by this skill (overrides `tools_add` from
    /// other loaded skills with lower precedence).
    #[serde(default)]
    pub tools_remove: Vec<String>,

    /// Output format constraint passed to the model (`"json"`, schema
    /// string, etc.). Empty / absent = no constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,

    /// Environment variables this skill expects at runtime. Engine UI
    /// can prompt for these; resolution is the runtime's concern.
    #[serde(default)]
    pub env_vars: Vec<SkillEnvVar>,

    #[serde(default)]
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillEnvVar {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

impl Skill {
    /// Load one skill by name. Project tier wins.
    pub fn load(resolver: &Resolver, name: &str) -> Result<Self, EntityError> {
        let _ = SkillName::parse(name).map_err(|e| EntityError::InvalidName(e.to_string()))?;
        let rel = std::path::PathBuf::from(DIR).join(name).join(FILE);
        let Some((_scope, path)) = resolver.find_file(&rel) else {
            return Err(EntityError::NotFound {
                kind: "skill",
                name: name.into(),
            });
        };
        let mut skill: Skill = read_toml(&path)?;
        skill.name = name.into();
        Ok(skill)
    }

    /// List all skills across both tiers; project wins on name clash.
    pub fn list(resolver: &Resolver) -> Result<Vec<Self>, EntityError> {
        let mut out: Vec<(Scope, Skill)> = Vec::new();
        for loc in entity_locations(resolver)? {
            let Ok(_) = SkillName::parse(&loc.name) else { continue };
            let manifest = loc.path.join(FILE);
            let Some(mut skill) = read_toml_if_exists::<Skill>(&manifest)? else {
                continue;
            };
            skill.name = loc.name;
            out.push((loc.scope, skill));
        }
        out.sort_by(|a, b| a.1.name.cmp(&b.1.name).then(b.0.cmp(&a.0)));
        out.dedup_by(|a, b| a.1.name == b.1.name);
        Ok(out.into_iter().map(|(_, s)| s).collect())
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
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
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

    #[test]
    fn load_minimal() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        write(
            &global.join("skills/debugger/skill.toml"),
            r#"
display_name = "Debugger"
description = "Helps debug stuff"
prompt_inject = "Think carefully about errors."
tools_add = ["shell"]
"#,
        );
        let resolver = Resolver::new(&global, None::<&Path>);
        let s = Skill::load(&resolver, "debugger").unwrap();
        assert_eq!(s.name, "debugger");
        assert_eq!(s.tools_add, vec!["shell".to_string()]);
    }

    #[test]
    fn list_skips_dirs_without_manifest() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        write(
            &global.join("skills/real/skill.toml"),
            r#"
display_name = "R"
description = "x"
"#,
        );
        std::fs::create_dir_all(global.join("skills/scratch")).unwrap();
        let resolver = Resolver::new(&global, None::<&Path>);
        let v = Skill::list(&resolver).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "real");
    }

    #[test]
    fn env_vars_round_trip() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        write(
            &global.join("skills/web/skill.toml"),
            r#"
display_name = "Web"
description = "x"

[[env_vars]]
name = "BRAVE_API_KEY"
description = "Brave Search key"
required = true
"#,
        );
        let resolver = Resolver::new(&global, None::<&Path>);
        let s = Skill::load(&resolver, "web").unwrap();
        assert_eq!(s.env_vars.len(), 1);
        assert!(s.env_vars[0].required);
    }
}
