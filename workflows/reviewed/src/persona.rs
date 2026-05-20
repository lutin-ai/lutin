use std::sync::LazyLock;

use lutin_entities::Persona;

include!(concat!(env!("OUT_DIR"), "/personas_data.rs"));

pub static PERSONAS: LazyLock<Vec<Persona>> = LazyLock::new(|| {
    PERSONAS_RAW
        .iter()
        .map(|(name, body)| {
            let mut p: Persona = toml::from_str(body)
                .unwrap_or_else(|e| panic!("parse persona `{name}`: {e}"));
            p.name = (*name).into();
            p
        })
        .collect()
});

pub fn find(name: &str) -> Option<&'static Persona> {
    PERSONAS.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personas_parse_nonempty_unique() {
        let ps = &*PERSONAS;
        assert!(!ps.is_empty(), "no personas bundled");
        let mut seen = std::collections::HashSet::new();
        for p in ps {
            assert!(seen.insert(p.name.clone()), "duplicate persona: {}", p.name);
            assert!(!p.system_prompt.is_empty(), "{}: empty system_prompt", p.name);
        }
    }
}
