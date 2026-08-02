//! HTTP plumbing shared by every provider.

use crate::SttError;

/// How much of an error body to keep. Enough to identify the fault, short
/// enough that a provider returning an HTML error page doesn't flood the log.
const MAX_ERROR_BODY: usize = 400;

/// Turn a non-2xx response into [`SttError::Provider`], preserving the status
/// and a bounded prefix of the body.
///
/// Every provider funnels through this so a misconfiguration reads the same
/// regardless of vendor, and so no provider forgets to check the status and
/// then fails confusingly at JSON decoding instead.
pub(crate) async fn ensure_ok(response: reqwest::Response) -> Result<reqwest::Response, SttError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    Err(SttError::Provider(format!(
        "HTTP {status}: {}",
        truncate(&body, MAX_ERROR_BODY)
    )))
}

pub(crate) fn transport(error: reqwest::Error) -> SttError {
    SttError::Transport(error.to_string())
}

pub(crate) fn decode(error: impl std::fmt::Display) -> SttError {
    SttError::Decode(error.to_string())
}

/// Char-safe truncation with an ellipsis marker, so a multi-byte body can't
/// panic on a byte-index slice.
pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_untouched() {
        assert_eq!(truncate("hello", 400), "hello");
    }

    #[test]
    fn long_text_is_marked_as_cut() {
        let cut = truncate(&"x".repeat(500), 400);
        assert_eq!(cut.chars().count(), 401);
        assert!(cut.ends_with('…'));
    }

    /// A byte-index slice here would panic; providers do return non-ASCII
    /// error bodies.
    #[test]
    fn multibyte_bodies_do_not_panic() {
        let text = "é".repeat(500);
        assert_eq!(truncate(&text, 10).chars().count(), 11);
    }
}
