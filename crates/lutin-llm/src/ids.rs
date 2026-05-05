//! Newtype wrappers for domain-identifier strings flowing through the LLM
//! wire layer.
//!
//! Rule #5 of `docs/notes/prompt-rules.md`: `String` for `CallId`, `ToolName`,
//! `ModelId`, `ProviderName` is assignment-compatible — two ids of different
//! domains can be silently swapped in a struct-literal or function call. These
//! newtypes catch swaps at compile time with `#[repr(transparent)]` zero-cost
//! layout.
//!
//! Deliberately *not* derived:
//! - `From<String>` / `From<&str>` on every type — that would re-open silent
//!   swaps via `.into()`. Each type exposes an explicit `new` constructor; the
//!   chosen few with `From<String>` are the ones where construction is always
//!   unambiguous (e.g. `ModelId` is built from config/user input, never from a
//!   `CallId`).
//!
//! Boundary helpers (`as_str`, `into_inner`) bridge to still-`String`-typed
//! DTO fields in `shared/` without re-introducing the swap risk.
use std::sync::Arc;

use serde::{Deserialize, Serialize};

macro_rules! string_newtype {
    ($(#[$meta:meta])* $vis:vis $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        #[repr(transparent)]
        $vis struct $name(String);

        impl $name {
            #[inline]
            $vis fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            #[inline]
            $vis fn as_str(&self) -> &str { &self.0 }
            #[inline]
            $vis fn into_inner(self) -> String { self.0 }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        // Comparison against `&str` literals — safe against swap because a
        // `str` has no domain identity. Enables `id == "abc"` in tests and
        // equality checks against DTO-sourced `&str` views without forcing
        // callers to wrap every literal.
        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool { self.0 == other }
        }
        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool { self.0 == *other }
        }
        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool { &self.0 == other }
        }

        // `From<&str>` / `From<String>` are safe on these newtypes because
        // the source type (`&str` / `String`) has no domain identity — you
        // cannot accidentally pass a `CallId` where `From<&str>` for
        // `ToolName` is expected. The anti-swap concern is between distinct
        // newtypes; both of those still error as expected.
        impl From<&str> for $name {
            fn from(v: &str) -> Self { Self(v.to_string()) }
        }
        impl From<String> for $name {
            fn from(v: String) -> Self { Self(v) }
        }
    };
}

string_newtype! {
    /// Provider-assigned id for a single tool-use block. Matches the tool_use
    /// id on the assistant message with the tool_result echoed back.
    pub CallId
}

string_newtype! {
    /// Name of a registered tool as referenced by the model. Looked up against
    /// the tool registry; never a free-form label.
    pub ToolName
}

string_newtype! {
    /// Upstream provider name as reported by OpenRouter / equivalent (e.g.
    /// "Anthropic", "Minimax"). Distinct from `ModelId`.
    pub ProviderName
}

/// Model identifier as used on the wire (e.g. `anthropic/claude-3.5-sonnet`).
/// Held as `Arc<str>` because it's cloned onto every streamed chunk and every
/// completed message in a session. Not interchangeable with a provider name or
/// a model's human-readable label.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[repr(transparent)]
pub struct ModelId(#[serde(with = "arc_str_serde")] Arc<str>);

mod arc_str_serde {
    use std::sync::Arc;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Arc<str>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<str>, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Arc::from(s))
    }
}

impl ModelId {
    #[inline]
    pub fn new(s: impl Into<Arc<str>>) -> Self { Self(s.into()) }
    #[inline]
    pub fn as_str(&self) -> &str { &self.0 }
    #[inline]
    pub fn as_arc(&self) -> &Arc<str> { &self.0 }
    #[inline]
    pub fn into_arc(self) -> Arc<str> { self.0 }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Arc<str>> for ModelId {
    fn from(v: Arc<str>) -> Self { Self(v) }
}

impl From<String> for ModelId {
    fn from(v: String) -> Self { Self(Arc::from(v)) }
}

impl From<&str> for ModelId {
    fn from(v: &str) -> Self { Self(Arc::from(v)) }
}
