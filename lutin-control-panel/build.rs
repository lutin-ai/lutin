//! Stages the in-tree default chat workflow + the lutin-* SDK crates it
//! depends on into `OUT_DIR/embedded_chat/` and `OUT_DIR/embedded_vendor/`
//! so `include_dir!` can pick them up without dragging the workflow's
//! 2 GB+ `target/` build cache into the binary.
//!
//! On boot, the control-panel re-stages `<global>/.lutin/vendor/` from
//! the embedded payload wholesale (so SDK upgrades flow in on every CP
//! release). The seeded `<global>/.lutin/workflows/chat/` references its
//! deps via `path = "../../vendor/<crate>"`, so cargo resolves them to
//! the staged vendor tree at build time. Both Cargo.tomls are rewritten
//! here at stage time:
//!
//! * `chat/Cargo.toml`: `path = "../../crates/<crate>"` →
//!   `path = "../../vendor/<crate>"`.
//! * each vendored crate's `Cargo.toml`: `*.workspace = true` is
//!   inlined to literal values pulled from the workspace root, and an
//!   empty `[workspace]` table is appended so cargo treats the staged
//!   copy as its own workspace root.
//!
//! Vendor crate set + workspace-package values are derived from the
//! checked-in Cargo.tomls (chat workflow + workspace root + each
//! lutin-* crate for transitive closure), so adding a new lutin dep
//! to the workflow is a one-line change in `workflows/chat/Cargo.toml`
//! and nothing here.
//!
//! `seed_version.txt` is an FNV-1a hash of the staged payload —
//! `defaults.rs` compares it against a marker on disk to decide when
//! to clobber the seeded chat workflow's `src/`. FNV-1a is stable
//! across Rust toolchains (unlike `std::hash::DefaultHasher`), so a
//! rebuild on a different rustc doesn't trigger a spurious re-seed.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Top-level entries under `workflows/chat/` that ship in the binary.
/// Either filenames or subdirectory names; subdirs are copied
/// recursively. Anything not listed here (notably `target/` and
/// `Cargo.lock`) is excluded.
const STAGED_CHAT: &[&str] = &["Cargo.toml", "manifest.toml", "src"];

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("control-panel manifest has a parent")
        .to_path_buf();
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));

    let workspace_pkg = parse_workspace_package(&workspace_root.join("Cargo.toml"));
    let vendor_crates = transitive_lutin_closure(
        &workspace_root,
        &workspace_root.join("workflows/chat/Cargo.toml"),
    );
    let chat_dst = out_dir.join("embedded_chat");
    let vendor_dst = out_dir.join("embedded_vendor");
    for d in [&chat_dst, &vendor_dst] {
        if d.exists() {
            fs::remove_dir_all(d).expect("clear staging dir");
        }
        fs::create_dir_all(d).expect("create staging dir");
    }

    stage_chat(&workspace_root, &chat_dst);
    stage_vendor(&workspace_root, &vendor_dst, &vendor_crates, &workspace_pkg);

    let mut hasher = Fnv64::new();
    hash_dir(&chat_dst, &mut hasher);
    hash_dir(&vendor_dst, &mut hasher);
    let seed_hash = format!("{:016x}", hasher.finish());
    fs::write(out_dir.join("seed_version.txt"), seed_hash).expect("write seed_version.txt");

    // Workspace package values change rarely but should re-trigger
    // build.rs on edit so vendored Cargo.tomls stay in sync.
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("Cargo.toml").display()
    );
}

#[derive(Clone)]
struct WorkspacePackage {
    version: String,
    edition: String,
}

fn parse_workspace_package(workspace_cargo: &Path) -> WorkspacePackage {
    let raw = fs::read_to_string(workspace_cargo)
        .unwrap_or_else(|e| panic!("read {}: {e}", workspace_cargo.display()));
    let val: toml::Value = toml::from_str(&raw)
        .unwrap_or_else(|e| panic!("parse {}: {e}", workspace_cargo.display()));
    let pkg = val
        .get("workspace")
        .and_then(|v| v.get("package"))
        .unwrap_or_else(|| panic!("no [workspace.package] in {}", workspace_cargo.display()));
    WorkspacePackage {
        version: pkg
            .get("version")
            .and_then(|v| v.as_str())
            .expect("workspace.package.version")
            .to_string(),
        edition: pkg
            .get("edition")
            .and_then(|v| v.as_str())
            .expect("workspace.package.edition")
            .to_string(),
    }
}

/// Walk the dep graph starting from the chat workflow's lutin-* deps
/// and collect every transitively-reachable lutin-* crate. Returns a
/// sorted set so build output is deterministic.
fn transitive_lutin_closure(workspace_root: &Path, chat_cargo: &Path) -> Vec<String> {
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut queue = lutin_deps(chat_cargo);
    while let Some(name) = queue.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let nested_cargo = workspace_root.join("crates").join(&name).join("Cargo.toml");
        queue.extend(lutin_deps(&nested_cargo));
    }
    visited.into_iter().collect()
}

/// Pull dep keys whose name starts with `lutin-` from a Cargo.toml's
/// `[dependencies]` table. Ignores `[dev-dependencies]` and
/// `[build-dependencies]` (workflows don't need those at runtime in
/// the seeded copy).
fn lutin_deps(cargo_toml: &Path) -> Vec<String> {
    let raw = fs::read_to_string(cargo_toml)
        .unwrap_or_else(|e| panic!("read {}: {e}", cargo_toml.display()));
    let val: toml::Value = toml::from_str(&raw)
        .unwrap_or_else(|e| panic!("parse {}: {e}", cargo_toml.display()));
    let Some(deps) = val.get("dependencies").and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    deps.keys()
        .filter(|k| k.starts_with("lutin-"))
        .cloned()
        .collect()
}

fn stage_chat(workspace_root: &Path, dst: &Path) {
    let src = workspace_root.join("workflows/chat");
    for entry in STAGED_CHAT {
        let from = src.join(entry);
        let to = dst.join(entry);
        if *entry == "Cargo.toml" {
            let raw = fs::read_to_string(&from)
                .unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
            fs::write(&to, raw.replace("../../crates/", "../../vendor/"))
                .unwrap_or_else(|e| panic!("write {}: {e}", to.display()));
        } else {
            copy_recursive(&from, &to);
        }
        println!("cargo:rerun-if-changed={}", from.display());
    }
}

fn stage_vendor(
    workspace_root: &Path,
    dst: &Path,
    crates: &[String],
    pkg: &WorkspacePackage,
) {
    for crate_name in crates {
        let from = workspace_root.join("crates").join(crate_name);
        let to = dst.join(crate_name);
        fs::create_dir_all(&to).expect("create vendor crate dir");

        let cargo_from = from.join("Cargo.toml");
        let cargo_to = to.join("Cargo.toml");
        let raw = fs::read_to_string(&cargo_from)
            .unwrap_or_else(|e| panic!("read {}: {e}", cargo_from.display()));
        fs::write(&cargo_to, rewrite_vendor_cargo_toml(&raw, crates, pkg))
            .unwrap_or_else(|e| panic!("write {}: {e}", cargo_to.display()));

        let src_from = from.join("src");
        let src_to = to.join("src");
        copy_recursive(&src_from, &src_to);

        println!("cargo:rerun-if-changed={}", cargo_from.display());
        println!("cargo:rerun-if-changed={}", src_from.display());
    }
}

/// Each vendored crate becomes a standalone package (its own workspace
/// root). Two transformations:
///
/// * `<key>.workspace = true` → inline literal pulled from
///   `[workspace.package]` (only `version` and `edition` appear in
///   this codebase).
/// * `lutin-X = { workspace = true ... }` → `lutin-X = { path = "../lutin-X" ... }`.
///
/// Everything else (intra-crate `path = "../<dep>"`, version-pinned
/// deps, features) already works in the flat vendor layout. An empty
/// `[workspace]` table is appended so cargo doesn't try to walk up
/// into `<global>/.lutin/` looking for a parent workspace.
fn rewrite_vendor_cargo_toml(raw: &str, crates: &[String], pkg: &WorkspacePackage) -> String {
    let mut out = String::with_capacity(raw.len() + 32);
    for line in raw.split_inclusive('\n') {
        let rewritten = rewrite_vendor_line(line, crates, pkg);
        out.push_str(rewritten.as_deref().unwrap_or(line));
    }
    if !out.contains("\n[workspace]\n") && !out.starts_with("[workspace]\n") {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n[workspace]\n");
    }
    out
}

fn rewrite_vendor_line(
    line: &str,
    crates: &[String],
    pkg: &WorkspacePackage,
) -> Option<String> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("version.workspace = true") {
        return Some(line.replacen(
            "version.workspace = true",
            &format!("version = \"{}\"", pkg.version),
            1,
        ));
    }
    if trimmed.starts_with("edition.workspace = true") {
        return Some(line.replacen(
            "edition.workspace = true",
            &format!("edition = \"{}\"", pkg.edition),
            1,
        ));
    }
    for dep in crates {
        // Match either `lutin-X = { workspace = true ... }` or
        // `lutin-X.workspace = true`. The codebase uses both forms.
        let table_prefix = format!("{dep} = {{");
        let dotted_prefix = format!("{dep}.workspace = true");
        if trimmed.starts_with(&table_prefix) && line.contains("workspace = true") {
            return Some(line.replacen(
                "workspace = true",
                &format!("path = \"../{dep}\""),
                1,
            ));
        }
        if trimmed.starts_with(&dotted_prefix) {
            return Some(line.replacen(
                &dotted_prefix,
                &format!("{dep} = {{ path = \"../{dep}\" }}"),
                1,
            ));
        }
    }
    None
}

fn copy_recursive(from: &Path, to: &Path) {
    let ft = fs::symlink_metadata(from)
        .unwrap_or_else(|e| panic!("stat {}: {e}", from.display()))
        .file_type();
    if ft.is_dir() {
        fs::create_dir_all(to).expect("create staged subdir");
        for entry in fs::read_dir(from).expect("read staged dir") {
            let entry = entry.expect("staged dir entry");
            copy_recursive(&entry.path(), &to.join(entry.file_name()));
        }
    } else if ft.is_file() {
        fs::copy(from, to).expect("copy staged file");
    }
}

/// Hash a directory's contents (paths + bytes) into the running
/// hasher. Walk in sorted order so the hash is filesystem-iteration
/// independent.
fn hash_dir(dir: &Path, hasher: &mut Fnv64) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        hasher.update(entry.file_name().to_string_lossy().as_bytes());
        let ft = entry.file_type().expect("entry file type");
        if ft.is_dir() {
            hash_dir(&path, hasher);
        } else if ft.is_file() {
            let bytes = fs::read(&path).expect("read staged file");
            hasher.update(&bytes);
        }
    }
}

/// FNV-1a 64-bit. Stable across Rust versions (unlike
/// `std::hash::DefaultHasher`), which matters because the digest is
/// written to disk and compared on subsequent CP launches — a hasher
/// change would otherwise force a spurious re-seed.
struct Fnv64 {
    h: u64,
}

impl Fnv64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn new() -> Self {
        Self { h: Self::OFFSET }
    }

    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.h ^= b as u64;
            self.h = self.h.wrapping_mul(Self::PRIME);
        }
    }

    fn finish(&self) -> u64 {
        self.h
    }
}
