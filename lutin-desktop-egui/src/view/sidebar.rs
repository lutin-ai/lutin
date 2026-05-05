//! Sidebar views (left + right).
//!
//! Will own:
//!   * Left sidebar — project list + create-project form, plus the
//!     active project's session list ("+ New" button + per-session
//!     entries).
//!   * Right sidebar — project details panel.
//!
//! Currently still rendered inline by `draw_left_sidebar` /
//! `draw_project_sessions_panel` / `draw_right_sidebar` in
//! `crate::app`.
