//! Workflow cdylib loader.
//!
//! The cdylib for each workflow is stored *inside* its Docker image
//! and shipped from the control-panel over the WS protocol via
//! `GetWorkflowCdylib`. This module never reads from a shared
//! filesystem — bytes arrive over the wire, get written into a
//! desktop-local cache keyed by image digest, and are dlopened from
//! there. libloading needs an on-disk path, so the cache file is the
//! one bridge to the filesystem.
//!
//! Cache layout: `<cache>/lutin/workflows/<id>/<digest>/lib<id>.so`.
//! `<digest>` makes upgrades trivial — a new image rebuild produces a
//! new digest; the next `ListWorkflows`+install lands the new bytes
//! in a parallel directory. Old digests can be GC'd later.
//!
//! The cache lives inside `dirs::cache_dir()` (`~/.cache` on Linux),
//! intentionally not under `~/.config` — bytes are reproducible from
//! the image and treated as derived data.
//!
//! The returned `Box<dyn Workflow>` borrows code (vtables, statics)
//! from the `Library` it came from, so the two are kept together in
//! `WorkflowLibrary` and the field-drop order is load-bearing: the
//! workflow drops first, then the library closes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::{Library, Symbol};
use lutin_ids::WorkflowId;
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
    #[error("write cdylib {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("dlopen {path}: {source}")]
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

/// Cache of loaded workflow libraries, keyed by `(workflow_id, digest)`.
/// Each `(id, digest)` pair points at a unique on-disk file, so a new
/// image rebuild produces a new key and we never have to invalidate.
/// `Arc<WorkflowLibrary>` so multiple callers can share a single open
/// handle.
pub struct WorkflowCache {
    libs: HashMap<(WorkflowId, String), Arc<WorkflowLibrary>>,
    cache_root: PathBuf,
}

impl WorkflowCache {
    /// `cache_root` is the directory under which per-workflow / per-
    /// digest subdirs are created. Typically
    /// `dirs::cache_dir().join("lutin").join("workflows")`.
    pub fn new(cache_root: PathBuf) -> Self {
        Self {
            libs: HashMap::new(),
            cache_root,
        }
    }

    /// Try to dlopen a workflow whose bytes are already on disk for the
    /// given digest. Returns `Ok(None)` when the cache file is missing —
    /// the caller should request the bytes via `GetWorkflowCdylib` and
    /// install them with [`install`]. Returns `Ok(Some(_))` for both
    /// fresh and previously-loaded entries.
    pub fn try_load(
        &mut self,
        workflow: &WorkflowId,
        digest: &str,
    ) -> Result<Option<Arc<WorkflowLibrary>>, LoadError> {
        let key = (workflow.clone(), digest.to_owned());
        if let Some(existing) = self.libs.get(&key) {
            return Ok(Some(Arc::clone(existing)));
        }
        let path = self.path_for(workflow, digest);
        if !path.exists() {
            return Ok(None);
        }
        let lib = open_workflow(&path)?;
        let entry = Arc::new(lib);
        self.libs.insert(key, Arc::clone(&entry));
        Ok(Some(entry))
    }

    /// Install freshly-fetched bytes for `(workflow, digest)`: write
    /// them to the cache and dlopen. Replaces any previously-cached
    /// entry for the same key.
    pub fn install(
        &mut self,
        workflow: &WorkflowId,
        digest: &str,
        bytes: &[u8],
    ) -> Result<Arc<WorkflowLibrary>, LoadError> {
        let path = self.path_for(workflow, digest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| LoadError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // Write to a sibling tmp file then rename so a partial write
        // can never be dlopened on a later run.
        let tmp = path.with_extension("so.tmp");
        std::fs::write(&tmp, bytes).map_err(|source| LoadError::Write {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, &path).map_err(|source| LoadError::Write {
            path: path.clone(),
            source,
        })?;
        let lib = open_workflow(&path)?;
        let entry = Arc::new(lib);
        self.libs
            .insert((workflow.clone(), digest.to_owned()), Arc::clone(&entry));
        Ok(entry)
    }

    fn path_for(&self, workflow: &WorkflowId, digest: &str) -> PathBuf {
        let id = workflow.as_str();
        // Digests sometimes include `:` (e.g. `sha256:abc`); replace to
        // keep the path portable.
        let digest_seg = digest.replace([':', '/'], "_");
        self.cache_root
            .join(id)
            .join(digest_seg)
            .join(format!("lib{id}.so"))
    }
}

fn open_workflow(path: &Path) -> Result<WorkflowLibrary, LoadError> {
    let probe = lutin_workflow_ui::typeid_probe();
    eprintln!("[desktop]      typeid_probe = {probe:?}");
    // SAFETY: dlopen runs the cdylib's init code; we trust workflows
    // shipped via images vetted by the control-panel.
    let lib = unsafe {
        Library::new(path).map_err(|source| LoadError::Dlopen {
            path: path.to_path_buf(),
            source,
        })?
    };
    // SAFETY: the symbol's lifetime is bound to `lib`, which we keep
    // alive in the returned `WorkflowLibrary`. We invoke the factory
    // once and immediately drop the `Symbol` view.
    let workflow_box = unsafe {
        let sym: Symbol<CreateWorkflowFn> =
            lib.get(CREATE_WORKFLOW_SYMBOL)
                .map_err(|source| LoadError::MissingSymbol {
                    symbol: String::from_utf8_lossy(CREATE_WORKFLOW_SYMBOL).into_owned(),
                    source,
                })?;
        sym()
    };
    Ok(WorkflowLibrary {
        workflow: workflow_box,
        _lib: lib,
    })
}
