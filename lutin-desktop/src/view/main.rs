//! Main-pane view.
//!
//! Will own:
//!   * Welcome screen when no project is selected.
//!   * Session tabs + workflow `Slot::Main` rendering for the active
//!     project.
//!   * Cargo build progress / failure log when a workflow is being
//!     compiled. Maps to `../lutin/desktop/src/view/main_window.rs` +
//!     `workflow_session.rs`.
//!
//! Currently still rendered inline by `draw_main`, `draw_session_tabs`,
//! and `draw_build_log` in `crate::app`.
