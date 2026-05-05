//! Persistent ed25519 signing keypair: load-or-create + atomic file
//! write helpers. Used by every tier that holds a long-lived signing
//! key (control-panel, project supervisor) and by their handoff files
//! (pubkey, bound addr).

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use lutin_auth::SigningKey;

/// Load existing 32-byte ed25519 seed, or generate one and persist it
/// with `0o600`. Publication is atomic via temp+`link()`: on `EEXIST`
/// some other actor (or a prior run) won the race, so we read their
/// bytes instead of overwriting. This avoids the TOCTOU window a
/// post-rename re-read would open — we only read paths we either
/// just observed to exist or just failed to create.
pub fn load_or_create_keypair(path: &Path) -> anyhow::Result<SigningKey> {
    match std::fs::read(path) {
        Ok(bytes) => return seed_from_bytes(bytes).map(|s| SigningKey::from_bytes(&s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|_| anyhow::anyhow!("rng"))?;
    match write_atomic_create_new(path, &seed, 0o600) {
        Ok(()) => Ok(SigningKey::from_bytes(&seed)),
        Err(WriteAtomicError::AlreadyExists) => {
            let on_disk = std::fs::read(path)?;
            let seed = seed_from_bytes(on_disk)?;
            Ok(SigningKey::from_bytes(&seed))
        }
        Err(WriteAtomicError::Io(e)) => Err(e.into()),
    }
}

fn seed_from_bytes(bytes: Vec<u8>) -> anyhow::Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("keypair file must be 32 bytes"))
}

#[derive(Debug)]
pub enum WriteAtomicError {
    AlreadyExists,
    Io(std::io::Error),
}

impl From<std::io::Error> for WriteAtomicError {
    fn from(e: std::io::Error) -> Self {
        WriteAtomicError::Io(e)
    }
}

/// Write `bytes` to `path` atomically, failing if `path` already exists.
/// Uses temp file + `link()` (which is atomic and fails with `EEXIST` on
/// the destination) so concurrent readers never observe a partially
/// written file, and racing writers can't silently overwrite each other.
pub fn write_atomic_create_new(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), WriteAtomicError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = create_temp(path, bytes, mode)?;
    let result = match std::fs::hard_link(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(WriteAtomicError::AlreadyExists)
        }
        Err(e) => Err(WriteAtomicError::Io(e)),
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Write `bytes` to `path` via temp file + rename, overwriting any
/// existing file. Use for non-security-critical handoff files where
/// last-writer-wins is fine.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = create_temp(path, bytes, mode)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

fn create_temp(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<std::path::PathBuf> {
    for attempt in 0..16u32 {
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "rng"))?;
        let suffix_hex: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
        let tmp = path.with_extension(format!("tmp.{suffix_hex}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
        {
            Ok(mut f) => {
                f.write_all(bytes)?;
                f.sync_all()?;
                return Ok(tmp);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 15 => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not create temp file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keypair");
        let first = load_or_create_keypair(&path).unwrap();
        let second = load_or_create_keypair(&path).unwrap();
        assert_eq!(first.to_bytes(), second.to_bytes());
    }

    #[test]
    fn rejects_wrong_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keypair");
        std::fs::write(&path, [0u8; 16]).unwrap();
        assert!(load_or_create_keypair(&path).is_err());
    }

    #[test]
    fn adopts_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keypair");
        let seed = [7u8; 32];
        std::fs::write(&path, seed).unwrap();
        let loaded = load_or_create_keypair(&path).unwrap();
        assert_eq!(loaded.to_bytes(), seed);
        let loaded2 = load_or_create_keypair(&path).unwrap();
        assert_eq!(loaded2.to_bytes(), seed);
    }
}
