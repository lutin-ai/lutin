use std::sync::LazyLock;

use crate::types::Principle;

include!(concat!(env!("OUT_DIR"), "/principles_data.rs"));

pub static PRINCIPLES: LazyLock<Vec<Principle>> = LazyLock::new(|| assemble(""));

/// Reassemble the filesystem hierarchy embedded as `/`-joined names into
/// a tree. `prefix` is the parent path; a node is a direct child of it
/// when stripping `prefix/` leaves a name with no further `/`.
fn assemble(prefix: &str) -> Vec<Principle> {
    let mut out = Vec::new();
    for (name, body) in PRINCIPLES_RAW {
        let Some(rest) = child_segment(name, prefix) else {
            continue;
        };
        if rest.contains('/') {
            continue;
        }
        let mut p: Principle = toml::from_str(body)
            .unwrap_or_else(|e| panic!("parse principle `{name}`: {e}"));
        p.name = (*name).into();
        p.children = assemble(name);
        out.push(p);
    }
    out.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.name.cmp(&b.name)));
    out
}

fn child_segment<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(name);
    }
    name.strip_prefix(prefix)?.strip_prefix('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten<'a>(ps: &'a [Principle], out: &mut Vec<&'a Principle>) {
        for p in ps {
            out.push(p);
            flatten(&p.children, out);
        }
    }

    #[test]
    fn principles_parse_nonempty_unique() {
        assert!(!PRINCIPLES.is_empty(), "no principles bundled");
        let mut all = Vec::new();
        flatten(&PRINCIPLES, &mut all);
        let mut seen = std::collections::HashSet::new();
        for p in all {
            assert!(seen.insert(p.name.clone()), "duplicate principle: {}", p.name);
            assert!(!p.title.is_empty(), "{}: empty title", p.name);
            assert!(!p.description.is_empty(), "{}: empty description", p.name);
        }
    }
}
