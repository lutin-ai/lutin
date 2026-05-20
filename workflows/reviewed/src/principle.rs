use std::sync::LazyLock;

use crate::types::Principle;

include!(concat!(env!("OUT_DIR"), "/principles_data.rs"));

pub static PRINCIPLES: LazyLock<Vec<Principle>> = LazyLock::new(|| {
    PRINCIPLES_RAW
        .iter()
        .map(|(name, body)| {
            let mut p: Principle = toml::from_str(body)
                .unwrap_or_else(|e| panic!("parse principle `{name}`: {e}"));
            p.name = (*name).into();
            p
        })
        .collect()
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn principles_parse_nonempty_unique() {
        let ps = &*PRINCIPLES;
        assert!(!ps.is_empty(), "no principles bundled");
        let mut seen = std::collections::HashSet::new();
        for p in ps {
            assert!(seen.insert(p.name.clone()), "duplicate principle: {}", p.name);
            assert!(!p.title.is_empty(), "{}: empty title", p.name);
            assert!(!p.description.is_empty(), "{}: empty description", p.name);
        }
    }
}
