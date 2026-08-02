//! One module per speech-to-text vendor.
//!
//! Each provides a struct with `new(..)` and an inherent `transcribe`; the
//! dispatch lives on [`crate::SttProvider`] so the set stays closed and
//! exhaustively matched.

pub(crate) mod deepgram;
pub(crate) mod openai;
