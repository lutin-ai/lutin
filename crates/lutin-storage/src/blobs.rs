//! Content-addressed blob store. Hash = sha256 (raw 32 bytes; hex for paths/display).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Result, StoreError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct BlobHash([u8; 32]);

impl BlobHash {
    pub fn new(bytes: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(bytes);
        let digest = h.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        let bytes = hex::decode(s).map_err(|_| StoreError::BadHash)?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| StoreError::BadHash)?;
        Ok(Self(arr))
    }
}

#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn path_for(&self, hash: &BlobHash) -> PathBuf {
        // Two-char fan-out keeps any single dir small.
        let hex = hash.to_hex();
        let (a, b) = hex.split_at(2);
        self.root.join(a).join(b)
    }

    /// Store `data` and return its hash. Atomic via write-temp + rename.
    /// No-op if blob already exists.
    pub fn put(&self, data: &[u8]) -> Result<BlobHash> {
        let hash = BlobHash::new(data);
        let target = self.path_for(&hash);
        if target.exists() {
            return Ok(hash);
        }
        let parent = target.parent().expect("blob path has parent");
        fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(".tmp-{}", hash.to_hex()));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        // rename is atomic on same filesystem
        if let Err(e) = fs::rename(&tmp, &target) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(hash)
    }

    pub fn get(&self, hash: &BlobHash) -> Result<Vec<u8>> {
        let path = self.path_for(hash);
        let bytes = fs::read(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => StoreError::NotFound,
            _ => StoreError::Io(e),
        })?;
        let actual = BlobHash::new(&bytes);
        if actual != *hash {
            return Err(StoreError::BlobHashMismatch);
        }
        Ok(bytes)
    }

    pub fn exists(&self, hash: &BlobHash) -> bool {
        self.path_for(hash).exists()
    }

    pub fn delete(&self, hash: &BlobHash) -> Result<bool> {
        let path = self.path_for(hash);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}
