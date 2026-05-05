//! Lutin desktop chrome — egui app that talks to a running
//! `lutin-control-panel` and (in C2+) dlopens workflow cdylibs to
//! render their per-scope UIs into the four fixed slots
//! (`LeftSidebar`, `TopBar`, `RightSidebar`, `Main`).

mod app;
mod bridge;
mod cp;
mod loader;
mod proj;
pub mod settings;
mod view;

pub use app::App;
pub use settings::{ConnectionProfile, DesktopSettings};
pub use cp::{
    CpClient, CpCommand, CpConfig, CpUpdate, RequestId, Token, TokenError, run as run_cp_worker,
};
pub use loader::{LoadError, WorkflowCache, WorkflowLibrary, workflow_so_path};
