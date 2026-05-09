//! Seed the global config tier with in-tree defaults on launch.
//!
//! Each entry in [`DEFAULT_PERSONAS`] / [`DEFAULT_PRINCIPLES`] is
//! written into `<global>/<dir>/<name>.toml` if absent. User edits
//! to an existing file are preserved (we never overwrite). Deletions
//! are NOT remembered — a deleted default reappears on next launch.
//! If we want delete-stickiness later, the plan is a marker file
//! plus a "Restore defaults" action in settings.

use std::io;
use std::path::Path;
use tracing::info;

const DEFAULT_PERSONAS: &[(&str, &str)] = &[
    (
        "assistant",
        include_str!("../../personas/assistant.toml"),
    ),
    (
        "coder",
        include_str!("../../personas/coder.toml"),
    ),
    (
        "orchestrator",
        include_str!("../../personas/orchestrator.toml"),
    ),
    (
        "researcher",
        include_str!("../../personas/researcher.toml"),
    ),
    (
        "reviewer",
        include_str!("../../personas/reviewer.toml"),
    ),
];

/// Default principles seeded into `<global>/principles/`. Picked up
/// by the principled workflow's `Principle::list` via the same
/// `Resolver` the personas loader uses, so a fresh install ships
/// with the full software-principles library — every installed
/// principle is always active for its declared `applies_to` tools.
/// Editing the seeded copy is the supported way to tune them; the
/// in-tree source of truth is here.
const DEFAULT_PRINCIPLES: &[(&str, &str)] = &[
    (
        "avoid-boolean-parameters",
        include_str!("../../principles/avoid-boolean-parameters.toml"),
    ),
    (
        "avoid-global-mutable-state",
        include_str!("../../principles/avoid-global-mutable-state.toml"),
    ),
    (
        "avoid-locks-and-arc-in-rust",
        include_str!("../../principles/avoid-locks-and-arc-in-rust.toml"),
    ),
    (
        "avoid-nullable-sprawl",
        include_str!("../../principles/avoid-nullable-sprawl.toml"),
    ),
    (
        "composition-over-inheritance",
        include_str!("../../principles/composition-over-inheritance.toml"),
    ),
    (
        "delete-dead-code",
        include_str!("../../principles/delete-dead-code.toml"),
    ),
    (
        "design-data-layout-first",
        include_str!("../../principles/design-data-layout-first.toml"),
    ),
    (
        "dont-catch-what-you-cant-handle",
        include_str!("../../principles/dont-catch-what-you-cant-handle.toml"),
    ),
    (
        "errors-as-values",
        include_str!("../../principles/errors-as-values.toml"),
    ),
    (
        "guard-clauses-over-nesting",
        include_str!("../../principles/guard-clauses-over-nesting.toml"),
    ),
    (
        "inline-single-use-helpers",
        include_str!("../../principles/inline-single-use-helpers.toml"),
    ),
    (
        "iterate-dont-recurse",
        include_str!("../../principles/iterate-dont-recurse.toml"),
    ),
    (
        "make-illegal-states-unrepresentable",
        include_str!("../../principles/make-illegal-states-unrepresentable.toml"),
    ),
    (
        "message-passing-over-shared-state",
        include_str!("../../principles/message-passing-over-shared-state.toml"),
    ),
    (
        "name-things-for-what-they-are",
        include_str!("../../principles/name-things-for-what-they-are.toml"),
    ),
    (
        "newtype-domain-primitives",
        include_str!("../../principles/newtype-domain-primitives.toml"),
    ),
    (
        "no-premature-abstraction",
        include_str!("../../principles/no-premature-abstraction.toml"),
    ),
    (
        "parse-dont-validate",
        include_str!("../../principles/parse-dont-validate.toml"),
    ),
    (
        "pass-by-reference",
        include_str!("../../principles/pass-by-reference.toml"),
    ),
    (
        "prefer-immutability",
        include_str!("../../principles/prefer-immutability.toml"),
    ),
    (
        "prefer-pure-functions",
        include_str!("../../principles/prefer-pure-functions.toml"),
    ),
    (
        "prefer-slices-over-owned-collections",
        include_str!("../../principles/prefer-slices-over-owned-collections.toml"),
    ),
    (
        "prefer-stack-over-heap",
        include_str!("../../principles/prefer-stack-over-heap.toml"),
    ),
    (
        "prefer-vec-over-hashmap",
        include_str!("../../principles/prefer-vec-over-hashmap.toml"),
    ),
    (
        "scope-mutation-tightly",
        include_str!("../../principles/scope-mutation-tightly.toml"),
    ),
    (
        "shell-no-dangerous-commands",
        include_str!("../../principles/shell-no-dangerous-commands.toml"),
    ),
    (
        "single-source-of-truth",
        include_str!("../../principles/single-source-of-truth.toml"),
    ),
    (
        "split-functions-by-responsibility",
        include_str!("../../principles/split-functions-by-responsibility.toml"),
    ),
    (
        "test-behavior-not-implementation",
        include_str!("../../principles/test-behavior-not-implementation.toml"),
    ),
    (
        "validate-at-boundaries",
        include_str!("../../principles/validate-at-boundaries.toml"),
    ),
    (
        "write-for-the-reader",
        include_str!("../../principles/write-for-the-reader.toml"),
    ),
];

pub fn seed(global_config_dir: &Path) -> io::Result<()> {
    let personas_dir = global_config_dir.join("personas");
    for (name, body) in DEFAULT_PERSONAS {
        let path = personas_dir.join(format!("{name}.toml"));
        if path.exists() {
            continue;
        }
        info!(path = %path.display(), "seeding default persona");
        std::fs::create_dir_all(&personas_dir)?;
        std::fs::write(&path, body)?;
    }

    let principles_dir = global_config_dir.join("principles");
    for (name, body) in DEFAULT_PRINCIPLES {
        let path = principles_dir.join(format!("{name}.toml"));
        if path.exists() {
            continue;
        }
        info!(path = %path.display(), "seeding default principle");
        std::fs::create_dir_all(&principles_dir)?;
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
        for (name, _) in DEFAULT_PRINCIPLES {
            assert!(
                tmp.path().join(format!("principles/{name}.toml")).is_file(),
                "missing seeded principle: {name}"
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
