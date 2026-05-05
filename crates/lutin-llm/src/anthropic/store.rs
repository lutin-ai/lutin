//! Credential storage for Anthropic OAuth.
//!
//! [`OAuthCredentialStore`] holds the access/refresh token pair and serialises
//! refresh through an async mutex — the refresh token may be single-use, so
//! parallel refreshes would race and lock the user out. Persistence is
//! pluggable via [`CredBackend`]:
//!
//! - [`KeyringBackend`] — OS keychain (macOS Keychain / Windows Cred Mgr /
//!   Linux Secret Service). Host-side default.
//! - [`EncryptedFileBackend`] — AES-256-GCM over a file on disk, key supplied
//!   by the caller. For containerised engines with no D-Bus / keyring.
//! - [`MemoryBackend`] — in-process only. Tests and ephemeral flows.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use super::oauth::{refresh_tokens, TokenResponse};
use crate::LlmError;

const KEYRING_SERVICE: &str = "com.lutin.anthropic-oauth";
const KEYRING_USER: &str = "default";

const EXPIRY_BUFFER_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ms: i64,
    pub scopes: Vec<String>,
}

impl Credentials {
    pub fn from_token_response(
        tr: TokenResponse,
        prev_refresh: Option<String>,
    ) -> Result<Self, LlmError> {
        let refresh_token = tr.refresh_token.or(prev_refresh).ok_or_else(|| {
            LlmError::Other(
                "token response omitted refresh_token and no prior token was cached".into(),
            )
        })?;
        let scopes = tr
            .scope
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        Ok(Self {
            access_token: tr.access_token,
            refresh_token,
            expires_at_ms: now_ms() + tr.expires_in * 1000,
            scopes,
        })
    }

    pub fn near_expiry(&self) -> bool {
        now_ms() + EXPIRY_BUFFER_MS >= self.expires_at_ms
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Backend trait + impls
// ---------------------------------------------------------------------------

/// Persistence layer for Anthropic OAuth credentials. Sync because all impls
/// are cheap local I/O; the async surface lives at the store level where
/// refresh is serialised.
pub trait CredBackend: Send + Sync {
    fn read(&self) -> Result<Option<Credentials>, LlmError>;
    fn write(&self, creds: &Credentials) -> Result<(), LlmError>;
    fn clear(&self) -> Result<(), LlmError>;
}

/// OS keychain via the `keyring` crate. Not usable inside a plain Docker
/// container (no D-Bus / Secret Service) — use [`EncryptedFileBackend`]
/// there.
pub struct KeyringBackend {
    service: String,
    user: String,
}

impl KeyringBackend {
    pub fn new() -> Self {
        Self {
            service: KEYRING_SERVICE.into(),
            user: KEYRING_USER.into(),
        }
    }

    pub fn with_names(service: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            user: user.into(),
        }
    }

    fn entry(&self) -> Result<Entry, LlmError> {
        Entry::new(&self.service, &self.user).map_err(|e| LlmError::Other(format!("keyring: {e}")))
    }
}

impl Default for KeyringBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CredBackend for KeyringBackend {
    fn read(&self) -> Result<Option<Credentials>, LlmError> {
        match self.entry()?.get_password() {
            Ok(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            // Treat "not found" as absent; any other error propagates.
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(LlmError::Other(format!("keyring read: {e}"))),
        }
    }

    fn write(&self, creds: &Credentials) -> Result<(), LlmError> {
        let raw = serde_json::to_string(creds)?;
        self.entry()?
            .set_password(&raw)
            .map_err(|e| LlmError::Other(format!("keyring write: {e}")))
    }

    fn clear(&self) -> Result<(), LlmError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(LlmError::Other(format!("keyring delete: {e}"))),
        }
    }
}

/// AES-256-GCM encrypted JSON file. Intended for containerised engines that
/// don't have access to an OS keychain.
///
/// The 32-byte key must be supplied by the caller — typically passed in from
/// the desktop at engine-start time, or derived from a machine-stable secret
/// mounted into the container. **Do not hard-code it.**
pub struct EncryptedFileBackend {
    path: PathBuf,
    cipher: Aes256Gcm,
}

impl EncryptedFileBackend {
    /// Use a 32-byte key. The path is the file holding the ciphertext; it
    /// will be created (0600) on the first `write`.
    pub fn new(path: PathBuf, key: [u8; 32]) -> Self {
        let cipher = Aes256Gcm::new((&key).into());
        Self { path, cipher }
    }
}

/// On-disk frame: `| 12 B nonce | ciphertext+tag |`. Self-describing enough
/// to let future key rotations detect mismatch (decryption fails) without
/// version bytes.
const NONCE_LEN: usize = 12;

impl CredBackend for EncryptedFileBackend {
    fn read(&self) -> Result<Option<Credentials>, LlmError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(LlmError::Other(format!("cred file read: {e}"))),
        };
        if bytes.len() < NONCE_LEN + 16 {
            return Err(LlmError::Other("cred file: truncated".into()));
        }
        let (nonce_bytes, ct) = bytes.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        let plain = self
            .cipher
            .decrypt(nonce, ct)
            .map_err(|_| LlmError::Other("cred file: decrypt failed (wrong key?)".into()))?;
        let creds: Credentials = serde_json::from_slice(&plain)?;
        Ok(Some(creds))
    }

    fn write(&self, creds: &Credentials) -> Result<(), LlmError> {
        let plain = serde_json::to_vec(creds)?;
        let nonce_bytes: [u8; NONCE_LEN] = rand::random();
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = self
            .cipher
            .encrypt(nonce, plain.as_ref())
            .map_err(|_| LlmError::Other("cred file: encrypt failed".into()))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LlmError::Other(format!("cred file mkdir: {e}")))?;
        }
        // Write to a sibling tempfile then rename, so a crash mid-write
        // leaves the previous good ciphertext in place rather than a
        // truncated file the user has to delete by hand.
        let tmp = self.path.with_extension("tmp");
        write_file_0600(&tmp, &out)?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| LlmError::Other(format!("cred file rename: {e}")))
    }

    fn clear(&self) -> Result<(), LlmError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(LlmError::Other(format!("cred file delete: {e}"))),
        }
    }
}

#[cfg(unix)]
fn write_file_0600(path: &std::path::Path, data: &[u8]) -> Result<(), LlmError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| LlmError::Other(format!("cred file open: {e}")))?;
    f.write_all(data)
        .map_err(|e| LlmError::Other(format!("cred file write: {e}")))
}

#[cfg(not(unix))]
fn write_file_0600(path: &std::path::Path, data: &[u8]) -> Result<(), LlmError> {
    std::fs::write(path, data).map_err(|e| LlmError::Other(format!("cred file write: {e}")))
}

/// In-memory only; loses state on drop. Useful for tests and for the
/// `ANTHROPIC_OAUTH_TOKEN`-override path where refresh is not performed.
pub struct MemoryBackend {
    inner: Mutex<Option<Credentials>>,
}

impl MemoryBackend {
    pub fn empty() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }
}

impl CredBackend for MemoryBackend {
    fn read(&self) -> Result<Option<Credentials>, LlmError> {
        Ok(self.inner.lock().unwrap().clone())
    }
    fn write(&self, creds: &Credentials) -> Result<(), LlmError> {
        *self.inner.lock().unwrap() = Some(creds.clone());
        Ok(())
    }
    fn clear(&self) -> Result<(), LlmError> {
        *self.inner.lock().unwrap() = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Thread-safe Anthropic OAuth credential store. Clone freely; all clones
/// share the same cache, refresh mutex, and backend.
#[derive(Clone)]
pub struct OAuthCredentialStore {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    cached: Mutex<Option<Credentials>>,
    refresh_lock: AsyncMutex<()>,
    http: reqwest::Client,
    backend: Arc<dyn CredBackend>,
}

impl OAuthCredentialStore {
    /// Build a store on an arbitrary backend, loading any existing creds.
    ///
    /// "No creds stored" (`Ok(None)` from the backend) is fine and yields an
    /// unauthenticated store. Backend errors (e.g. keyring unreachable,
    /// encrypted-file decrypt failure) propagate — callers must decide
    /// whether to treat them as fatal or to fall back to a different
    /// backend.
    pub fn load_with(backend: Arc<dyn CredBackend>) -> Result<Self, LlmError> {
        let cached = backend.read()?;
        Ok(Self::build(backend, cached))
    }

    /// Convenience: keyring-backed store.
    pub fn load() -> Result<Self, LlmError> {
        Self::load_with(Arc::new(KeyringBackend::new()))
    }

    /// Persist fresh credentials to `backend` and return a store holding them.
    pub fn store_with(
        backend: Arc<dyn CredBackend>,
        creds: Credentials,
    ) -> Result<Self, LlmError> {
        backend.write(&creds)?;
        Ok(Self::build(backend, Some(creds)))
    }

    /// Convenience: keyring-backed store with fresh creds.
    pub fn store(creds: Credentials) -> Result<Self, LlmError> {
        Self::store_with(Arc::new(KeyringBackend::new()), creds)
    }

    fn build(backend: Arc<dyn CredBackend>, cached: Option<Credentials>) -> Self {
        Self {
            inner: Arc::new(StoreInner {
                cached: Mutex::new(cached),
                refresh_lock: AsyncMutex::new(()),
                http: reqwest::Client::new(),
                backend,
            }),
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.inner.cached.lock().unwrap().is_some()
    }

    pub fn clear(&self) -> Result<(), LlmError> {
        // Clear backend first; if that fails, leave the cache intact so the
        // two sources of truth don't diverge. Only drop the in-memory copy
        // once the persistent copy is gone.
        self.inner.backend.clear()?;
        *self.inner.cached.lock().unwrap() = None;
        Ok(())
    }

    /// Fast path under no lock; slow path serialised behind `refresh_lock`
    /// with a re-check so parallel callers don't double-refresh.
    pub async fn get_valid_access_token(&self) -> Result<String, LlmError> {
        if let Some(c) = self.load_unexpired() {
            return Ok(c.access_token);
        }
        let _guard = self.inner.refresh_lock.lock().await;
        if let Some(c) = self.load_unexpired() {
            return Ok(c.access_token);
        }
        self.do_refresh().await
    }

    /// Refresh after a 401 from the Messages API: bypasses the cache freshness
    /// check, but still serialised through `refresh_lock`. If a concurrent task
    /// already refreshed past `stale_token`, returns the new token instead.
    pub async fn refresh_after_auth_error(&self, stale_token: &str) -> Result<String, LlmError> {
        let _guard = self.inner.refresh_lock.lock().await;
        // Snapshot the cached token under the sync lock, then drop the guard
        // before doing any async work / network I/O.
        let cached = {
            let g = self.inner.cached.lock().unwrap();
            g.clone()
        };
        if let Some(c) = cached {
            if c.access_token != stale_token {
                return Ok(c.access_token);
            }
        }
        self.do_refresh().await
    }

    fn load_unexpired(&self) -> Option<Credentials> {
        self.inner
            .cached
            .lock()
            .unwrap()
            .as_ref()
            .filter(|c| !c.near_expiry())
            .cloned()
    }

    async fn do_refresh(&self) -> Result<String, LlmError> {
        // Snapshot the current credentials under the sync lock, then drop the
        // guard before any `.await` or backend I/O.
        let current = {
            let g = self.inner.cached.lock().unwrap();
            g.clone()
                .ok_or_else(|| LlmError::Other("not authenticated".into()))?
        };

        match refresh_tokens(&self.inner.http, &current.refresh_token).await {
            Ok(tr) => {
                let new = Credentials::from_token_response(tr, Some(current.refresh_token))?;
                // Persist to the backend first; only mirror into the cache
                // once the durable copy is in place so the two sources of
                // truth can't diverge on a backend write failure.
                self.inner.backend.write(&new)?;
                let token = new.access_token.clone();
                {
                    let mut g = self.inner.cached.lock().unwrap();
                    *g = Some(new);
                }
                Ok(token)
            }
            Err(err) => {
                if is_fatal_refresh_error(&err) {
                    let _ = self.clear();
                }
                Err(err)
            }
        }
    }
}

/// Only `invalid_grant` / `invalid_refresh_token` mean "really logged out".
/// Transient errors (429, 5xx, network) must not clear credentials.
fn is_fatal_refresh_error(err: &LlmError) -> bool {
    match err {
        LlmError::Other(msg) => {
            let m = msg.to_lowercase();
            m.contains("invalid_grant")
                || m.contains("invalid_refresh_token")
                || m.contains("refresh token not found")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_creds() -> Credentials {
        Credentials {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at_ms: now_ms() + 10_000,
            scopes: vec!["user:inference".into()],
        }
    }

    #[test]
    fn encrypted_file_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("lutin-cred-test-{}.bin", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let key: [u8; 32] = [7u8; 32];
        let backend = EncryptedFileBackend::new(tmp.clone(), key);

        assert!(backend.read().unwrap().is_none());
        let creds = sample_creds();
        backend.write(&creds).unwrap();

        let loaded = backend.read().unwrap().expect("present");
        assert_eq!(loaded.access_token, "at");
        assert_eq!(loaded.refresh_token, "rt");

        // Wrong key fails cleanly.
        let wrong = EncryptedFileBackend::new(tmp.clone(), [0u8; 32]);
        assert!(wrong.read().is_err());

        backend.clear().unwrap();
        assert!(backend.read().unwrap().is_none());
    }

    #[test]
    fn memory_backend_isolated_per_store() {
        let b: Arc<dyn CredBackend> = Arc::new(MemoryBackend::empty());
        let store = OAuthCredentialStore::load_with(b.clone()).unwrap();
        assert!(!store.is_authenticated());
        b.write(&sample_creds()).unwrap();
        // The store caches the initial read, so a later backend-side write
        // isn't reflected unless we reload.
        assert!(!store.is_authenticated());
        let store2 = OAuthCredentialStore::load_with(b).unwrap();
        assert!(store2.is_authenticated());
    }
}
