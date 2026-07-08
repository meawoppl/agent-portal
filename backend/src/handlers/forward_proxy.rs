//! Forward-origin router: Host-based dispatch, token-handoff auth, and the
//! hyper-over-tunnel reverse proxy (docs/PORT_FORWARDING.md).
//!
//! Requests whose `Host` matches `{label}.{PORTAL_FORWARD_DOMAIN}` never reach
//! the normal router: `forward_host_gate` (a middleware at the router root)
//! sends them here. The label is resolved to a session via the admin
//! `custom_subdomains` table then the auto `forward_subdomains` LUT.
//! `/__portal/auth` exchanges a short-lived handoff
//! JWT (minted on the portal origin by [`open_forward`]) for an 8-hour
//! host-only cookie; everything else requires that cookie, looks up the
//! session's currently-forwarded port (revocation-aware), and is
//! reverse-proxied through a tunnel stream to `127.0.0.1:{port}`.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use diesel::prelude::*;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use tower_cookies::Cookies;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::errors::AppError;
use crate::AppState;

/// Handoff token TTL (portal origin → forward origin redirect).
const HANDOFF_TTL_SECS: i64 = 60;
/// Forward-origin cookie TTL.
const COOKIE_TTL_SECS: i64 = 8 * 60 * 60;
/// `next` length cap (open-redirect hardening).
const MAX_NEXT_LEN: usize = 2048;

const FWD_COOKIE: &str = "portal_fwd";
const AUD_HANDOFF: &str = "portal-forward-auth";
const AUD_COOKIE: &str = "portal-forward-session";

/// Hop-by-hop headers (RFC 9110 §7.6.1) — never forwarded in either
/// direction. `upgrade` stays stripped until WS passthrough lands.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

#[derive(Debug, Serialize, Deserialize)]
struct ForwardClaims {
    aud: String,
    session_id: Uuid,
    exp: i64,
    iat: i64,
}

fn mint_token(app_state: &AppState, aud: &str, session_id: Uuid, ttl: i64) -> String {
    let now = Utc::now().timestamp();
    let claims = ForwardClaims {
        aud: aud.to_string(),
        session_id,
        exp: now + ttl,
        iat: now,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(app_state.jwt_secret.as_bytes()),
    )
    .unwrap_or_default()
}

/// Verify a forward JWT and require it to match this origin's session. (The
/// forward port isn't in the token — a session has at most one, looked up per
/// request so revocation and re-pointing take effect immediately.)
fn verify_token(app_state: &AppState, token: &str, aud: &str, session_id: Uuid) -> bool {
    let mut validation = Validation::default();
    validation.set_audience(&[aud]);
    // `set_audience` alone only checks `aud` *if present*. Require it (and
    // `exp`) so a token minted for some other purpose with the same secret,
    // but no `aud`, can't pass this auth boundary.
    validation.set_required_spec_claims(&["exp", "aud"]);
    match decode::<ForwardClaims>(
        token,
        &DecodingKey::from_secret(app_state.jwt_secret.as_bytes()),
        &validation,
    ) {
        Ok(data) => data.claims.session_id == session_id,
        Err(_) => false,
    }
}

/// Subdomain labels reserved from forwarding — they fall through the host gate
/// to the normal router rather than being treated as (always-missing) forward
/// hosts, and are refused as custom-subdomain assignments. Shared with
/// `admin_subdomains` so the two boundaries can't drift.
pub(crate) const RESERVED_LABELS: &[&str] =
    &["www", "api", "admin", "portal", "app", "static", "assets"];

/// True if `label` is reserved (case-insensitive is unnecessary — callers
/// lowercase first).
pub(crate) fn is_reserved_label(label: &str) -> bool {
    RESERVED_LABELS.contains(&label)
}

/// Normalize an authority and extract the forward subdomain label: lowercase,
/// strip `:port` and trailing dot, then require a single valid DNS label
/// (`[a-z0-9-]`, 1–63 chars, no leading/trailing hyphen, no dots) that isn't a
/// reserved word, and an exact domain match on the rest. Covers both auto
/// 8-hex labels and admin custom labels; the label is resolved to a session
/// via the LUTs downstream (unknown → 404). Reserved / malformed labels return
/// `None` so the request falls through to the normal router.
pub fn parse_forward_host(host: &str, forward_domain: &str) -> Option<String> {
    let host = normalize_authority(host);
    let domain = normalize_authority(forward_domain);
    let rest = host.strip_suffix(&domain)?;
    let label = rest.strip_suffix('.')?;

    let valid = !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && !is_reserved_label(label)
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    valid.then(|| label.to_string())
}

fn normalize_authority(authority: &str) -> String {
    let lower = authority.trim().to_ascii_lowercase();
    // Strip `:port` (not IPv6-literal aware — forward domains are hostnames).
    let no_port = match lower.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host.to_string(),
        _ => lower,
    };
    no_port.trim_end_matches('.').to_string()
}

/// Clamp `next` to an origin-relative path: single leading `/` (reject `//`
/// and `/\` — protocol-relative escapes), no control characters, bounded
/// length. Anything else becomes `/`.
pub fn validate_next(next: &str) -> String {
    let ok = next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && next.len() <= MAX_NEXT_LEN
        && !next.bytes().any(|b| b.is_ascii_control());
    if ok {
        next.to_string()
    } else {
        "/".to_string()
    }
}

/// Public origin for a forward, e.g. `http://a3f9c2e1.localhost:3000`.
fn forward_origin(app_state: &AppState, label: &str) -> Option<String> {
    let domain = app_state.forward_domain.as_deref()?;
    let scheme = if app_state.public_url.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    Some(format!("{scheme}://{label}.{domain}"))
}

#[derive(Debug, Deserialize)]
pub struct OpenForwardQuery {
    #[serde(default)]
    pub next: Option<String>,
}

/// GET /api/sessions/{id}/forwards/open — portal-origin entry point.
/// Authenticated by the normal portal cookie (session read access), mints the
/// 60s handoff token and bounces to the session's forward origin.
pub async fn open_forward(
    State(app_state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    cookies: Cookies,
    Query(query): Query<OpenForwardQuery>,
) -> Result<Redirect, AppError> {
    let user_id = crate::handlers::agent_comms::resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;
    crate::handlers::forwards::member_session(&mut conn, session_id, user_id)?;

    // Forwarding must be enabled and the session must have an active forward.
    if app_state.forward_domain.is_none() {
        return Err(AppError::ServiceUnavailable("Forwarding is not configured"));
    }
    if crate::handlers::forwards::active_forward_port(&mut conn, session_id)?.is_none() {
        return Err(AppError::NotFound("forward"));
    }
    let label = crate::handlers::forwards::ensure_subdomain_label(&mut conn, session_id)?;
    let origin = forward_origin(&app_state, &label)
        .ok_or(AppError::ServiceUnavailable("Forwarding is not configured"))?;

    let token = mint_token(&app_state, AUD_HANDOFF, session_id, HANDOFF_TTL_SECS);
    let next = validate_next(query.next.as_deref().unwrap_or("/"));
    let target = format!(
        "{origin}/__portal/auth?token={token}&next={}",
        urlencoding::encode(&next)
    );
    Ok(Redirect::temporary(&target))
}

/// Router-root middleware: requests for a forward host never reach the
/// normal router.
pub async fn forward_host_gate(
    State(app_state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(domain) = app_state.forward_domain.clone() else {
        return next.run(req).await;
    };
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .or_else(|| req.uri().authority().map(|a| a.to_string()));
    let Some(host) = host else {
        return next.run(req).await;
    };
    match parse_forward_host(&host, &domain) {
        Some(label) => dispatch(app_state, label, req).await,
        None => next.run(req).await,
    }
}

/// Everything served on a forward origin. Resolves the label to a session and
/// its live port (both may 404) before auth + reverse proxy.
async fn dispatch(app_state: Arc<AppState>, label: String, req: Request) -> Response {
    // Resolve label → session up front. An unknown label is a 404 (no leak of
    // whether the label ever existed).
    let (session_id, port, session_key) = {
        let mut conn = match app_state.conn() {
            Ok(conn) => conn,
            Err(_) => return plain_status(StatusCode::SERVICE_UNAVAILABLE, "database unavailable"),
        };
        let session_id = match crate::handlers::forwards::session_for_label(&mut conn, &label) {
            Ok(id) => id,
            Err(_) => return plain_status(StatusCode::NOT_FOUND, "no such forward"),
        };

        if req.uri().path() == "/__portal/auth" {
            return handle_auth(&app_state, session_id, &req);
        }

        // Fail closed but distinctly: a DB error is a 503, not a silent "no
        // forward" (which would masquerade as revocation/auth weirdness).
        let forward = match crate::handlers::forwards::active_forward(&mut conn, session_id) {
            Ok(forward) => forward,
            Err(_) => return plain_status(StatusCode::SERVICE_UNAVAILABLE, "database unavailable"),
        };

        // A *public* forward serves with no auth. Otherwise (private or
        // absent) require the cookie — and gate BEFORE distinguishing
        // private-from-absent, so an unauthenticated caller can't probe
        // whether a private forward is active (they always get the same auth
        // bounce; only authenticated callers see the revocation 404).
        let port = if let Some((port, true)) = forward {
            port
        } else {
            let authed = cookie_value(req.headers(), FWD_COOKIE)
                .map(|token| verify_token(&app_state, &token, AUD_COOKIE, session_id))
                .unwrap_or(false);
            if !authed {
                return unauthenticated_response(&app_state, session_id, &req);
            }
            match forward {
                Some((port, _)) => port,
                None => {
                    return plain_status(StatusCode::NOT_FOUND, "this forward has been revoked")
                }
            }
        };

        use crate::schema::sessions;
        let session_key = match sessions::table
            .find(session_id)
            .select(sessions::session_key)
            .first::<String>(&mut conn)
        {
            Ok(key) => key,
            Err(_) => return plain_status(StatusCode::NOT_FOUND, "session not found"),
        };
        (session_id, port, session_key)
    };

    proxy_request(&app_state, &session_key, session_id, port, &label, req).await
}

/// `/__portal/auth?token=…&next=…` — exchange the handoff token for the
/// forward-origin cookie.
fn handle_auth(app_state: &AppState, session_id: Uuid, req: &Request) -> Response {
    #[derive(Deserialize)]
    struct AuthQuery {
        token: Option<String>,
        next: Option<String>,
    }
    let query: AuthQuery = req
        .uri()
        .query()
        .and_then(|q| serde_urlencoded::from_str(q).ok())
        .unwrap_or(AuthQuery {
            token: None,
            next: None,
        });

    let valid = query
        .token
        .as_deref()
        .map(|t| verify_token(app_state, t, AUD_HANDOFF, session_id))
        .unwrap_or(false);
    if !valid {
        return plain_status(StatusCode::FORBIDDEN, "invalid or expired forward token");
    }

    let cookie_jwt = mint_token(app_state, AUD_COOKIE, session_id, COOKIE_TTL_SECS);
    let secure = if app_state.public_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{FWD_COOKIE}={cookie_jwt}; Path=/; Max-Age={COOKIE_TTL_SECS}; HttpOnly; SameSite=Lax{secure}"
    );
    let next = validate_next(query.next.as_deref().unwrap_or("/"));

    let mut response = Redirect::temporary(&next).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

/// Missing/expired cookie: bounce navigations through the portal origin (the
/// user re-authenticates transparently while logged in); plain 401 for
/// XHR/WS so API calls fail loudly instead of redirecting into HTML.
fn unauthenticated_response(app_state: &AppState, session_id: Uuid, req: &Request) -> Response {
    let is_navigation = req.method() == Method::GET
        && (header_is(req.headers(), "sec-fetch-mode", "navigate")
            || req
                .headers()
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|accept| accept.contains("text/html")));
    if !is_navigation {
        return plain_status(StatusCode::UNAUTHORIZED, "forward session expired");
    }
    let next = req
        .uri()
        .path_and_query()
        .map(|pq| pq.to_string())
        .unwrap_or_else(|| "/".to_string());
    let target = format!(
        "{}/api/sessions/{}/forwards/open?next={}",
        app_state.public_url.trim_end_matches('/'),
        session_id,
        urlencoding::encode(&validate_next(&next))
    );
    Redirect::temporary(&target).into_response()
}

fn header_is(headers: &HeaderMap, name: &str, value: &str) -> bool {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case(value))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|pair| {
        let (k, v) = pair.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

/// The `Cookie` header minus the portal's own cookie — the forwarded app
/// must never see portal credentials, but its own cookies pass through.
fn filtered_cookie_header(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    let kept: Vec<&str> = raw
        .split(';')
        .map(str::trim)
        .filter(|pair| {
            pair.split_once('=')
                .is_none_or(|(name, _)| name != FWD_COOKIE)
        })
        .filter(|pair| !pair.is_empty())
        .collect();
    (!kept.is_empty()).then(|| kept.join("; "))
}

fn plain_status(status: StatusCode, msg: &'static str) -> Response {
    (status, msg).into_response()
}

/// Reverse-proxy one request through a fresh tunnel stream.
async fn proxy_request(
    app_state: &AppState,
    session_key: &str,
    session_id: Uuid,
    port: u16,
    label: &str,
    req: Request,
) -> Response {
    use crate::handlers::websocket::TunnelError;

    let stream = match app_state
        .session_manager
        .open_tunnel(session_key, port)
        .await
    {
        Ok(stream) => stream,
        Err(TunnelError::NotConnected) => {
            return plain_status(
                StatusCode::SERVICE_UNAVAILABLE,
                "the agent for this session is offline",
            )
        }
        Err(TunnelError::Refused(e)) => {
            warn!("Tunnel refused for {}:{}: {}", session_id, port, e);
            return plain_status(
                StatusCode::BAD_GATEWAY,
                "nothing is listening on the forwarded port",
            );
        }
        Err(TunnelError::OpenTimeout) | Err(TunnelError::ClosedEarly) => {
            return plain_status(StatusCode::GATEWAY_TIMEOUT, "forward open timed out")
        }
    };

    let io = hyper_util::rt::TokioIo::new(stream);
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(io).await {
        Ok(pair) => pair,
        Err(e) => {
            warn!("Tunnel HTTP handshake failed: {}", e);
            return plain_status(StatusCode::BAD_GATEWAY, "upstream handshake failed");
        }
    };
    // `with_upgrades` keeps the connection driver alive after a 101 so the
    // upgraded byte stream (WebSocket) stays readable/writable.
    tokio::spawn(async move {
        if let Err(e) = conn.with_upgrades().await {
            debug!("Tunnel HTTP connection ended: {}", e);
        }
    });

    let original_host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    // A client `Connection: Upgrade` + `Upgrade: websocket` request is
    // passed through verbatim: keep the upgrade headers, grab the browser's
    // pending upgrade before consuming the request, and if the upstream
    // agrees (101) splice the two upgraded byte streams together.
    let is_upgrade = wants_upgrade(req.headers());
    let mut req = req;
    let browser_upgrade = is_upgrade.then(|| hyper::upgrade::on(&mut req));

    let upstream_req = match build_upstream_request(req, port, is_upgrade) {
        Ok(r) => r,
        Err(msg) => return plain_status(StatusCode::BAD_REQUEST, msg),
    };

    let mut upstream_resp = match sender.send_request(upstream_req).await {
        Ok(resp) => resp,
        Err(e) => {
            warn!("Tunnel upstream request failed: {}", e);
            return plain_status(StatusCode::BAD_GATEWAY, "upstream request failed");
        }
    };

    let upstream_is_101 = upstream_resp.status() == StatusCode::SWITCHING_PROTOCOLS;
    match (browser_upgrade, upstream_is_101) {
        (Some(browser_upgrade), true) => {
            let upstream_upgrade = hyper::upgrade::on(&mut upstream_resp);
            tokio::spawn(splice_upgraded(browser_upgrade, upstream_upgrade));
            // Relay the 101 (with its Upgrade/Connection/Sec-WebSocket-Accept
            // headers) so the browser completes its handshake.
            build_downstream_response(app_state, upstream_resp, label, port, original_host)
        }
        // Upstream returned 101 to a request the browser never asked to
        // upgrade — there is no browser-side splice, so refuse rather than
        // send a dangling protocol switch.
        (None, true) => {
            warn!("Upstream returned 101 to a non-upgrade request; refusing");
            plain_status(
                StatusCode::BAD_GATEWAY,
                "unexpected upstream protocol switch",
            )
        }
        // Normal response (browser wanted an upgrade or not; upstream said no).
        (_, false) => {
            build_downstream_response(app_state, upstream_resp, label, port, original_host)
        }
    }
}

/// True for a WebSocket-style upgrade request: some `Connection` value
/// contains the `upgrade` token and `Upgrade` is present. Inspects *all*
/// `Connection` header lines — a client may split `keep-alive` and `Upgrade`
/// into separate fields, equivalent to comma-joining them.
fn wants_upgrade(headers: &HeaderMap) -> bool {
    let connection_upgrade = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
    connection_upgrade && headers.contains_key(header::UPGRADE)
}

/// Copy bytes both ways between the browser's upgraded connection and the
/// upstream's, until either side closes. Both futures must resolve before any
/// bytes flow (the 101s have been sent on both ends).
async fn splice_upgraded(browser: hyper::upgrade::OnUpgrade, upstream: hyper::upgrade::OnUpgrade) {
    let (browser_io, upstream_io) = match tokio::try_join!(browser, upstream) {
        Ok(pair) => pair,
        Err(e) => {
            debug!("Upgrade handshake did not complete: {}", e);
            return;
        }
    };
    let mut browser_io = hyper_util::rt::TokioIo::new(browser_io);
    let mut upstream_io = hyper_util::rt::TokioIo::new(upstream_io);
    if let Err(e) = tokio::io::copy_bidirectional(&mut browser_io, &mut upstream_io).await {
        debug!("Upgraded stream copy ended: {}", e);
    }
}

/// Rewrite the browser request for the loopback upstream: origin-form URI,
/// hop-by-hop headers stripped, portal credentials removed, `Host` pinned to
/// `localhost:{port}` (defuses dev-server host checks), `X-Forwarded-*` set.
///
/// When `is_upgrade`, the `Connection` and `Upgrade` headers are preserved
/// (and other hop-by-hop headers still stripped) so a WebSocket handshake
/// reaches the upstream intact.
fn build_upstream_request(
    req: Request,
    port: u16,
    is_upgrade: bool,
) -> Result<hyper::Request<Body>, &'static str> {
    let (parts, body) = req.into_parts();

    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let uri: Uri = path_and_query.parse().map_err(|_| "bad request path")?;

    let mut builder = hyper::Request::builder().method(parts.method).uri(uri);
    let headers = builder.headers_mut().ok_or("bad request headers")?;

    for (name, value) in parts.headers.iter() {
        let lower = name.as_str();
        // On an upgrade, `connection` and `upgrade` are the handshake and must
        // pass through; the rest of the hop-by-hop set is still dropped.
        let keep_for_upgrade = is_upgrade && (lower == "connection" || lower == "upgrade");
        if (HOP_BY_HOP.contains(&lower) && !keep_for_upgrade)
            || lower == "host"
            || lower == "authorization"
            || lower == "cookie"
        {
            continue;
        }
        headers.append(name.clone(), value.clone());
    }
    // Cookies pass through minus the portal's own.
    if let Some(filtered) = filtered_cookie_header(&parts.headers) {
        if let Ok(value) = HeaderValue::from_str(&filtered) {
            headers.insert(header::COOKIE, value);
        }
    }
    if let Ok(host) = HeaderValue::from_str(&format!("localhost:{port}")) {
        headers.insert(header::HOST, host);
    }
    if let Some(original_host) = parts.headers.get(header::HOST) {
        headers.insert(
            HeaderName::from_static("x-forwarded-host"),
            original_host.clone(),
        );
    }
    headers.insert(
        HeaderName::from_static("x-forwarded-proto"),
        HeaderValue::from_static("http"),
    );
    if let Some(connect) = parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
    {
        if let Ok(value) = HeaderValue::from_str(&connect.0.ip().to_string()) {
            headers.insert(HeaderName::from_static("x-forwarded-for"), value);
        }
    }

    builder.body(body).map_err(|_| "bad request")
}

/// Strip hop-by-hop headers and rewrite absolute `Location`s pointing at the
/// loopback upstream back to the forward origin.
fn build_downstream_response(
    app_state: &AppState,
    resp: hyper::Response<hyper::body::Incoming>,
    label: &str,
    port: u16,
    original_host: Option<String>,
) -> Response {
    let (parts, body) = resp.into_parts();
    // On a 101 the browser needs the `Connection` + `Upgrade` handshake
    // headers; keep exactly those two and still strip the rest of hop-by-hop.
    // (`Sec-WebSocket-Accept` isn't hop-by-hop, so it passes normally.)
    let is_upgrade = parts.status == StatusCode::SWITCHING_PROTOCOLS;
    let mut response = Response::builder().status(parts.status);
    let headers = match response.headers_mut() {
        Some(h) => h,
        None => return plain_status(StatusCode::BAD_GATEWAY, "bad upstream response"),
    };

    // Prefer reconstructing the origin from the request's own Host so the
    // scheme/port the browser used is preserved verbatim.
    let scheme = if app_state.public_url.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    let origin = original_host
        .map(|h| format!("{scheme}://{h}"))
        .or_else(|| forward_origin(app_state, label));

    for (name, value) in parts.headers.iter() {
        let lower = name.as_str();
        let keep_for_upgrade = is_upgrade && (lower == "connection" || lower == "upgrade");
        if HOP_BY_HOP.contains(&lower) && !keep_for_upgrade {
            continue;
        }
        // Framing policy is rewritten below: drop the upstream's
        // X-Frame-Options and any `frame-ancestors` CSP directive (the rest
        // of its CSP passes through). Multiple CSP headers intersect
        // (most-restrictive wins), so appending ours without stripping the
        // upstream's could never *allow* the portal to embed.
        if lower == "x-frame-options" {
            continue;
        }
        if lower == "content-security-policy" {
            if let Ok(csp) = value.to_str() {
                // `None` = the policy was only frame-ancestors — drop it.
                if let Some(rest) = strip_frame_ancestors(csp) {
                    if let Ok(v) = HeaderValue::from_str(&rest) {
                        headers.append(name.clone(), v);
                    }
                }
                continue;
            }
        }
        if (lower == "location" || lower == "content-location") && origin.is_some() {
            if let Ok(loc) = value.to_str() {
                let rewritten = rewrite_upstream_location(loc, port, origin.as_deref().unwrap());
                if let Ok(v) = HeaderValue::from_str(&rewritten) {
                    headers.append(name.clone(), v);
                    continue;
                }
            }
        }
        headers.append(name.clone(), value.clone());
    }

    // Allow the portal (and the forward origin itself) to embed the response —
    // the session-view preview overlay renders forwards in an iframe
    // (docs/PORT_FORWARDING.md). This *narrows* framing for apps that shipped
    // no policy (previously any site could frame them) and re-scopes apps
    // that shipped `frame-ancestors 'self'`/`X-Frame-Options` (e.g. Jupyter)
    // to portal-or-self instead of blocking the overlay.
    if !is_upgrade {
        let portal_origin = app_state.public_url.trim_end_matches('/');
        if let Ok(v) = HeaderValue::from_str(&format!("frame-ancestors 'self' {portal_origin}")) {
            headers.append(header::CONTENT_SECURITY_POLICY, v);
        }
    }

    match response.body(Body::new(body)) {
        Ok(r) => r,
        Err(_) => plain_status(StatusCode::BAD_GATEWAY, "bad upstream response"),
    }
}

/// Remove any `frame-ancestors` directive from a CSP header value, keeping
/// every other directive intact. `None` when nothing remains. The directive
/// name is the first whitespace-separated token (CSP whitespace includes
/// HTAB, not just space), compared case-insensitively.
pub fn strip_frame_ancestors(csp: &str) -> Option<String> {
    let kept: Vec<&str> = csp
        .split(';')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .filter(|d| {
            let name = d.split_ascii_whitespace().next().unwrap_or("");
            !name.eq_ignore_ascii_case("frame-ancestors")
        })
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join("; "))
    }
}

/// `http://localhost:8080/foo` → `{forward origin}/foo` (also `127.0.0.1`
/// and `[::1]` authorities). Anything else passes through untouched.
pub fn rewrite_upstream_location(location: &str, port: u16, origin: &str) -> String {
    for authority in [
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
    ] {
        for scheme in ["http", "https"] {
            let prefix = format!("{scheme}://{authority}");
            if let Some(rest) = location.strip_prefix(&prefix) {
                if rest.is_empty() || rest.starts_with('/') || rest.starts_with('?') {
                    return format!("{origin}{rest}");
                }
            }
        }
    }
    location.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A valid 8-lowercase-hex subdomain label.
    const LABEL: &str = "a3f9c2e1";

    #[test]
    fn parse_forward_host_accepts_dev_authority() {
        // Auto hash label and admin custom labels are both valid single DNS
        // labels; resolution to a session (or 404) happens downstream.
        assert_eq!(
            parse_forward_host(&format!("{LABEL}.localhost:3000"), "localhost:3000").as_deref(),
            Some(LABEL)
        );
        assert_eq!(
            parse_forward_host("my-app.localhost:3000", "localhost:3000").as_deref(),
            Some("my-app")
        );
    }

    #[test]
    fn parse_forward_host_normalizes_case_and_trailing_dot() {
        // Authority is lowercased and the trailing dot stripped before
        // matching, so a mixed-case domain and a FQDN dot both parse; the
        // label itself is emitted lowercase.
        assert_eq!(
            parse_forward_host(&format!("{LABEL}.LocalHost."), "LOCALHOST").as_deref(),
            Some(LABEL)
        );
    }

    #[test]
    fn parse_forward_host_rejects_non_labels() {
        for host in [
            format!("{LABEL}.evil.com"),             // wrong domain
            format!("sub.{LABEL}.localhost"),        // extra label (contains a dot)
            "under_score.localhost".to_string(),     // invalid char
            "-lead.localhost".to_string(),           // leading hyphen
            "trail-.localhost".to_string(),          // trailing hyphen
            format!("{}.localhost", "a".repeat(64)), // 64 chars, too long
            "portal.example.com".to_string(),        // ordinary host
        ] {
            assert!(
                parse_forward_host(&host, "localhost").is_none(),
                "should reject {host}"
            );
        }
    }

    #[test]
    fn parse_forward_host_falls_through_reserved_labels() {
        // Reserved labels must not be treated as (always-missing) forward
        // hosts; they return None so the normal router serves them.
        for label in RESERVED_LABELS {
            assert!(
                parse_forward_host(&format!("{label}.localhost"), "localhost").is_none(),
                "reserved label {label} should fall through"
            );
        }
    }

    #[test]
    fn strip_frame_ancestors_preserves_other_directives() {
        // Jupyter-style: only frame-ancestors → drop the whole header.
        assert_eq!(strip_frame_ancestors("frame-ancestors 'self'"), None);
        assert_eq!(strip_frame_ancestors("FRAME-ANCESTORS 'none'"), None);
        // CSP whitespace includes HTAB (codex review nit on #1251).
        assert_eq!(strip_frame_ancestors("frame-ancestors\t'none'"), None);
        // Mixed policy: everything else survives verbatim.
        assert_eq!(
            strip_frame_ancestors("default-src 'self'; frame-ancestors 'none'; img-src data:")
                .as_deref(),
            Some("default-src 'self'; img-src data:")
        );
        // No frame-ancestors: unchanged (modulo whitespace normalization).
        assert_eq!(
            strip_frame_ancestors("default-src 'self'").as_deref(),
            Some("default-src 'self'")
        );
        // A directive that merely starts with the same bytes is not stripped.
        assert_eq!(
            strip_frame_ancestors("frame-ancestors-custom 'x'").as_deref(),
            Some("frame-ancestors-custom 'x'")
        );
    }

    #[test]
    fn validate_next_clamps_hostile_values() {
        assert_eq!(validate_next("/dash?tab=1"), "/dash?tab=1");
        assert_eq!(validate_next("//evil.com"), "/");
        assert_eq!(validate_next("/\\evil.com"), "/");
        assert_eq!(validate_next("https://evil.com"), "/");
        assert_eq!(validate_next("no-slash"), "/");
        assert_eq!(validate_next("/a\r\nSet-Cookie: x"), "/");
        assert_eq!(validate_next(&format!("/{}", "a".repeat(3000))), "/");
    }

    #[test]
    fn wants_upgrade_detects_websocket_handshake() {
        let ws = |conn: &str, has_upgrade: bool| {
            let mut h = HeaderMap::new();
            h.insert(header::CONNECTION, HeaderValue::from_str(conn).unwrap());
            if has_upgrade {
                h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
            }
            wants_upgrade(&h)
        };
        // Real browser sends `Connection: Upgrade` (sometimes with keep-alive).
        assert!(ws("Upgrade", true));
        assert!(ws("keep-alive, Upgrade", true));
        assert!(ws("upgrade", true)); // case-insensitive token
                                      // Missing either half is not an upgrade.
        assert!(!ws("Upgrade", false));
        assert!(!ws("keep-alive", true));
        assert!(!wants_upgrade(&HeaderMap::new()));

        // Split into two separate Connection header lines (RFC-legal) — the
        // upgrade token must still be found regardless of field order.
        let mut split = HeaderMap::new();
        split.append(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        split.append(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        split.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        assert!(wants_upgrade(&split));
    }

    #[test]
    fn verify_token_requires_aud_claim() {
        // A JWT signed with the same secret but carrying no `aud` (e.g. a
        // token minted for some other purpose) must not authenticate a
        // forward, even with a matching session_id + exp.
        #[derive(serde::Serialize)]
        struct NoAud {
            session_id: Uuid,
            exp: i64,
        }
        let secret = b"test-secret-value-at-least-32-bytes-long";
        let sid = Uuid::new_v4();
        let no_aud = encode(
            &Header::default(),
            &NoAud {
                session_id: sid,
                exp: chrono::Utc::now().timestamp() + 60,
            },
            &EncodingKey::from_secret(secret),
        )
        .unwrap();

        let mut validation = Validation::default();
        validation.set_audience(&[AUD_HANDOFF]);
        validation.set_required_spec_claims(&["exp", "aud"]);
        let decoded =
            decode::<ForwardClaims>(&no_aud, &DecodingKey::from_secret(secret), &validation);
        assert!(decoded.is_err(), "token without aud must be rejected");
    }

    #[test]
    fn location_rewrite_covers_loopback_authorities() {
        let origin = "http://a3f9c2e1.localhost:3000";
        assert_eq!(
            rewrite_upstream_location("http://localhost:8080/login", 8080, origin),
            format!("{origin}/login")
        );
        assert_eq!(
            rewrite_upstream_location("http://127.0.0.1:8080", 8080, origin),
            origin
        );
        // Different port / non-loopback pass through.
        assert_eq!(
            rewrite_upstream_location("http://localhost:9090/x", 8080, origin),
            "http://localhost:9090/x"
        );
        assert_eq!(
            rewrite_upstream_location("https://example.com/x", 8080, origin),
            "https://example.com/x"
        );
        // Authority prefix that isn't a path boundary must not rewrite.
        assert_eq!(
            rewrite_upstream_location("http://localhost:80801/x", 8080, origin),
            "http://localhost:80801/x"
        );
    }
}
