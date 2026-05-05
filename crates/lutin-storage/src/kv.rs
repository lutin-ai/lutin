//! Typed sled tree wrapper.

use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Result, StoreError};

pub struct Kv<T> {
    tree: sled::Tree,
    _ty: PhantomData<T>,
}

impl<T> Kv<T>
where
    T: Serialize + DeserializeOwned,
{
    pub(crate) fn open(db: &sled::Db, namespace: &str) -> Result<Self> {
        let tree = db.open_tree(format!("kv:{namespace}"))?;
        Ok(Self {
            tree,
            _ty: PhantomData,
        })
    }

    pub fn put(&self, key: &str, value: &T) -> Result<()> {
        let bytes = postcard::to_allocvec(value)?;
        self.tree.insert(key.as_bytes(), bytes)?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Option<T>> {
        let Some(raw) = self.tree.get(key.as_bytes())? else {
            return Ok(None);
        };
        let value = postcard::from_bytes(&raw)?;
        Ok(Some(value))
    }

    pub fn delete(&self, key: &str) -> Result<bool> {
        Ok(self.tree.remove(key.as_bytes())?.is_some())
    }

    pub fn iter(&self) -> impl Iterator<Item = Result<(String, T)>> + '_ {
        self.tree.iter().map(|entry| {
            let (k, v) = entry?;
            let key = std::str::from_utf8(&k)
                .map_err(|_| StoreError::Postcard(postcard::Error::DeserializeBadEncoding))?
                .to_string();
            let value = postcard::from_bytes(&v)?;
            Ok((key, value))
        })
    }

    /// Compare-and-swap. Replaces the entry if its current value matches
    /// `expected`; insert when `expected` is `None`; delete when `new` is
    /// `None`. Returns `true` on success.
    pub fn cas(&self, key: &str, expected: Option<&T>, new: Option<&T>) -> Result<bool> {
        let expected_bytes = expected.map(postcard::to_allocvec).transpose()?;
        let new_bytes = new.map(postcard::to_allocvec).transpose()?;
        let res = self.tree.compare_and_swap(
            key.as_bytes(),
            expected_bytes.as_deref(),
            new_bytes.as_deref().map(sled::IVec::from),
        )?;
        Ok(res.is_ok())
    }
}
