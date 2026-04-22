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
            AppError::Internal(e) => {
                tracing::error!("Internal error: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
        };
        (status, msg.to_string()).into_response()
    }
}
