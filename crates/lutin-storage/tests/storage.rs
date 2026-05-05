use std::path::PathBuf;

use lutin_storage::{BlobHash, ResolvedEntity, Resolver, Scope, Store, StoreLayout};
use serde::{Deserialize, Serialize};
use tempfile::tempdir;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Item {
    id: u32,
    name: String,
}

fn open_store(dir: &std::path::Path) -> Store {
    Store::open(StoreLayout::Combined(dir.into())).unwrap()
}

#[test]
fn kv_put_get_delete_list() {
    let dir = tempdir().unwrap();
    let store = open_store(dir.path());
    let kv = store.kv::<Item>("items").unwrap();
    let a = Item { id: 1, name: "a".into() };
    let b = Item { id: 2, name: "b".into() };
    kv.put("a", &a).unwrap();
    kv.put("b", &b).unwrap();
    assert_eq!(kv.get("a").unwrap(), Some(a.clone()));
    assert_eq!(kv.get("missing").unwrap(), None);
    assert_eq!(kv.iter().count(), 2);
    assert!(kv.delete("a").unwrap());
    assert!(!kv.delete("a").unwrap());
    assert_eq!(kv.get("a").unwrap(), None);
}

#[test]
fn kv_cas() {
    let dir = tempdir().unwrap();
    let store = open_store(dir.path());
    let kv = store.kv::<Item>("items").unwrap();
    let a = Item { id: 1, name: "a".into() };
    let b = Item { id: 1, name: "b".into() };
    // Insert when missing.
    assert!(kv.cas("k", None, Some(&a)).unwrap());
    assert!(!kv.cas("k", None, Some(&a)).unwrap()); // already exists
    // Replace when matches.
    assert!(kv.cas("k", Some(&a), Some(&b)).unwrap());
    // Mismatch fails.
    assert!(!kv.cas("k", Some(&a), Some(&b)).unwrap());
    // Delete when matches.
    assert!(kv.cas("k", Some(&b), None).unwrap());
    assert_eq!(kv.get("k").unwrap(), None);
}

#[test]
fn transcript_append_iter_truncate() {
    let dir = tempdir().unwrap();
    let store = open_store(dir.path());
    let xc = store.transcript("session-1").unwrap();
    assert_eq!(xc.last_seq().unwrap(), None);
    let s0 = xc.append(b"a").unwrap();
    let s1 = xc.append(b"b").unwrap();
    let s2 = xc.append(b"c").unwrap();
    assert!(s1 > s0 && s2 > s1);
    let all: Vec<_> = xc
        .iter_from(0)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(all.len(), 3);
    let from_after_s0: Vec<_> = xc
        .iter_from(s0 + 1)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(from_after_s0.len(), 2);
    let removed = xc.truncate_before(s2).unwrap();
    assert_eq!(removed, 2);
    let remaining: Vec<_> = xc
        .iter_from(0)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(remaining.len(), 1);
}

#[test]
fn transcript_typed_roundtrip() {
    let dir = tempdir().unwrap();
    let store = open_store(dir.path());
    let xc = store.transcript("typed").unwrap();
    let i = Item { id: 7, name: "x".into() };
    xc.append_typed(&i).unwrap();
    let out: Vec<(_, Item)> = xc
        .iter_typed(0)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(out[0].1, i);
}

#[test]
fn blob_put_get_dedupe() {
    let dir = tempdir().unwrap();
    let store = open_store(dir.path());
    let blobs = store.blobs();
    let h1 = blobs.put(b"hello").unwrap();
    let h2 = blobs.put(b"hello").unwrap();
    assert_eq!(h1, h2);
    let h3 = blobs.put(b"world").unwrap();
    assert_ne!(h1, h3);
    assert_eq!(blobs.get(&h1).unwrap(), b"hello");
    assert!(blobs.exists(&h1));
    assert!(blobs.delete(&h1).unwrap());
    assert!(!blobs.exists(&h1));
}

#[test]
fn blob_get_corrupt_detects_mismatch() {
    let dir = tempdir().unwrap();
    let store = open_store(dir.path());
    let blobs = store.blobs();
    let h = blobs.put(b"data").unwrap();
    // Corrupt by hand.
    let hex = h.to_hex();
    let (a, b) = hex.split_at(2);
    let path = dir.path().join("blobs").join(a).join(b);
    std::fs::write(&path, b"tampered").unwrap();
    let err = blobs.get(&h).unwrap_err();
    assert!(matches!(err, lutin_storage::StoreError::BlobHashMismatch));
}

#[test]
fn snapshots_write_latest_prune() {
    let dir = tempdir().unwrap();
    let store = open_store(dir.path());
    let snaps = store.snapshots("session").unwrap();
    let m1 = snaps.write_typed(10, &Item { id: 1, name: "a".into() }).unwrap();
    let _m2 = snaps.write_typed(20, &Item { id: 2, name: "b".into() }).unwrap();
    let m3 = snaps.write_typed(30, &Item { id: 3, name: "c".into() }).unwrap();
    let (latest_meta, latest): (_, Item) = snaps.latest_typed().unwrap().unwrap();
    assert_eq!(latest_meta.seq, 30);
    assert_eq!(latest.id, 3);
    assert_eq!(latest_meta.hash, m3.hash);
    let removed = snaps.prune_keeping(1).unwrap();
    assert_eq!(removed, 2);
    let refs = snaps.referenced_hashes().unwrap();
    assert_eq!(refs.len(), 1);
    // Old blob still on disk (no gc), oldest meta gone from index.
    assert!(store.blobs().exists(&m1.hash));
}

fn write(dir: &std::path::Path, rel: &str, contents: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, contents).unwrap();
}

#[test]
fn resolver_project_overrides_global() {
    let dir = tempdir().unwrap();
    let global = dir.path().join("global");
    let project = dir.path().join("project");
    write(&global, "personas/foo.toml", "global-foo");
    write(&project, "personas/foo.toml", "project-foo");
    write(&global, "personas/bar.toml", "global-bar");
    let r = Resolver::new(&global, Some(&project));
    let (scope, path) = r.find_file(&PathBuf::from("personas/foo.toml")).unwrap();
    assert_eq!(scope, Scope::Project);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "project-foo");
    let (scope, _) = r.find_file(&PathBuf::from("personas/bar.toml")).unwrap();
    assert_eq!(scope, Scope::Global);
    assert!(r.find_file(&PathBuf::from("personas/missing.toml")).is_none());
}

#[test]
fn resolver_list_entities_appends_both_tiers() {
    let dir = tempdir().unwrap();
    let global = dir.path().join("global");
    let project = dir.path().join("project");
    write(&global, "personas/alice.toml", "g");
    write(&global, "personas/bob.toml", "g");
    write(&project, "personas/alice.toml", "p"); // duplicate name across tiers
    write(&project, "personas/carol.toml", "p");
    let r = Resolver::new(&global, Some(&project));
    let mut all: Vec<ResolvedEntity> = r.list_entities("personas").unwrap();
    all.sort_by_key(|e| (e.scope, e.name.clone()));
    assert_eq!(all.len(), 4); // alice appears twice, scope-tagged
    let mut unique: Vec<ResolvedEntity> = r.list_entities_unique("personas").unwrap();
    unique.sort_by_key(|e| e.name.clone());
    assert_eq!(unique.len(), 3);
    let alice = unique.iter().find(|e| e.name == "alice").unwrap();
    assert_eq!(alice.scope, Scope::Project);
}

#[test]
fn blob_hash_known_value() {
    let h = BlobHash::new(b"");
    assert_eq!(
        h.to_hex(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
