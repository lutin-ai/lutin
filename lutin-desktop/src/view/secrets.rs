//! Secret store view (stub).
//!
//! Reference: `../lutin/desktop/src/view/secrets.rs`. Manages
//! credentials the chrome itself owns — distinct from the global
//! provider config (which lives in `<global>/settings.toml` and is
//! managed by the control panel). The original use case is the
//! Anthropic OAuth subscription flow: the desktop runs the OAuth
//! handshake, persists the bearer token in an OS keychain (or
//! file-backed fallback), and the chrome refreshes it on demand.
//!
//! Punted until we wire OAuth into a workflow. The chrome's only
//! current "secret" is the control-panel token, which lives in
//! `crate::settings::DesktopSettings`.
