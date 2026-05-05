//! On-disk storage primitives shared by control-panel, project, and
//! workflow tiers.
//!
//! - [`Store`] groups sled trees + a blob CAS directory under one root.
//!   Callers open one `Store` per process; namespaces (`Kv`, `Transcript`,
//!   `Snapshots`) are cheap to derive from it.
//! - [`Resolver`] handles the two-tier (global + project) filesystem
//!   lookup for personas, workflows, settings, secrets.

use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;

pub mod blobs;
pub mod kv;
pub mod resolver;
pub mod snapshots;
pub mod transcript;

pub use blobs::{BlobHash, BlobStore};
pub use kv::Kv;
pub use resolver::{ResolvedEntity, Resolver, Scope};
pub use snapshots::{SnapshotMeta, Snapshots};
pub use transcript::{Seq, Transcript};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sled: {0}")]
    Sled(#[from] sled::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("blob hash mismatch")]
    BlobHashMismatch,
    #[error("bad hash")]
    BadHash,
    #[error("not found")]
    NotFound,
    #[error("cas conflict")]
    CasConflict,
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Selects the on-disk layout for a [`Store`].
pub enum StoreLayout {
    /// Single root: sled lives under `root/db`, blobs under `root/blobs`.
    Combined(PathBuf),
    /// Independent paths for the sled db and the blob CAS. Use when blobs
    /// are shared between processes (supervisor + workflows).
    Split { db: PathBuf, blobs: PathBuf },
}

#[derive(Clone)]
pub struct Store {
    db: Arc<sled::Db>,
    blobs: BlobStore,
}

impl Store {
    /// Open (or create) a `Store` with the given on-disk layout.
    pub fn open(layout: StoreLayout) -> Result<Self> {
        let (db_path, blob_path) = match layout {
            StoreLayout::Combined(root) => {
                std::fs::create_dir_all(&root)?;
                (root.join("db"), root.join("blobs"))
            }
            StoreLayout::Split { db, blobs } => (db, blobs),
        };
        let db = sled::open(db_path)?;
        let blobs = BlobStore::open(blob_path)?;
        Ok(Self {
            db: Arc::new(db),
            blobs,
        })
    }

    pub fn kv<T>(&self, namespace: &str) -> Result<Kv<T>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        Kv::open(&self.db, namespace)
    }

    pub fn transcript(&self, namespace: &str) -> Result<Transcript> {
        Transcript::open(self.db.clone(), namespace)
    }

    pub fn snapshots(&self, namespace: &str) -> Result<Snapshots> {
        Snapshots::open(&self.db, namespace, self.blobs.clone())
    }

    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    pub fn flush(&self) -> Result<()> {
        self.db.flush()?;
        Ok(())
    }
}

/// Standard layout helpers for the two on-disk roots.
pub mod layout {
    use std::path::{Path, PathBuf};

    pub fn global_config(root: &Path) -> PathBuf {
        root.join("lutin").join(".lutin")
    }

    pub fn project_config(root: &Path, slug: &str) -> PathBuf {
        root.join(slug).join(".lutin")
    }

    pub fn project_storage(storage_root: &Path, slug: &str) -> PathBuf {
        storage_root.join("projects").join(slug)
    }

    pub fn global_storage(storage_root: &Path) -> PathBuf {
        storage_root.join("global")
    }
}
