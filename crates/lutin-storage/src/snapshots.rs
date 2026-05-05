//! Snapshots: bytes stored in blob CAS, an index of (seq → hash) in sled.

use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;

use crate::blobs::BlobStore;
use crate::transcript::Seq;
use crate::{BlobHash, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotMeta {
    pub seq: Seq,
    pub hash: BlobHash,
    pub size: u64,
}

#[derive(Clone)]
pub struct Snapshots {
    tree: sled::Tree,
    blobs: BlobStore,
}

fn key(seq: Seq) -> [u8; 8] {
    seq.to_be_bytes()
}

fn parse_key(bytes: &[u8]) -> Option<Seq> {
    bytes.try_into().ok().map(Seq::from_be_bytes)
}

impl Snapshots {
    pub(crate) fn open(db: &sled::Db, namespace: &str, blobs: BlobStore) -> Result<Self> {
        let tree = db.open_tree(format!("snap:{namespace}"))?;
        Ok(Self { tree, blobs })
    }

    pub fn write(&self, at_seq: Seq, bytes: &[u8]) -> Result<SnapshotMeta> {
        let hash = self.blobs.put(bytes)?;
        let meta = SnapshotMeta {
            seq: at_seq,
            hash,
            size: bytes.len() as u64,
        };
        let meta_bytes = postcard::to_allocvec(&meta)?;
        self.tree.insert(key(at_seq), meta_bytes)?;
        Ok(meta)
    }

    pub fn write_typed<S: Serialize>(&self, at_seq: Seq, state: &S) -> Result<SnapshotMeta> {
        let bytes = postcard::to_allocvec(state)?;
        self.write(at_seq, &bytes)
    }

    pub fn latest(&self) -> Result<Option<(SnapshotMeta, Vec<u8>)>> {
        let Some((_, meta_bytes)) = self.tree.last()? else {
            return Ok(None);
        };
        let meta: SnapshotMeta = postcard::from_bytes(&meta_bytes)?;
        let bytes = self.blobs.get(&meta.hash)?;
        Ok(Some((meta, bytes)))
    }

    pub fn latest_typed<S: DeserializeOwned>(&self) -> Result<Option<(SnapshotMeta, S)>> {
        let Some((meta, bytes)) = self.latest()? else {
            return Ok(None);
        };
        let state = postcard::from_bytes(&bytes)?;
        Ok(Some((meta, state)))
    }

    /// Delete snapshot index entries except the most recent `keep`.
    /// Blobs remain in CAS (other refs may exist); call `gc_blobs` to
    /// drop unreferenced ones.
    pub fn prune_keeping(&self, keep: usize) -> Result<usize> {
        let mut entries: Vec<Seq> = self
            .tree
            .iter()
            .filter_map(|res| res.ok().and_then(|(k, _)| parse_key(&k)))
            .collect();
        entries.sort();
        if entries.len() <= keep {
            return Ok(0);
        }
        let cut = entries.len() - keep;
        let mut removed = 0;
        for seq in &entries[..cut] {
            if self.tree.remove(key(*seq))?.is_some() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn referenced_hashes(&self) -> Result<Vec<BlobHash>> {
        let mut out = Vec::new();
        for entry in self.tree.iter() {
            let (_, v) = entry?;
            let meta: SnapshotMeta = postcard::from_bytes(&v)?;
            out.push(meta.hash);
        }
        Ok(out)
    }
}
