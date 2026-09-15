//! HTTP<->WebSocket scheme helpers shared across crates.
//!
//! Single home for the `http(s)://` <-> `ws(s)://` scheme swap previously
//! hand-rolled per call site (`str::replace`/`replacen` on the whole URL).
//! Everything here is std-only so the crate keeps compiling for
//! `wasm32-unknown-unknown`.

/// Rewrite an `http(s)://` URL to its `ws(s)://` counterpart.
///
/// Only the leading scheme is swapped; any other occurrence (a redirect URL
/// embedded in the query string, say) is left alone, and anything without an
/// HTTP(S) scheme passes through unchanged.
#[must_use]
pub fn http_to_ws(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        url.to_string()
    }
}

/// Rewrite a `ws(s)://` URL to its `http(s)://` counterpart.
///
/// Mirror of [`http_to_ws`]: only the leading scheme is swapped, and anything
/// without a WS(S) scheme passes through unchanged (so an already-HTTP base
/// URL stays usable).
#[must_use]
pub fn ws_to_http(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_to_ws_swaps_leading_scheme_only() {
        assert_eq!(
            http_to_ws("https://portal.example/base"),
            "wss://portal.example/base"
        );
        assert_eq!(
            http_to_ws("http://localhost:3000/base"),
            "ws://localhost:3000/base"
        );
        // An embedded URL in the query string is not a scheme: leave it alone.
        assert_eq!(
            http_to_ws("https://a.example/?next=http://b.example/"),
            "wss://a.example/?next=http://b.example/"
        );
    }

    #[test]
    fn http_to_ws_passes_other_schemes_through() {
        assert_eq!(http_to_ws("wss://portal.example"), "wss://portal.example");
        assert_eq!(http_to_ws("ws://localhost:3000"), "ws://localhost:3000");
        assert_eq!(http_to_ws("not-a-url"), "not-a-url");
    }

    #[test]
    fn ws_to_http_swaps_leading_scheme_only() {
        assert_eq!(
            ws_to_http("wss://portal.example/base"),
            "https://portal.example/base"
        );
        assert_eq!(
            ws_to_http("ws://localhost:3000/base"),
            "http://localhost:3000/base"
        );
        assert_eq!(
            ws_to_http("wss://a.example/?next=ws://b.example/"),
            "https://a.example/?next=ws://b.example/"
        );
    }

    #[test]
    fn ws_to_http_passes_other_schemes_through() {
        assert_eq!(
            ws_to_http("https://portal.example"),
            "https://portal.example"
        );
        assert_eq!(ws_to_http("http://localhost:3000"), "http://localhost:3000");
        assert_eq!(ws_to_http("not-a-url"), "not-a-url");
    }

    #[test]
    fn scheme_helpers_round_trip() {
        for url in [
            "https://portal.example/a?x=1#frag",
            "http://localhost:3000/a?x=1#frag",
        ] {
            assert_eq!(ws_to_http(&http_to_ws(url)), url);
        }
        for url in ["wss://portal.example/a?x=1", "ws://localhost:3000/a?x=1"] {
            assert_eq!(http_to_ws(&ws_to_http(url)), url);
        }
    }
}
