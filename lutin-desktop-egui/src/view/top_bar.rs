//! Top-bar view stub.
//!
//! Will own: connection-state pill, view-mode toggle (Projects /
//! Settings), notification + last-error display, and the active
//! project's workflow icon + display name (looked up from
//! `WorkflowInfo`, not from the cdylib).
//!
//! Currently still rendered inline by `draw_top_bar` in `crate::app`;
//! extract here once chrome state has stable accessors.
