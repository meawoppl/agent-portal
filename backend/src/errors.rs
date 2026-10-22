use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use shared::api::{ApiError, ApiErrorCode};

#[derive(Debug)]
pub enum AppError {
    DbPool,
    DbQuery(String),
    Unauthorized,
    Forbidden,
    BadRequest(&'static str),
    /// A request payload exceeded a configured cap (413). Carries a dynamic
    /// caller-facing message (e.g. the size and limit) because the CLI surfaces
    /// the response body verbatim.
    PayloadTooLarge(String),
    Conflict(&'static str),
    NotFound(&'static str),
    BadGateway(&'static str),
    /// Like [`AppError::BadGateway`] but carries a dynamic upstream message —
    /// used to relay an agent CLI's own error text (login start failures) to
    /// the browser verbatim, matching the login contract's "errors relay as
    /// Display strings".
    BadGatewayMessage(String),
    GatewayTimeout(&'static str),
    ServiceUnavailable(&'static str),
    Internal(String),
}

/// Blanket Diesel-error conversion so handlers can use `?` directly on
/// query results. Matches the historical blanket `map_err(|e|
/// AppError::DbQuery(e.to_string()))` mapping — handlers that want a
/// different status for `diesel::result::Error::NotFound` keep an explicit
/// `map_err`/`optional()` at the call site.
impl From<diesel::result::Error> for AppError {
    fn from(e: diesel::result::Error) -> Self {
        AppError::DbQuery(e.to_string())
    }
}

impl From<diesel::r2d2::PoolError> for AppError {
    fn from(_: diesel::r2d2::PoolError) -> Self {
        AppError::DbPool
    }
}

impl IntoResponse for AppError {
    /// The message as a plain-text body, as every client has always read it,
    /// with the typed [`ApiError`] attached for [`negotiate_error_body`] to
    /// send instead to a client that asks for JSON.
    fn into_response(self) -> Response {
        use ApiErrorCode as Code;
        let (status, code, msg) = match &self {
            AppError::DbPool => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Code::Unavailable,
                "Database connection failed",
            ),
            AppError::DbQuery(e) => {
                tracing::error!("Database query failed: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Code::Internal,
                    "Database query failed",
                )
            }
            AppError::Unauthorized => {
                (StatusCode::UNAUTHORIZED, Code::Unauthorized, "Unauthorized")
            }
            AppError::Forbidden => (StatusCode::FORBIDDEN, Code::Forbidden, "Forbidden"),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, Code::BadRequest, *msg),
            AppError::PayloadTooLarge(msg) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Code::PayloadTooLarge,
                msg.as_str(),
            ),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, Code::Conflict, *msg),
            AppError::NotFound(what) => (StatusCode::NOT_FOUND, Code::NotFound, *what),
            AppError::BadGateway(msg) => (StatusCode::BAD_GATEWAY, Code::UpstreamFailed, *msg),
            AppError::BadGatewayMessage(msg) => {
                (StatusCode::BAD_GATEWAY, Code::UpstreamFailed, msg.as_str())
            }
            AppError::GatewayTimeout(msg) => {
                (StatusCode::GATEWAY_TIMEOUT, Code::UpstreamTimeout, *msg)
            }
            AppError::ServiceUnavailable(msg) => {
                (StatusCode::SERVICE_UNAVAILABLE, Code::Unavailable, *msg)
            }
            AppError::Internal(e) => {
                tracing::error!("Internal error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Code::Internal,
                    "Internal server error",
                )
            }
        };
        // Only failures known to be transient. `ServiceUnavailable` also
        // covers features that are not configured, which no retry fixes.
        let retryable = matches!(self, AppError::DbPool | AppError::GatewayTimeout(_));
        let mut response = (status, msg.to_string()).into_response();
        response.extensions_mut().insert(ApiError {
            code,
            message: msg.to_string(),
            retryable,
        });
        response
    }
}

/// Whether an `Accept` header names `application/json` itself with a
/// non-zero quality. A wildcard alone doesn't count: a client that sends
/// `*/*` (every browser `fetch`, most CLIs) has always read plain text.
fn accepts_json(accept: &str) -> bool {
    accept.split(',').any(|range| {
        let mut parts = range.split(';');
        let media = parts.next().unwrap_or_default().trim();
        let quality = parts
            .filter_map(|param| {
                let (key, value) = param.split_once('=')?;
                key.trim()
                    .eq_ignore_ascii_case("q")
                    .then(|| value.trim().parse::<f32>().ok())?
            })
            .next()
            .unwrap_or(1.0);
        media.eq_ignore_ascii_case("application/json") && quality > 0.0
    })
}

/// Send a typed [`ApiError`] as the JSON body to a request that asked for
/// JSON, and the plain-text message to everything else. Only the body and
/// its content headers change; the status and every other header (auth,
/// cookies, `Retry-After`) are kept. Responses without an `ApiError`
/// (successes, upgrades, handlers with their own error bodies) pass through.
pub async fn negotiate_error_body(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let json = request
        .headers()
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(accepts_json);
    let mut response = next.run(request).await;
    let Some(error) = response.extensions_mut().remove::<ApiError>() else {
        return response;
    };
    // The body depends on Accept, so caches must key on it too.
    response
        .headers_mut()
        .append(header::VARY, HeaderValue::from_static("accept"));
    let Some(body) = json.then(|| serde_json::to_vec(&error).ok()).flatten() else {
        return response;
    };
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, axum::body::Body::from(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diesel_errors_convert_to_db_query() {
        let err: AppError = diesel::result::Error::NotFound.into();
        assert!(matches!(err, AppError::DbQuery(_)));
        assert_eq!(
            err.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn service_unavailable_maps_to_503() {
        let response = AppError::ServiceUnavailable("OAuth client not configured").into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn auth_failures_map_to_expected_statuses() {
        let cases = [
            (AppError::Unauthorized, StatusCode::UNAUTHORIZED),
            (AppError::Forbidden, StatusCode::FORBIDDEN),
            (AppError::DbPool, StatusCode::INTERNAL_SERVER_ERROR),
            (
                AppError::DbQuery("lookup failed".to_string()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (error, expected_status) in cases {
            assert_eq!(error.into_response().status(), expected_status);
        }
    }

    #[test]
    fn handler_failures_map_to_expected_statuses() {
        let cases = [
            (AppError::BadRequest("bad input"), StatusCode::BAD_REQUEST),
            (AppError::NotFound("missing"), StatusCode::NOT_FOUND),
            (AppError::BadGateway("send failed"), StatusCode::BAD_GATEWAY),
            (
                AppError::GatewayTimeout("timed out"),
                StatusCode::GATEWAY_TIMEOUT,
            ),
            (
                AppError::Internal("disk failed".to_string()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (error, expected_status) in cases {
            assert_eq!(error.into_response().status(), expected_status);
        }
    }
}

#[cfg(test)]
mod negotiation_tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt;

    #[test]
    fn json_is_named_explicitly_with_nonzero_quality() {
        for yes in [
            "application/json",
            "text/html, application/json;q=0.5",
            "Application/JSON",
            "application/json ; q=1",
        ] {
            assert!(accepts_json(yes), "{yes}");
        }
        for no in [
            "",
            "*/*",
            "application/*",
            "text/plain",
            "text/*",
            "application/json;q=0",
            "application/json; q=0.0",
            "application/jsonseq",
            "application/problem+json",
        ] {
            assert!(!accepts_json(no), "{no}");
        }
    }

    fn app() -> Router {
        Router::new()
            .route("/ok", get(|| async { "fine" }))
            .route(
                "/db",
                get(|| async {
                    Err::<(), _>(AppError::DbQuery("relation users has secrets".into()))
                }),
            )
            .route(
                "/auth",
                get(|| async {
                    (
                        [
                            (header::RETRY_AFTER, "5"),
                            (header::SET_COOKIE, "cc_session=; Max-Age=0"),
                            (header::VARY, "origin"),
                        ],
                        AppError::Unauthorized,
                    )
                }),
            )
            .layer(axum::middleware::from_fn(negotiate_error_body))
    }

    async fn get_with(path: &str, accept: Option<&str>) -> (Response, String) {
        let mut request = Request::get(path);
        if let Some(accept) = accept {
            request = request.header(header::ACCEPT, accept);
        }
        let response = app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
        (
            Response::from_parts(parts, Body::empty()),
            String::from_utf8(bytes.to_vec()).unwrap(),
        )
    }

    #[tokio::test]
    async fn plain_text_unless_json_is_asked_for() {
        for accept in [
            None,
            Some("*/*"),
            Some("text/*"),
            Some("application/json;q=0"),
        ] {
            let (response, body) = get_with("/db", accept).await;
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(body, "Database query failed", "{accept:?}");
            assert_eq!(response.headers()[header::VARY], "accept");
        }
        let (response, body) = get_with("/db", Some("application/json")).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        // The internal detail stays in the log, never the body.
        let error: ApiError = serde_json::from_str(&body).unwrap();
        assert_eq!(
            error,
            ApiError {
                code: ApiErrorCode::Internal,
                message: "Database query failed".into(),
                retryable: false,
            }
        );
    }

    #[tokio::test]
    async fn successes_are_untouched() {
        let (response, body) = get_with("/ok", Some("application/json")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body, "fine");
        assert!(response.headers().get(header::VARY).is_none());
    }

    #[tokio::test]
    async fn status_and_other_headers_are_kept() {
        for accept in [None, Some("application/json")] {
            let (response, body) = get_with("/auth", accept).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let headers = response.headers();
            assert_eq!(headers[header::RETRY_AFTER], "5");
            assert_eq!(headers[header::SET_COOKIE], "cc_session=; Max-Age=0");
            let vary: Vec<_> = headers.get_all(header::VARY).iter().collect();
            assert_eq!(vary, ["origin", "accept"]);
            assert!(body.contains("Unauthorized"));
        }
    }
}
