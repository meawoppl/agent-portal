use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::errors::AppError;

/// Error response for device flow endpoints
#[derive(Debug, Serialize)]
pub struct DeviceFlowError {
    pub error: String,
    pub message: String,
}

/// Device flow API error that returns JSON
pub struct DeviceFlowApiError {
    status: StatusCode,
    error: String,
    message: String,
}

impl DeviceFlowApiError {
    pub(super) fn service_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            error: "service_unavailable".to_string(),
            message: "Device flow authentication is not available. Server may be in dev mode or OAuth is not configured.".to_string(),
        }
    }

    pub(super) fn not_found(msg: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: "not_found".to_string(),
            message: msg.to_string(),
        }
    }

    pub(super) fn internal_error(msg: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: "internal_error".to_string(),
            message: msg.to_string(),
        }
    }
}

impl IntoResponse for DeviceFlowApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(DeviceFlowError {
                error: self.error,
                message: self.message,
            }),
        )
            .into_response()
    }
}

pub(super) fn auth_error_to_device_flow(error: AppError, action: &str) -> DeviceFlowApiError {
    match error {
        AppError::Forbidden => DeviceFlowApiError {
            status: StatusCode::FORBIDDEN,
            error: "forbidden".to_string(),
            message: format!("You are not allowed to {} device authorization", action),
        },
        AppError::DbPool | AppError::DbQuery(_) => {
            DeviceFlowApiError::internal_error("Database connection failed")
        }
        _ => DeviceFlowApiError {
            status: StatusCode::UNAUTHORIZED,
            error: "unauthorized".to_string(),
            message: format!("You must be logged in to {} device authorization", action),
        },
    }
}
