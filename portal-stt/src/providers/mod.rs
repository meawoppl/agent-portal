//! One module per speech-to-text vendor.
//!
//! Each exposes a struct with `from_env(&SttEnv)` and an inherent `transcribe`;
//! dispatch lives on [`crate::SttProvider`] so the set stays closed and
//! exhaustively matched.
//!
//! The vendors do not share a request shape — single-shot multipart (OpenAI,
//! Azure), raw body (Deepgram, IBM), base64-in-JSON (Google, Simplismart), and
//! submit-then-poll jobs (AssemblyAI, Rev AI, Speechmatics, AWS) — which is why
//! the common parts live in `crate::http` and `crate::poll` rather than in a
//! shared base type.

pub(crate) mod assemblyai;
pub(crate) mod aws;
pub(crate) mod azure;
pub(crate) mod deepgram;
pub(crate) mod google;
pub(crate) mod ibm;
pub(crate) mod openai;
pub(crate) mod revai;
pub(crate) mod simplismart;
pub(crate) mod speechmatics;
