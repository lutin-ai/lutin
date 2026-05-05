//! Append-only typed log keyed by monotonic `Seq`.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Result, StoreError};

pub type Seq = u64;

#[derive(Clone)]
pub struct Transcript {
    db: Arc<sled::Db>,
    tree: sled::Tree,
}

fn key(seq: Seq) -> [u8; 8] {
    seq.to_be_bytes()
}

fn parse_key(bytes: &[u8]) -> Option<Seq> {
    bytes.try_into().ok().map(Seq::from_be_bytes)
}

impl Transcript {
    pub(crate) fn open(db: Arc<sled::Db>, namespace: &str) -> Result<Self> {
        let tree = db.open_tree(format!("xc:{namespace}"))?;
        Ok(Self { db, tree })
    }

    pub fn append(&self, payload: &[u8]) -> Result<Seq> {
        // `generate_id` is monotonic per-db, so seqs are unique and
        // ordered across all transcripts in this Store.
        let next = self.db.generate_id()?;
        self.tree.insert(key(next), payload)?;
        Ok(next)
    }

    pub fn append_typed<E: Serialize>(&self, entry: &E) -> Result<Seq> {
        let bytes = postcard::to_allocvec(entry)?;
        self.append(&bytes)
    }

    pub fn last_seq(&self) -> Result<Option<Seq>> {
        Ok(self.tree.last()?.and_then(|(k, _)| parse_key(&k)))
    }

    /// Iterate entries with seq >= `from`.
    pub fn iter_from(&self, from: Seq) -> impl Iterator<Item = Result<(Seq, Vec<u8>)>> {
        self.tree.range(key(from)..).map(|res| {
            let (k, v) = res?;
            let seq = parse_key(&k)
                .ok_or(StoreError::Postcard(postcard::Error::DeserializeBadEncoding))?;
            Ok((seq, v.to_vec()))
        })
    }

    pub fn iter_typed<E: DeserializeOwned>(
        &self,
        from: Seq,
    ) -> impl Iterator<Item = Result<(Seq, E)>> {
        self.iter_from(from).map(|res| {
            let (seq, bytes) = res?;
            let entry = postcard::from_bytes(&bytes)?;
            Ok((seq, entry))
        })
    }

    /// Drop entries with seq < `before`. Used after a snapshot at `before`.
    pub fn truncate_before(&self, before: Seq) -> Result<usize> {
        let mut removed = 0;
        let upper = key(before);
        let to_remove: Vec<_> = self
            .tree
            .range(..upper)
            .filter_map(|res| res.ok().map(|(k, _)| k))
            .collect();
        for k in to_remove {
            if self.tree.remove(&k)?.is_some() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}
