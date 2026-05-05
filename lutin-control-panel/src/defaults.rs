//! Seed the global config tier with in-tree defaults on launch.
//!
//! Currently only the assistant persona is seeded; workflow `.so`s come
//! from installed Docker images (see `workflow_images`).

use std::io;
use std::path::Path;
use tracing::info;

const EMBEDDED_ASSISTANT_PERSONA: &str = include_str!("../../personas/assistant.toml");

pub fn seed(global_config_dir: &Path) -> io::Result<()> {
    seed_assistant_persona(global_config_dir)?;
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
    fn seed_creates_persona() {
        let tmp = tempdir().unwrap();
        seed(tmp.path()).unwrap();
        assert!(tmp.path().join("personas/assistant.toml").is_file());
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
}
