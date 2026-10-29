//! Human-readable operational context. Names supplement stable IDs; they never
//! grant authority. Never record credentials, cookies, query strings or bodies.
use axum::{
    extract::{ConnectInfo, MatchedPath, Request, State},
    middleware::Next,
    response::Response,
};
use diesel::prelude::*;
use std::net::SocketAddr;
use tracing::Instrument;
use uuid::Uuid;

/// Bound user-controlled labels and prevent multiline log injection.
pub fn label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect()
}

pub fn user_label(name: Option<&str>, email: &str) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => label(&format!("{name} <{email}>")),
        None => label(email),
    }
}

pub fn record_user(user: &crate::models::User) {
    tracing::Span::current().record("user", user_label(user.name.as_deref(), &user.email));
    tracing::Span::current().record("user_id", tracing::field::display(user.id));
}

/// Called once at socket registration, not on streamed output or heartbeats.
pub fn record_session(conn: &mut PgConnection, id: Uuid) {
    use crate::schema::{sessions, users};
    let span = tracing::Span::current();
    span.record("session_id", tracing::field::display(id));
    let row = sessions::table
        .inner_join(users::table)
        .filter(sessions::id.eq(id))
        .select((
            sessions::session_name,
            sessions::hostname,
            users::id,
            users::name,
            users::email,
        ))
        .first::<(String, String, Uuid, Option<String>, String)>(conn);
    if let Ok((name, host, user_id, name_user, email)) = row {
        span.record("session_name", label(&name));
        span.record("computer", label(&host));
        span.record("owner", user_label(name_user.as_deref(), &email));
        span.record("owner_id", tracing::field::display(user_id));
    } else {
        for field in ["session_name", "computer", "owner", "owner_id"] {
            span.record(field, "unknown");
        }
    }
}

/// The peer is the TCP peer, possibly a reverse proxy. Forwarded addresses are
/// explicitly labelled as reported/untrusted; neither is used for authorization.
/// A span adds context to existing events without an access-log line per poll.
fn request_span(request: &Request, forward_domain: Option<&str>) -> tracing::Span {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|p| p.0.ip().to_string())
        .unwrap_or_else(|| "unknown".into());
    let header = |name| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(label)
            .unwrap_or_default()
    };
    let forwarded = forward_domain.is_some_and(|domain| {
        request
            .headers()
            .get("host")
            .and_then(|h| h.to_str().ok())
            .and_then(|host| crate::handlers::forward_proxy::parse_forward_host(host, domain))
            .is_some()
    });
    let route = if forwarded {
        "<forwarded app>"
    } else {
        request
            .extensions()
            .get::<MatchedPath>()
            .map(|p| p.as_str())
            .unwrap_or("<unmatched>")
    };
    tracing::info_span!("request", method = %request.method(), route = %route,
        peer_ip = %peer, reported_forwarded_for = %header("x-forwarded-for"), user_agent = %header("user-agent"),
        user = tracing::field::Empty, user_id = tracing::field::Empty)
}

pub async fn request_context(
    State(state): State<std::sync::Arc<crate::AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let span = request_span(&request, state.forward_domain.as_deref());
    next.run(request).instrument(span).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[derive(Clone, Default)]
    struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    pub(crate) fn capture(f: impl FnOnce()) -> String {
        let output = Buffer::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = output.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn request_context_logs_plain_addresses_but_no_query_or_credentials() {
        let mut request = Request::builder()
            .uri("/api/sessions?ticket=secret-query")
            .header("authorization", "Bearer secret-bearer")
            .header("cookie", "secret-cookie")
            .header("x-forwarded-for", "203.0.113.9")
            .header("user-agent", "test-agent")
            .body(axum::body::Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        let logs = capture(|| {
            request_span(&request, None).in_scope(|| tracing::warn!("example rejection"))
        });
        for expected in ["<unmatched>", "127.0.0.1", "203.0.113.9", "test-agent"] {
            assert!(logs.contains(expected), "{logs}");
        }
        request
            .headers_mut()
            .insert("host", "demo.portal.example.test".parse().unwrap());
        *request.uri_mut() = "/secret-app-path/credential".parse().unwrap();
        let forwarded_logs = capture(|| {
            request_span(&request, Some("portal.example.test"))
                .in_scope(|| tracing::warn!("forward failed"))
        });
        assert!(forwarded_logs.contains("forwarded app"));
        assert!(!forwarded_logs.contains("secret-app-path"));
        for secret in ["secret-query", "secret-bearer", "secret-cookie", "Some("] {
            assert!(!logs.contains(secret), "{logs}");
        }
    }

    #[tokio::test]
    async fn routed_request_uses_template_instead_of_sensitive_path_segments() {
        use tower::ServiceExt;
        use tracing::instrument::WithSubscriber;
        let output = Buffer::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let router = axum::Router::new().route(
            "/api/example/{id}",
            axum::routing::get(|request: Request| async move {
                request_span(&request, None).in_scope(|| tracing::warn!("example"));
                axum::http::StatusCode::OK
            }),
        );
        router
            .oneshot(
                Request::builder()
                    .uri("/api/example/private-value?token=secret")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .with_subscriber(subscriber)
            .await
            .unwrap();
        let logs = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("/api/example/{id}"), "{logs}");
        assert!(!logs.contains("private-value"));
        assert!(!logs.contains("secret"));
    }

    #[test]
    fn labels_are_readable_bounded_and_single_line() {
        assert_eq!(
            user_label(Some(" Matt "), "m@example.test"),
            "Matt <m@example.test>"
        );
        assert_eq!(user_label(None, "m@example.test"), "m@example.test");
        assert_eq!(label("line\n\r\tbreak"), "linebreak");
        assert_eq!(label(&"é".repeat(300)).chars().count(), 256);
    }
}
