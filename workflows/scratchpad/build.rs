use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    embed_dir(
        manifest.join("../../principles"),
        out_dir.join("principles_data.rs"),
        "PRINCIPLES_RAW",
    );
    embed_dir(
        manifest.join("../../personas"),
        out_dir.join("personas_data.rs"),
        "PERSONAS_RAW",
    );
}

fn embed_dir(src_dir: PathBuf, out_path: PathBuf, static_name: &str) {
    println!("cargo:rerun-if-changed={}", src_dir.display());

    let mut entries: Vec<(String, String)> = Vec::new();
    let read =
        fs::read_dir(&src_dir).unwrap_or_else(|e| panic!("read {}: {e}", src_dir.display()));
    for entry in read {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("filename");
        let body =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        entries.push((name.to_string(), body));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut src = format!("pub(crate) static {static_name}: &[(&str, &str)] = &[\n");
    for (name, body) in &entries {
        src.push_str(&format!("    ({name:?}, {body:?}),\n"));
    }
    src.push_str("];\n");
    fs::write(&out_path, src)
        .unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
}
