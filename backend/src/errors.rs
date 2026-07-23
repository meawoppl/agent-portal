use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

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
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            AppError::DbPool => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database connection failed",
            ),
            AppError::DbQuery(e) => {
                tracing::error!("Database query failed: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Database query failed")
            }
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized"),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "Forbidden"),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, *msg),
            AppError::PayloadTooLarge(msg) => (StatusCode::PAYLOAD_TOO_LARGE, msg.as_str()),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, *msg),
            AppError::NotFound(what) => (StatusCode::NOT_FOUND, *what),
            AppError::BadGateway(msg) => (StatusCode::BAD_GATEWAY, *msg),
            AppError::GatewayTimeout(msg) => (StatusCode::GATEWAY_TIMEOUT, *msg),
            AppError::ServiceUnavailable(msg) => (StatusCode::SERVICE_UNAVAILABLE, *msg),
            AppError::Internal(e) => {
                tracing::error!("Internal error: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
        };
        (status, msg.to_string()).into_response()
    }
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
