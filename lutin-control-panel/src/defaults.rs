//! Seed the global config tier with in-tree defaults on launch.
//!
//! Each entry in [`DEFAULT_PERSONAS`] is written into
//! `<global>/personas/<name>.toml` if absent. User edits to an
//! existing file are preserved (we never overwrite). Deletions are
//! NOT remembered — a deleted default reappears on next launch. If we
//! want delete-stickiness later, the plan is a marker file plus a
//! "Restore defaults" action in settings.

use std::io;
use std::path::Path;
use tracing::info;

const DEFAULT_PERSONAS: &[(&str, &str)] = &[
    (
        "assistant",
        include_str!("../../personas/assistant.toml"),
    ),
    (
        "orchestrator",
        include_str!("../../personas/orchestrator.toml"),
    ),
    (
        "researcher",
        include_str!("../../personas/researcher.toml"),
    ),
];

pub fn seed(global_config_dir: &Path) -> io::Result<()> {
    let dir = global_config_dir.join("personas");
    for (name, body) in DEFAULT_PERSONAS {
        let path = dir.join(format!("{name}.toml"));
        if path.exists() {
            continue;
        }
        info!(path = %path.display(), "seeding default persona");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&path, body)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn seed_creates_all_defaults() {
        let tmp = tempdir().unwrap();
        seed(tmp.path()).unwrap();
        for (name, _) in DEFAULT_PERSONAS {
            assert!(
                tmp.path().join(format!("personas/{name}.toml")).is_file(),
                "missing seeded persona: {name}"
            );
        }
    }

    #[test]
    fn second_seed_preserves_user_persona() {
        let tmp = tempdir().unwrap();
        seed(tmp.path()).unwrap();
        let path = tmp.path().join("personas/assistant.toml");
        std::fs::write(&path, b"user content").unwrap();
        seed(tmp.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "user content");
    }

    #[test]
    fn second_seed_resurrects_deleted_default() {
        let tmp = tempdir().unwrap();
        seed(tmp.path()).unwrap();
        let path = tmp.path().join("personas/researcher.toml");
        std::fs::remove_file(&path).unwrap();
        seed(tmp.path()).unwrap();
        assert!(path.is_file());
    }
}
