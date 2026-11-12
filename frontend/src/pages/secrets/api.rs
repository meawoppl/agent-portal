//! Home-portal metadata requests only. Never fetch saved values or surface
//! server bodies, which could accidentally echo sensitive input on failure.
use crate::utils;
use gloo_net::http::{Request, Response};
use serde::Serialize;

pub async fn mutate(method: &str, path: &str, body: &impl Serialize) -> Result<Response, String> {
    let url = utils::api_url(path);
    let request = match method {
        "DELETE" => Request::delete(&url),
        _ => Request::post(&url),
    }
    .header("x-portal-secrets", "1")
    .header("Idempotency-Key", &uuid::Uuid::new_v4().to_string())
    .header("Cache-Control", "no-store");
    let response = utils::send_json(request, body).await.map_err(|_| {
        "Could not reach the Portal. Refresh before retrying. If you were saving a value, enter it again."
            .to_string()
    })?;
    if response.ok() {
        return Ok(response);
    }
    Err(match response.status() {
        401 => "Sign in again before managing secrets.",
        403 => "This action requires permission and account verification. Use the verification button, then retry.",
        409 => "This secret changed. Refresh its metadata before trying again.",
        413 => "The value exceeds the supported size.",
        503 => "The secrets service is unavailable. Try again later.",
        _ => "The request could not be completed. Refresh and try again.",
    }
    .into())
}
