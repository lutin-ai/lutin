//! Generate the `DEFAULT_PERSONAS` / `DEFAULT_PRINCIPLES` slices used
//! by `defaults.rs` to seed the global config tier on first launch.
//!
//! Each `<repo>/personas/*.toml` and `<repo>/principles/*.toml` file
//! is emitted as a `(stem, include_str!("<absolute path>"))` tuple,
//! sorted by stem so output is deterministic. New files are picked up
//! on the next build with no code change.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest.parent().expect("manifest dir has a parent");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dest = out_dir.join("seed_defaults.rs");

    let personas = collect_toml(&repo_root.join("personas"));
    let principles = collect_toml(&repo_root.join("principles"));

    let mut buf = String::new();
    emit_slice(&mut buf, "DEFAULT_PERSONAS", &personas);
    emit_slice(&mut buf, "DEFAULT_PRINCIPLES", &principles);
    fs::write(&dest, buf).expect("write seed_defaults.rs");

    // Re-run when files are added/removed/changed in either dir.
    println!("cargo:rerun-if-changed={}", repo_root.join("personas").display());
    println!("cargo:rerun-if-changed={}", repo_root.join("principles").display());
    for (_, path) in personas.iter().chain(principles.iter()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn collect_toml(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_owned();
            Some((stem, path))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn emit_slice(buf: &mut String, name: &str, entries: &[(String, PathBuf)]) {
    buf.push_str(&format!(
        "pub(crate) const {name}: &[(&str, &str)] = &[\n"
    ));
    for (stem, path) in entries {
        // Absolute path so `include_str!` resolves regardless of where
        // the generated file lands under OUT_DIR.
        buf.push_str(&format!(
            "    ({:?}, include_str!({:?})),\n",
            stem,
            path.display().to_string()
        ));
    }
    buf.push_str("];\n\n");
}
