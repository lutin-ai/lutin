//! Two-tier filesystem resolver for personas, workflows, settings, etc.
//!
//! Looks at the project tier first then global. For "list" calls,
//! both tiers are concatenated and tagged with their [`Scope`].

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Global,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEntity {
    pub scope: Scope,
    pub name: String,
    pub path: PathBuf,
}

/// Roots are paths to the `.lutin/` directory at each tier.
pub struct Resolver {
    pub global: PathBuf,
    pub project: Option<PathBuf>,
}

impl Resolver {
    pub fn new(global: impl Into<PathBuf>, project: Option<impl Into<PathBuf>>) -> Self {
        Self {
            global: global.into(),
            project: project.map(Into::into),
        }
    }

    /// Highest-precedence file for `relative` (project wins). For
    /// settings-style merges call [`Self::all_files`] and merge yourself.
    pub fn find_file(&self, relative: &Path) -> Option<(Scope, PathBuf)> {
        if let Some(project) = &self.project {
            let p = project.join(relative);
            if p.is_file() {
                return Some((Scope::Project, p));
            }
        }
        let p = self.global.join(relative);
        if p.is_file() {
            return Some((Scope::Global, p));
        }
        None
    }

    /// Both tiers' files for `relative`, project last (so it overrides
    /// when callers fold in order).
    pub fn all_files(&self, relative: &Path) -> Vec<(Scope, PathBuf)> {
        let mut out = Vec::new();
        let g = self.global.join(relative);
        if g.is_file() {
            out.push((Scope::Global, g));
        }
        if let Some(project) = &self.project {
            let p = project.join(relative);
            if p.is_file() {
                out.push((Scope::Project, p));
            }
        }
        out
    }

    /// All entities in `kind/` across both tiers. Same name in both
    /// tiers yields two entries (caller decides; per spec, append both
    /// and disambiguate by scope).
    pub fn list_entities(&self, kind: &str) -> Result<Vec<ResolvedEntity>> {
        let mut out = Vec::new();
        let global_dir = self.global.join(kind);
        if global_dir.is_dir() {
            for entry in fs::read_dir(&global_dir)? {
                let entry = entry?;
                let path = entry.path();
                let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                out.push(ResolvedEntity {
                    scope: Scope::Global,
                    name: name.to_string(),
                    path,
                });
            }
        }
        if let Some(project) = &self.project {
            let project_dir = project.join(kind);
            if project_dir.is_dir() {
                for entry in fs::read_dir(&project_dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    out.push(ResolvedEntity {
                        scope: Scope::Project,
                        name: name.to_string(),
                        path,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Group `list_entities` output by name, keeping the project entry
    /// when both tiers have the same name (single highest-precedence
    /// pick per name).
    pub fn list_entities_unique(&self, kind: &str) -> Result<Vec<ResolvedEntity>> {
        let mut all = self.list_entities(kind)?;
        // Sort by name; for same name, put Project first so dedup_by keeps it
        // (dedup_by retains the first occurrence in each run of duplicates).
        all.sort_by(|a, b| a.name.cmp(&b.name).then(b.scope.cmp(&a.scope)));
        all.dedup_by(|a, b| a.name == b.name);
        Ok(all)
    }
}
