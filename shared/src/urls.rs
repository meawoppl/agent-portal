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

/// Why a string is not an acceptable portal origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginError {
    /// Not `http://` or `https://`.
    UnsupportedScheme,
    /// `http://` to anything but a loopback host.
    Insecure,
    /// Carries a path, query, fragment or userinfo — an origin is
    /// scheme + host + port and nothing else.
    NotAnOrigin,
    InvalidHost,
    InvalidPort,
    /// Longer than [`MAX_ORIGIN_LEN`] once canonicalized.
    TooLong,
}

impl std::fmt::Display for OriginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedScheme => "origin must start with https://",
            Self::Insecure => "plain http:// is only allowed for localhost",
            Self::NotAnOrigin => "origin must not contain a path, query, fragment or credentials",
            Self::InvalidHost => "origin host is not a valid hostname or IP address",
            Self::InvalidPort => "origin port is not a valid port number",
            Self::TooLong => "origin is too long",
        })
    }
}

/// Canonical form of a portal origin, identical to the browser's ASCII
/// serialization of `new URL(input).origin` (lowercase scheme and host,
/// punycode for IDNs, compressed IPv6, default port dropped) — so it compares
/// equal to an `Origin` header and two spellings of one portal match.
///
/// Only `https://` is accepted, except `http://` to a loopback host for local
/// development. Paths, queries, fragments, userinfo, wildcards and trailing-dot
/// hosts are rejected rather than stripped: an origin that arrived with extra
/// parts means the caller passed a URL where an identity was expected.
pub fn canonical_origin(input: &str) -> Result<String, OriginError> {
    let input = input.trim();
    check_raw_envelope(input)?;
    let url = url::Url::parse(input).map_err(|err| match err {
        url::ParseError::RelativeUrlWithoutBase => OriginError::UnsupportedScheme,
        url::ParseError::InvalidPort => OriginError::InvalidPort,
        _ => OriginError::InvalidHost,
    })?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err(OriginError::UnsupportedScheme);
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(OriginError::NotAnOrigin);
    }
    let host = url.host().ok_or(OriginError::InvalidHost)?;
    let loopback = match &host {
        url::Host::Domain(domain) if domain.ends_with('.') || domain.contains('*') => {
            return Err(OriginError::InvalidHost)
        }
        url::Host::Domain(domain) => *domain == "localhost",
        url::Host::Ipv4(addr) => addr.is_loopback(),
        url::Host::Ipv6(addr) => addr.is_loopback(),
    };
    if url.scheme() == "http" && !loopback {
        return Err(OriginError::Insecure);
    }
    let origin = url.origin().ascii_serialization();
    if origin.len() > MAX_ORIGIN_LEN {
        return Err(OriginError::TooLong);
    }
    Ok(origin)
}

/// Longest canonical origin accepted; matches the `VARCHAR(255)` columns
/// that store one.
pub const MAX_ORIGIN_LEN: usize = 255;

/// Validate the input as written, before WHATWG parsing normalizes it.
///
/// The parser repairs plenty of malformed input — `https:host` gains its
/// slashes, `%2a` decodes to `*`, `/a/..` collapses to `/`, an empty `@`
/// userinfo vanishes, an IPv4 trailing dot is dropped — so checking only the
/// parsed URL would accept strings that are not origins. The input must be
/// `scheme://authority` with at most one trailing `/`, and the authority may
/// not contain anything an origin can't.
fn check_raw_envelope(input: &str) -> Result<(), OriginError> {
    let (scheme, rest) = input
        .split_once("://")
        .ok_or(OriginError::UnsupportedScheme)?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return Err(OriginError::UnsupportedScheme);
    }
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() {
        return Err(OriginError::InvalidHost);
    }
    if authority
        .chars()
        .any(|c| matches!(c, '/' | '\\' | '?' | '#' | '@') || c.is_whitespace() || c.is_control())
    {
        return Err(OriginError::NotAnOrigin);
    }
    // `%` would let an encoded character through normalization; `*` makes the
    // entry a pattern rather than one origin.
    if authority.contains(['%', '*']) {
        return Err(OriginError::InvalidHost);
    }
    let host = match authority.rfind(':') {
        Some(colon) if !authority[colon..].contains(']') => &authority[..colon],
        _ => authority,
    };
    if host.ends_with('.') {
        return Err(OriginError::InvalidHost);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_origin_normalizes_spelling() {
        for (input, want) in [
            ("https://txcl.io", "https://txcl.io"),
            ("  HTTPS://TXCL.io/ ", "https://txcl.io"),
            ("https://txcl.io:443", "https://txcl.io"),
            (
                "https://portal.cosmicfrontier.org:8443",
                "https://portal.cosmicfrontier.org:8443",
            ),
            ("http://localhost:3000", "http://localhost:3000"),
            ("http://127.0.0.1", "http://127.0.0.1"),
            ("http://[::1]:3000/", "http://[::1]:3000"),
            ("https://[2001:0db8:0:0::1]", "https://[2001:db8::1]"),
            ("https://tëst.io", "https://xn--tst-jma.io"),
            ("https://txcl.io:", "https://txcl.io"),
        ] {
            assert_eq!(canonical_origin(input).as_deref(), Ok(want), "{input}");
        }
    }

    #[test]
    fn canonical_origin_rejects_non_origins() {
        use OriginError::*;
        for (input, want) in [
            ("txcl.io", UnsupportedScheme),
            ("ftp://txcl.io", UnsupportedScheme),
            ("wss://txcl.io", UnsupportedScheme),
            ("http://txcl.io", Insecure),
            ("http://localhost.evil.example", Insecure),
            ("https://txcl.io/dashboard", NotAnOrigin),
            ("https://txcl.io?x=1", NotAnOrigin),
            ("https://txcl.io#x", NotAnOrigin),
            ("https://user@txcl.io", NotAnOrigin),
            ("https://txcl.io\\dashboard", NotAnOrigin),
            ("https://", InvalidHost),
            ("https://*.txcl.io", InvalidHost),
            ("https://txcl.io.", InvalidHost),
            ("https://[zz::1]", InvalidHost),
            ("https://txcl.io:70000", InvalidPort),
            ("https://txcl.io:+443", InvalidPort),
            // Inputs the WHATWG parser would silently repair into an origin.
            ("https://%2a.work.example", InvalidHost),
            ("https://w%6Frk.example", InvalidHost),
            ("https://work.example/a/..", NotAnOrigin),
            ("https://@work.example", NotAnOrigin),
            ("https:work.example", UnsupportedScheme),
            ("https:/work.example", UnsupportedScheme),
            ("https://1.2.3.4.", InvalidHost),
            ("https://1.2.3.4.:8443", InvalidHost),
            ("https://wo\nrk.example", NotAnOrigin),
            ("https:///work.example", NotAnOrigin),
            ("ht\ntps://work.example", UnsupportedScheme),
            ("ht tps://work.example", UnsupportedScheme),
        ] {
            assert_eq!(canonical_origin(input), Err(want), "{input}");
        }
    }

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

    #[test]
    fn canonical_origin_is_bounded_for_storage() {
        let label = "a".repeat(60);
        let host = [label.as_str(); 5].join(".");
        let at_limit = format!("https://{}", &host[..MAX_ORIGIN_LEN - "https://".len()]);
        assert_eq!(
            canonical_origin(&at_limit).map(|o| o.len()),
            Ok(MAX_ORIGIN_LEN)
        );
        let over = format!("https://{host}:8443");
        assert_eq!(canonical_origin(&over), Err(OriginError::TooLong));
    }
}
