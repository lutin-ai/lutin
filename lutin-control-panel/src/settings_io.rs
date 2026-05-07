//! Provider-list read/write against the global `settings.toml`.
//!
//! The on-disk file may contain unrelated sections (`[chat]`, `[tts]`,
//! `[whisper]`, …) that workflow engines read directly via
//! `lutin_settings::Settings::load`. To avoid clobbering them when the
//! desktop chrome rewrites the providers table, we round-trip through
//! `toml::Value`: parse the file, swap the `providers` array, write
//! the rest back unchanged. The first time providers are saved, the
//! file is created if absent.

use std::path::{Path, PathBuf};

use lutin_control_protocol::ProviderConfig;

const SETTINGS_FILE: &str = "settings.toml";

fn settings_path(global_config_dir: &Path) -> PathBuf {
    global_config_dir.join(SETTINGS_FILE)
}

pub fn read_providers(global_config_dir: &Path) -> Result<Vec<ProviderConfig>, String> {
    let path = settings_path(global_config_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let root: toml::Value = toml::from_str(&text)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    let Some(arr) = root.get("providers").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let pc: ProviderConfig = entry
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| format!("providers[{i}]: {e}"))?;
        out.push(pc);
    }
    Ok(out)
}

pub fn write_providers(
    global_config_dir: &Path,
    providers: &[ProviderConfig],
) -> Result<(), String> {
    let path = settings_path(global_config_dir);

    // Load any existing structure so we preserve sibling sections.
    // toml::Value preserves order and unknown keys verbatim, which is
    // what we want for a non-destructive write.
    let mut root: toml::Value = match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s)
            .map_err(|e| format!("parse {}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(toml::map::Map::new())
        }
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };

    let table = root
        .as_table_mut()
        .ok_or_else(|| format!("{} is not a table", path.display()))?;
    // Build each provider as a toml table manually so None fields are
    // omitted on disk. We can't lean on `#[serde(skip_serializing_if =
    // "Option::is_none")]` on `ProviderConfig` because that struct also
    // travels over the wire via postcard, which has a fixed schema and
    // can't tolerate a serializer dropping fields.
    let arr: Vec<toml::Value> = providers
        .iter()
        .map(|p| {
            let mut t = toml::map::Map::new();
            t.insert("name".into(), toml::Value::String(p.name.clone()));
            let kind = toml::Value::try_from(p.kind)
                .map_err(|e| format!("encode provider {}: {e}", p.name))?;
            t.insert("kind".into(), kind);
            if let Some(v) = &p.api_key {
                t.insert("api_key".into(), toml::Value::String(v.clone()));
            }
            if let Some(v) = &p.api_key_env {
                t.insert("api_key_env".into(), toml::Value::String(v.clone()));
            }
            if let Some(v) = &p.base_url {
                t.insert("base_url".into(), toml::Value::String(v.clone()));
            }
            t.insert("use_oauth".into(), toml::Value::Boolean(p.use_oauth));
            Ok::<_, String>(toml::Value::Table(t))
        })
        .collect::<Result<_, _>>()?;
    table.insert("providers".into(), toml::Value::Array(arr));

    // Serialize. We deliberately write back the merged Value rather
    // than the typed `Settings` so unknown keys (forward-compat fields,
    // user comments lost — toml::Value drops comments either way) don't
    // get dropped on save.
    let serialized = toml::to_string_pretty(&root)
        .map_err(|e| format!("serialize {}: {e}", path.display()))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, serialized).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

