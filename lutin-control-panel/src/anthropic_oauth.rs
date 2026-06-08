//! Anthropic subscription-OAuth broker.
//!
//! CP is the single process that holds the refresh token (host keyring)
//! and the only writer of the brokered access-token file engines read
//! (`<global>/credentials/anthropic-access.json`). Engines never refresh —
//! a background task here keeps the mirrored token fresh well before the
//! 5-minute staleness buffer brokered stores apply, so containers always
//! find a usable token through their read-only `/global` mount.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lutin_llm::anthropic::{
    begin_login, brokered_token_path, oauth, BrokeredFileBackend, CredBackend, Credentials,
    KeyringBackend, PendingLogin, PlainFileBackend,
};
use tracing::{info, warn};

/// Keyring when available, plain 0600 file under the CP data dir otherwise.
/// Many Linux hosts run CP without a Secret Service on the bus; the file
/// lives outside any container mount so the refresh token stays host-only.
struct FallbackBackend {
    keyring: KeyringBackend,
    file: PlainFileBackend,
}

impl CredBackend for FallbackBackend {
    fn read(&self) -> Result<Option<Credentials>, lutin_llm::LlmError> {
        match self.keyring.read() {
            Ok(Some(c)) => Ok(Some(c)),
            Ok(None) => self.file.read(),
            Err(e) => {
                warn!(error = %e, "anthropic oauth: keyring unavailable, using file store");
                self.file.read()
            }
        }
    }

    fn write(&self, creds: &Credentials) -> Result<(), lutin_llm::LlmError> {
        match self.keyring.write(creds) {
            Ok(()) => {
                // Don't leave a stale plaintext copy behind once the
                // keyring works again.
                let _ = self.file.clear();
                Ok(())
            }
            Err(e) => {
                warn!(error = %e, "anthropic oauth: keyring unavailable, using file store");
                self.file.write(creds)
            }
        }
    }

    fn clear(&self) -> Result<(), lutin_llm::LlmError> {
        if let Err(e) = self.keyring.clear() {
            warn!(error = %e, "anthropic oauth: keyring clear failed");
        }
        self.file.clear()
    }
}

/// Refresh when within this much of expiry. Must comfortably exceed the
/// brokered store's 5-minute buffer plus one poll interval.
const REFRESH_BUFFER_MS: i64 = 20 * 60 * 1000;
const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct OauthBroker {
    inner: Arc<Inner>,
}

struct Inner {
    pending: Mutex<Option<PendingLogin>>,
    creds: Arc<dyn CredBackend>,
    mirror: BrokeredFileBackend,
    http: lutin_llm::reqwest::Client,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl OauthBroker {
    pub fn new(global_config_dir: PathBuf, data_dir: PathBuf) -> Self {
        let broker = Self {
            inner: Arc::new(Inner {
                pending: Mutex::new(None),
                creds: Arc::new(FallbackBackend {
                    keyring: KeyringBackend::new(),
                    file: PlainFileBackend::new(data_dir.join("anthropic-oauth.json")),
                }),
                mirror: BrokeredFileBackend::new(brokered_token_path(&global_config_dir)),
                http: lutin_llm::reqwest::Client::new(),
                refresh_lock: tokio::sync::Mutex::new(()),
            }),
        };
        tokio::spawn(broker.clone().refresh_loop());
        broker
    }

    pub fn begin(&self) -> Result<String, String> {
        let (pending, url) = begin_login().map_err(|e| e.to_string())?;
        *self.inner.pending.lock().unwrap() = Some(pending);
        Ok(url)
    }

    pub async fn complete(&self, code: &str) -> Result<i64, String> {
        let pending = self
            .inner
            .pending
            .lock()
            .unwrap()
            .take()
            .ok_or("no login in progress — start over")?;
        let _store = pending
            .complete_with(code, Arc::clone(&self.inner.creds))
            .await
            .map_err(|e| e.to_string())?;
        let creds = self
            .inner
            .creds
            .read()
            .map_err(|e| e.to_string())?
            .ok_or("login completed but no credentials stored")?;
        self.mirror(&creds);
        Ok(creds.expires_at_ms)
    }

    pub fn status(&self) -> (bool, Option<i64>) {
        match self.inner.creds.read() {
            Ok(Some(c)) => (true, Some(c.expires_at_ms)),
            _ => (false, None),
        }
    }

    pub fn logout(&self) -> Result<(), String> {
        self.inner.creds.clear().map_err(|e| e.to_string())?;
        self.inner.mirror.clear().map_err(|e| e.to_string())
    }

    fn mirror(&self, creds: &Credentials) {
        if let Err(e) = self.inner.mirror.write(creds) {
            warn!(error = %e, "anthropic oauth: failed to mirror access token");
        }
    }

    async fn refresh_loop(self) {
        loop {
            self.tick().await;
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Mirror-and-refresh pass: ensure the brokered file matches the keyring
    /// and refresh the token when close to expiry. Runs at startup (so a
    /// reboot re-creates the mirror) and every poll thereafter.
    async fn tick(&self) {
        let _guard = self.inner.refresh_lock.lock().await;
        let creds = match self.inner.creds.read() {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                warn!(error = %e, "anthropic oauth: credential read failed");
                return;
            }
        };
        if now_ms() + REFRESH_BUFFER_MS < creds.expires_at_ms {
            match self.inner.mirror.read() {
                Ok(Some(m)) if m.access_token == creds.access_token => {}
                _ => self.mirror(&creds),
            }
            return;
        }
        match oauth::refresh_tokens(&self.inner.http, &creds.refresh_token).await {
            Ok(tr) => match Credentials::from_token_response(tr, Some(creds.refresh_token)) {
                Ok(new) => {
                    if let Err(e) = self.inner.creds.write(&new) {
                        warn!(error = %e, "anthropic oauth: credential write failed");
                        return;
                    }
                    self.mirror(&new);
                    info!("anthropic oauth: access token refreshed");
                }
                Err(e) => warn!(error = %e, "anthropic oauth: bad token response"),
            },
            Err(e) => warn!(error = %e, "anthropic oauth: refresh failed"),
        }
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
