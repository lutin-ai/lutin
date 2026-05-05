//! Wire-facing identifier newtypes shared across tiers.
//!
//! Owns the `identifier!` macro and the three identifier types that
//! every wire boundary parses through (`Slug`, `WorkflowId`,
//! `SessionId`). Lives below `lutin-auth` and `lutin-project-protocol`
//! so both can depend on it without forming a cycle.

/// Define a `String`-backed identifier newtype with a max length and
/// the `[A-Za-z0-9_-]` charset. Generates `parse`, `as_str`, `Display`,
/// `Serialize`, parse-on-`Deserialize`, plus a sibling error enum.
#[macro_export]
macro_rules! identifier {
    ($name:ident, $err:ident, $max:expr, $what:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, ::serde::Serialize)]
        pub struct $name(String);

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum $err {
            Empty,
            TooLong,
            BadChar(char),
            BadFirstChar(char),
        }

        impl ::std::fmt::Display for $err {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match self {
                    $err::Empty => write!(f, "{} must not be empty", $what),
                    $err::TooLong => write!(f, "{} exceeds {} chars", $what, $max),
                    $err::BadChar(c) => write!(f, "{} contains invalid char: {:?}", $what, c),
                    $err::BadFirstChar(_) => {
                        write!(f, "{} must start with a letter or digit", $what)
                    }
                }
            }
        }

        impl ::std::error::Error for $err {}

        impl $name {
            pub fn parse(s: impl Into<String>) -> ::std::result::Result<Self, $err> {
                let s = s.into();
                if s.is_empty() {
                    return Err($err::Empty);
                }
                if s.len() > $max {
                    return Err($err::TooLong);
                }
                let first = s.chars().next().expect("non-empty checked above");
                if !first.is_ascii_alphanumeric() {
                    return Err($err::BadFirstChar(first));
                }
                if let Some(c) = s
                    .chars()
                    .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
                {
                    return Err($err::BadChar(c));
                }
                Ok($name(s))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D>(d: D) -> ::std::result::Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                let s = String::deserialize(d)?;
                $name::parse(s).map_err(::serde::de::Error::custom)
            }
        }
    };
}

// Slug: project identifier. ASCII alnum + `-`/`_`, 1..=64 chars.
identifier!(Slug, SlugError, 64, "slug");

// WorkflowId: workflow installed at `.lutin/workflows/<id>/`.
identifier!(WorkflowId, WorkflowIdError, 64, "workflow id");

// SessionId: opaque to clients; minted by the project supervisor.
identifier!(SessionId, SessionIdError, 64, "session id");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_leading_dash() {
        assert_eq!(Slug::parse("-foo"), Err(SlugError::BadFirstChar('-')));
    }

    #[test]
    fn rejects_leading_underscore() {
        assert_eq!(Slug::parse("_foo"), Err(SlugError::BadFirstChar('_')));
    }

    #[test]
    fn allows_dash_and_underscore_mid_and_end() {
        assert!(Slug::parse("foo-bar_baz").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(Slug::parse(""), Err(SlugError::Empty));
    }

    #[test]
    fn rejects_too_long() {
        let s: String = std::iter::repeat('a').take(65).collect();
        assert_eq!(Slug::parse(s), Err(SlugError::TooLong));
    }

    #[test]
    fn rejects_bad_char() {
        assert_eq!(Slug::parse("foo/bar"), Err(SlugError::BadChar('/')));
    }
}
