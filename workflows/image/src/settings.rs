//! Project-scoped image-workflow settings persistence.
//!
//! Stored at `<project_config_dir>/image/lutin.image.toml`. Every field
//! is optional in the on-disk format so partial overrides work; missing
//! fields fall back to `ImageSettings::default()`. Writes go through
//! `lutin_keypair::write_atomic` so a crashed write doesn't corrupt
//! the file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image_workflow::ImageSettings;
use serde::{Deserialize, Serialize};

const FILE: &str = "lutin.image.toml";
const DIR: &str = "image";

/// Path to the settings file relative to the project's `.lutin/` dir.
fn settings_path(project_config_dir: &Path) -> PathBuf {
    project_config_dir.join(DIR).join(FILE)
}

/// On-disk shape: every field optional. Decoupled from the wire
/// `ImageSettings` so adding new settings doesn't force the user's
/// existing toml to re-state them.
#[derive(Debug, Default, Serialize, Deserialize)]
struct OnDisk {
    comfyui_url: Option<String>,
    default_width: Option<u32>,
    default_height: Option<u32>,
    default_count: Option<u32>,
    default_steps: Option<u32>,
    default_cfg: Option<f32>,
    default_model_id: Option<String>,
}

impl OnDisk {
    fn merge_into(self, base: &mut ImageSettings) {
        if let Some(v) = self.comfyui_url {
            base.comfyui_url = v;
        }
        if let Some(v) = self.default_width {
            base.default_width = v;
        }
        if let Some(v) = self.default_height {
            base.default_height = v;
        }
        if let Some(v) = self.default_count {
            base.default_count = v;
        }
        if let Some(v) = self.default_steps {
            base.default_steps = v;
        }
        if let Some(v) = self.default_cfg {
            base.default_cfg = v;
        }
        if let Some(v) = self.default_model_id {
            base.default_model_id = v;
        }
    }

    fn from_settings(s: &ImageSettings) -> Self {
        Self {
            comfyui_url: Some(s.comfyui_url.clone()),
            default_width: Some(s.default_width),
            default_height: Some(s.default_height),
            default_count: Some(s.default_count),
            default_steps: Some(s.default_steps),
            default_cfg: Some(s.default_cfg),
            default_model_id: Some(s.default_model_id.clone()),
        }
    }
}

/// Load settings, substituting defaults for missing/absent fields.
/// A missing file is not an error — first-time use returns defaults.
pub fn load(project_config_dir: &Path) -> Result<ImageSettings> {
    let path = settings_path(project_config_dir);
    let mut out = ImageSettings::default();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let text =
        std::str::from_utf8(&bytes).with_context(|| format!("utf-8 {}", path.display()))?;
    let parsed: OnDisk =
        toml::from_str(text).with_context(|| format!("parse {}", path.display()))?;
    parsed.merge_into(&mut out);
    Ok(out)
}

/// Atomically write the full settings struct. Creates the `image/`
/// subdir on first save.
pub fn save(project_config_dir: &Path, settings: &ImageSettings) -> Result<()> {
    let path = settings_path(project_config_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let on_disk = OnDisk::from_settings(settings);
    let text =
        toml::to_string_pretty(&on_disk).context("serialize ImageSettings to toml")?;
    lutin_keypair::write_atomic(&path, text.as_bytes(), 0o600)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
