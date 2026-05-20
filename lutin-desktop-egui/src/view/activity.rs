//! Activity / notifications feed (stub).
//!
//! Today the chrome surfaces only the most recent notification + last
//! error in the top bar (`App::notification`, `App::last_error`).
//! `../lutin/desktop/src/view/activity.rs` shows a chronological feed
//! of all notifications, errors, and lifecycle events — useful when an
//! event is missed because something more recent overwrote it.
//!
//! When implementing: maintain a bounded ring buffer in `App`, push on
//! every `last_error` / `notification` update + project/session
//! lifecycle broadcast, render here from oldest to newest.
