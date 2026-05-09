//! Helpers shared across workflow engine binaries.
//!
//! Each workflow runs as its own subprocess (one per session) with its
//! own typed protocol. This crate absorbs the parts that don't vary
//! across workflows: building an `Agent` from a persona + settings,
//! and persisting per-session state to disk.
//!
//! Modules are independent — workflows can use [`agent`] without
//! adopting [`state`], and vice versa.

pub mod agent;
pub mod compaction;
pub mod prompt;
pub mod state;
pub mod summary;
