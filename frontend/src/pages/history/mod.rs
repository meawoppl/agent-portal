//! Session-history browser over the long-term archive (`/history`).
//!
//! Two pages: [`HistoryBrowserPage`] (stats strip, filters, session table)
//! and [`HistoryTranscriptPage`] (manifest header + the archived messages
//! rendered with the real portal renderers). All data comes from the
//! authenticated `/api/history/*` endpoints; the backend scopes visibility
//! (own + shared-with-me, everything for admins), so the UI never filters
//! for access — only for presentation.

mod browser;
mod fetch;
mod filters;
mod media_rewrite;
mod transcript;

pub use browser::HistoryBrowserPage;
pub use transcript::HistoryTranscriptPage;
