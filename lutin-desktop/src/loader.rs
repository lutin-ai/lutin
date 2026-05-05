//! Workflow cdylib loader.
//!
//! Resolves the `.so` path for a `(slug, workflow_id)` pair, dlopens
//! it, and calls the `create_workflow` factory exported by every
//! workflow cdylib (see `lutin-workflow-ui::CREATE_WORKFLOW_SYMBOL`).
//!
//! The returned `Box<dyn Workflow>` borrows code (vtables, statics)
//! from the `Library` it came from, so the two are kept together in
//! `WorkflowLibrary` and the field-drop order is load-bearing: the
//! workflow drops first, then the library closes.
//!
//! Path layout (post-D): the control-panel seeds workflows into
//! `<global_config_dir>/workflows/<id>/`, and the project tier
//! cargo-builds them in place so the cdylib lands at
//! `<global_config_dir>/workflows/<id>/target/<profile>/lib<id>.so`.
//! Both desktop and project tier therefore agree on layout — the
//! desktop reads the same `LUTIN_GLOBAL_CONFIG_DIR` /
//! `LUTIN_WORKFLOW_PROFILE` env vars the CP/project use.
//!
//! The cache key is `(slug, workflow)` rather than just `workflow` so
//! that, in a future where workflows can be project-overridden,
//! per-slug overrides Just Work — at the cost of dlopening the same
//! `.so` once per slug today (cheap; same file path on Linux returns
//! the same library handle).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::{Library, Symbol};
use lutin_ids::{Slug, WorkflowId};
use lutin_workflow_ui::{CREATE_WORKFLOW_SYMBOL, CreateWorkflowFn, Workflow};

/// One loaded workflow cdylib + the `Workflow` factory output it
/// produced. Field-drop order matters: `workflow` is dropped first
/// (its vtable lives in `_lib`), then `_lib` closes.
pub struct WorkflowLibrary {
    workflow: Box<dyn Workflow>,
    _lib: Library,
}

impl WorkflowLibrary {
    pub fn workflow(&self) -> &dyn Workflow {
        &*self.workflow
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(
        "dlopen {path}: {source} (start a session in this project to trigger the cargo build, or run `cargo build --release` in {path:?} if the .so is missing)"
    )]
    Dlopen {
        path: PathBuf,
        #[source]
        source: libloading::Error,
    },
    #[error("workflow .so missing `{symbol}` symbol: {source}")]
    MissingSymbol {
        symbol: String,
        #[source]
        source: libloading::Error,
    },
}

/// Where the cdylib for a workflow lives, given a workflows root and
/// cargo profile dir. Slug isn't part of the path — workflows are
/// global-tier in the seeded layout — but is kept in the cache key
/// for forward-compat with future per-project overrides.
pub fn workflow_so_path(workflows_root: &Path, profile_dir: &str, workflow: &WorkflowId) -> PathBuf {
    let id = workflow.as_str();
    workflows_root
        .join(id)
        .join("target")
        .join(profile_dir)
        .join(format!("lib{id}.so"))
}

/// Cache of loaded workflow libraries, keyed by `(slug, workflow_id)`.
/// `Arc<WorkflowLibrary>` so multiple project/session bridges can share
/// one library without ref-counting tricks at the call site.
pub struct WorkflowCache {
    libs: HashMap<(Slug, WorkflowId), Arc<WorkflowLibrary>>,
    workflows_root: PathBuf,
    profile_dir: String,
}

impl WorkflowCache {
    /// `workflows_root` is `<global_config_dir>/workflows`;
    /// `profile_dir` is `"release"` or `"debug"` (matching the project
    /// tier's `LUTIN_WORKFLOW_PROFILE`).
    pub fn new(workflows_root: PathBuf, profile_dir: String) -> Self {
        Self {
            libs: HashMap::new(),
            workflows_root,
            profile_dir,
        }
    }

    /// Load (or return a cached) `WorkflowLibrary` for the given
    /// project + workflow. The `.so` is dlopened on the first call and
    /// kept alive for the lifetime of this cache.
    pub fn load(
        &mut self,
        slug: &Slug,
        workflow: &WorkflowId,
    ) -> Result<Arc<WorkflowLibrary>, LoadError> {
        let key = (slug.clone(), workflow.clone());
        if let Some(existing) = self.libs.get(&key) {
            return Ok(Arc::clone(existing));
        }
        let path = workflow_so_path(&self.workflows_root, &self.profile_dir, workflow);
        // SAFETY: dlopen runs the cdylib's init code; we trust the
        // workflows we ship. Caller guarantees same-toolchain build
        // (see PLAN: "same-toolchain assumption").
        let lib = unsafe {
            Library::new(&path).map_err(|source| LoadError::Dlopen {
                path: path.clone(),
                source,
            })?
        };
        // SAFETY: the symbol's lifetime is bound to `lib`, which we
        // keep alive in the returned `WorkflowLibrary`. We invoke the
        // factory once and immediately drop the `Symbol` view.
        let workflow_box = unsafe {
            let sym: Symbol<CreateWorkflowFn> =
                lib.get(CREATE_WORKFLOW_SYMBOL)
                    .map_err(|source| LoadError::MissingSymbol {
                        symbol: String::from_utf8_lossy(CREATE_WORKFLOW_SYMBOL).into_owned(),
                        source,
                    })?;
            sym()
        };
        let entry = Arc::new(WorkflowLibrary {
            workflow: workflow_box,
            _lib: lib,
        });
        self.libs.insert(key, Arc::clone(&entry));
        Ok(entry)
    }
}
