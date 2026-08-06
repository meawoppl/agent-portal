//! Muse Code agent backend for `session-lib`.
//!
//! Muse's headless interface (`muse exec --json`) is an **event-sourced
//! journal**, a third protocol shape alongside Claude's role-tagged
//! messages and Codex's thread/turn events. See `docs/MUSE_SUPPORT.md` for
//! the protocol comparison and the measured behaviors this crate relies on.
//!
//! - [`MuseAgent`] selects this backend for `Session`.
//! - [`classifier`] turns journal records into neutral `AgentOutput`s.
//! - [`io_task`] runs one child process per turn, keyed to a portal-minted
//!   session id.

pub mod agent;
pub mod classifier;
pub mod io_task;

pub use agent::MuseAgent;
pub use classifier::{classify_record, MuseClassifier};
