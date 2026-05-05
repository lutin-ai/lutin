//! Main-pane view.
//!
//! Will own:
//!   * Welcome screen when no project is selected.
//!   * Session tabs + the active session's `WorkflowSessionUi` render
//!     (workflow cdylibs only paint into the Main pane).
//!
//! Currently still rendered inline by `draw_main` and
//! `draw_session_tabs` in `crate::app`.
