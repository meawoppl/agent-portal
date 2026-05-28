use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[derive(Debug, Clone, Copy)]
pub struct EmptyResponse(StatusCode);

impl EmptyResponse {
    pub const OK: Self = Self(StatusCode::OK);
    pub const CREATED: Self = Self(StatusCode::CREATED);
    pub const ACCEPTED: Self = Self(StatusCode::ACCEPTED);
    pub const NO_CONTENT: Self = Self(StatusCode::NO_CONTENT);
}

impl IntoResponse for EmptyResponse {
    fn into_response(self) -> Response {
        self.0.into_response()
    }
}
