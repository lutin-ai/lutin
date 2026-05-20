//! Chat-style leaf widgets — error blocks, collapsible thinking/reasoning,
//! tool-call cards, file-edit diffs, collapsed previews, header summaries.
//!
//! Each widget takes raw inputs (strings, JSON values, status enums). No
//! message-data type is provided; the caller maps their own data into the
//! widget signatures. Pull in what you want.

pub mod error;
pub mod thinking;
pub mod tool_call;
pub mod tool_diff;
pub mod tool_preview;
pub mod tool_summary;
