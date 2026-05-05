//! Seed the global config tier with in-tree defaults on launch.
//!
//! Three payloads are embedded into the control-panel binary at compile
//! time (staged through `build.rs` → `OUT_DIR/embedded_*/`):
//!
//! - **vendor** (`<global>/vendor/`): the lutin-* SDK crates the chat
//!   workflow depends on. Re-staged wholesale on every CP boot — this
//!   is how SDK upgrades flow into seeded and user-authored workflows
//!   alike. Treated as a CP-owned artifact; never user-editable.
//! - **chat workflow** (`<global>/workflows/chat/`): seeded once and
//!   re-seeded only when `seed_version` mismatches (so SDK breaking
//!   changes can update the default workflow's `src/`). User edits to
//!   the seeded chat are clobbered on version bump — users who want
//!   custom behavior should fork into a different workflow id, where
//!   their `src/` is theirs forever and only `vendor/` updates.
//! - **assistant persona** (`personas/assistant.toml`): seeded once,
//!   user edits preserved.
//!
//! Workflows running in tier-3 must NOT write to global; only the
//! control-panel does. This is the *only* path that touches global
//! config from the CP process.

use include_dir::{Dir, include_dir};
use std::io;
use std::path::Path;
use tracing::info;

static EMBEDDED_CHAT: Dir<'_> = include_dir!("$OUT_DIR/embedded_chat");
static EMBEDDED_VENDOR: Dir<'_> = include_dir!("$OUT_DIR/embedded_vendor");
const EMBEDDED_ASSISTANT_PERSONA: &str = include_str!("../../personas/assistant.toml");
const SEED_VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/seed_version.txt"));

const SEED_VERSION_MARKER: &str = ".seed_version";

/// Idempotent per payload:
/// * vendor is wholesale-rewritten every call (CP-owned artifact);
/// * chat is re-seeded only on `seed_version` mismatch;
/// * persona is seeded once.
pub fn seed(global_config_dir: &Path) -> io::Result<()> {
    restage_vendor(global_config_dir)?;
    seed_chat_workflow(global_config_dir)?;
    seed_assistant_persona(global_config_dir)?;
    Ok(())
}

/// Re-stage the vendored SDK tree from the embedded payload. The dir
/// is removed first so a smaller payload (after dropping a crate)
/// doesn't leave stale crates behind. Workflows depend on these via
/// `path = "../../vendor/<crate>"`, so the rewrite happens before the
/// project tier runs `cargo build`.
fn restage_vendor(global_config_dir: &Path) -> io::Result<()> {
    let dst = global_config_dir.join("vendor");
    info!(path = %dst.display(), "restaging workflow vendor tree");
    if dst.exists() {
        std::fs::remove_dir_all(&dst)?;
    }
    std::fs::create_dir_all(&dst)?;
    extract_into(&EMBEDDED_VENDOR, EMBEDDED_VENDOR.path(), &dst)?;
    Ok(())
}

/// Seed the chat workflow once, or re-seed on `seed_version` mismatch.
/// Re-seeding clobbers `Cargo.toml`, `manifest.toml`, and `src/` —
/// `target/` (cargo's build cache) is preserved so we don't force a
/// from-scratch rebuild on every CP release; cargo's incremental
/// detection takes it from there.
fn seed_chat_workflow(global_config_dir: &Path) -> io::Result<()> {
    let dst = global_config_dir.join("workflows").join("chat");
    let marker = dst.join(SEED_VERSION_MARKER);

    let on_disk = match std::fs::read_to_string(&marker) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let needs_seed = match on_disk.as_deref() {
        Some(v) => v.trim() != SEED_VERSION.trim(),
        None => true,
    };
    if !needs_seed {
        return Ok(());
    }

    let kind = if on_disk.is_some() { "re-seeding" } else { "seeding" };
    info!(path = %dst.display(), version = %SEED_VERSION.trim(), "{kind} default chat workflow");
    std::fs::create_dir_all(&dst)?;

    // Clobber the seeded surface only — keep `target/` and any
    // user-stashed files we don't ship in the embedded payload.
    for entry in &["Cargo.toml", "manifest.toml", "src"] {
        let p = dst.join(entry);
        match std::fs::symlink_metadata(&p) {
            Ok(meta) if meta.file_type().is_dir() => std::fs::remove_dir_all(&p)?,
            Ok(_) => std::fs::remove_file(&p)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    extract_into(&EMBEDDED_CHAT, EMBEDDED_CHAT.path(), &dst)?;
    std::fs::write(&marker, SEED_VERSION.trim())?;
    Ok(())
}

fn seed_assistant_persona(global_config_dir: &Path) -> io::Result<()> {
    let dir = global_config_dir.join("personas");
    let path = dir.join("assistant.toml");
    if path.exists() {
        return Ok(());
    }
    info!(path = %path.display(), "seeding default assistant persona");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, EMBEDDED_ASSISTANT_PERSONA)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn seed_creates_vendor_workflow_and_persona() {
        let tmp = tempdir().unwrap();
        seed(tmp.path()).unwrap();

        // vendor/ contains every embedded crate, with src/ + Cargo.toml.
        let vendor = tmp.path().join("vendor");
        assert!(vendor.is_dir());
        assert!(vendor.join("lutin-workflow-sdk/Cargo.toml").is_file());
        assert!(vendor.join("lutin-workflow-sdk/src/lib.rs").is_file());

        // chat workflow is seeded with a version marker and rewritten
        // Cargo.toml pointing at vendor.
        let chat = tmp.path().join("workflows/chat");
        let cargo = std::fs::read_to_string(chat.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("../../vendor/lutin-workflow-sdk"));
        assert!(!cargo.contains("../../crates/"));
        let marker = std::fs::read_to_string(chat.join(SEED_VERSION_MARKER)).unwrap();
        assert_eq!(marker.trim(), SEED_VERSION.trim());

        assert!(tmp.path().join("personas/assistant.toml").is_file());
    }

    #[test]
    fn second_seed_is_idempotent_when_version_matches() {
        let tmp = tempdir().unwrap();
        seed(tmp.path()).unwrap();
        let chat = tmp.path().join("workflows/chat");
        // Stash a user edit; the marker matches, so re-seed should
        // leave it alone.
        let user_file = chat.join("src/USER_EDIT.txt");
        std::fs::write(&user_file, b"hello").unwrap();
        seed(tmp.path()).unwrap();
        assert!(user_file.exists(), "user edits preserved when version matches");
    }

    #[test]
    fn seed_clobbers_workflow_on_version_mismatch() {
        let tmp = tempdir().unwrap();
        seed(tmp.path()).unwrap();
        let chat = tmp.path().join("workflows/chat");
        // Force a stale marker to simulate a CP upgrade.
        std::fs::write(chat.join(SEED_VERSION_MARKER), b"deadbeef").unwrap();
        let user_file = chat.join("src/USER_EDIT.txt");
        std::fs::write(&user_file, b"hello").unwrap();
        seed(tmp.path()).unwrap();
        assert!(!user_file.exists(), "src/ is clobbered on version mismatch");
        let marker = std::fs::read_to_string(chat.join(SEED_VERSION_MARKER)).unwrap();
        assert_eq!(marker.trim(), SEED_VERSION.trim());
    }

    #[test]
    fn vendor_is_restaged_every_call() {
        let tmp = tempdir().unwrap();
        seed(tmp.path()).unwrap();
        let stray = tmp.path().join("vendor/STRAY_CRATE");
        std::fs::create_dir_all(&stray).unwrap();
        seed(tmp.path()).unwrap();
        assert!(!stray.exists(), "vendor wholesale-rewritten on every boot");
    }
}

/// Mirror an `include_dir::Dir` onto disk under `dst`. We can't lean on
/// `Dir::extract` because it errors out if any subdirectory already
/// exists; partial seeds (e.g. someone manually pre-created `src/`)
/// would then permanently break startup.
///
/// `entry.path()` on embedded entries is already relative to the
/// top-level embedded root, so we strip that root once and join onto
/// `dst` — no per-recursion prefix bookkeeping. Subdirs are created in
/// a top-down pass before any files, so the file pass never has to
/// worry about missing parents.
fn extract_into(dir: &Dir<'_>, root_prefix: &Path, dst: &Path) -> io::Result<()> {
    for sub in dir.dirs() {
        let rel = sub
            .path()
            .strip_prefix(root_prefix)
            .expect("nested dir under embedded root");
        std::fs::create_dir_all(dst.join(rel))?;
        extract_into(sub, root_prefix, dst)?;
    }
    for file in dir.files() {
        let rel = file
            .path()
            .strip_prefix(root_prefix)
            .expect("file under embedded root");
        std::fs::write(dst.join(rel), file.contents())?;
    }
    Ok(())
}
