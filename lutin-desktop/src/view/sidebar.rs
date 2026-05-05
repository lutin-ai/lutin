//! Sidebar views (left + right).
//!
//! Will own:
//!   * Left sidebar — project list + create-project form. Maps to
//!     `../lutin/desktop/src/view/projects.rs` + `nav.rs`. Also hosts
//!     the workflow's `Slot::LeftSidebar` rendering.
//!   * Right sidebar — project details fallback when the active
//!     workflow doesn't request its own right sidebar.
//!
//! Currently still rendered inline by `draw_left_sidebar` /
//! `draw_right_sidebar` in `crate::app`.
