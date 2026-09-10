//! Blank-string guard re-exported from `shared`.
//!
//! This crate already depends on `shared`, so the check lives there instead
//! of in a local copy (covered by `shared::strings` tests).

pub use shared::strings::is_non_blank;
