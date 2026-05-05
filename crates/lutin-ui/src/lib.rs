//! Lutin UI — egui component library.
//!
//! Library, not framework: no app trait, no run loop, no required state container.
//! Pull in what you want, ignore the rest.

pub mod font;
pub mod markdown;
pub mod theme;
pub mod widget;

pub mod prelude {
    pub use crate::markdown::Markdown;
    pub use crate::theme::{Theme, dark::dark, set_theme, theme};
}
