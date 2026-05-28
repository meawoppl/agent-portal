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
    NotFound(&'static str),
    BadGateway(&'static str),
    GatewayTimeout(&'static str),
    ServiceUnavailable(&'static str),
    Internal(String),
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
}
