//! Chrome view layer.
//!
//! Each submodule renders one slice of the desktop chrome (top bar,
//! sidebar, main pane, dedicated views like Settings). The `App` struct
//! in `crate::app` owns state and dispatches to these modules.
//!
//! Modules are added incrementally — initial extraction lives next to
//! the rest of `app.rs`. Stubs here mark planned splits and lay out the
//! shape we're moving toward (`../lutin/desktop/src/view/` for the
//! reference layout we're loosely mirroring).

pub mod activity;
pub mod main;
pub mod secrets;
pub mod settings;
pub mod sidebar;
pub mod top_bar;
