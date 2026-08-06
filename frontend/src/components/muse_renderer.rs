//! Rendering support for Muse sessions.
//!
//! Muse's journal is a third protocol shape (see `docs/MUSE_SUPPORT.md`),
//! and its distinguishing feature for the view is that work arrives as a
//! **task tree** rather than tool-use blocks. [`task_tree`] holds the pure
//! reducer that turns classified records into that tree; the records reach
//! it from two channels (persisted output for structure, the ephemeral
//! side-channel for live status) which the session view interleaves.

pub mod task_tree;

pub use task_tree::{TaskNode, TaskState, TaskTree, ToolOutcome};
