//! Server-side verification shared by Secrets settings and approval cards.
use crate::utils;
use gloo_net::http::Request;
use shared::api::SecretVerificationStatus;

/// Explicit user-initiated verification, shared by inventory and approval flows.
pub(crate) async fn verify_session() -> Result<(), &'static str> {
    let request = async {
        let response = Request::post(&utils::api_url("/api/secrets/verification/check"))
            .header("x-portal-secrets", "1")
            .send()
            .await
            .map_err(|_| "Could not verify your account. Try again.")?;
        if response.status() == 401 {
            return Err("Your Portal login has expired. Sign in again before approving access.");
        }
        if !response.ok() {
            return Err(
                "Account verification did not succeed. Try again or check your account access.",
            );
        }
        let status = response
            .json::<SecretVerificationStatus>()
            .await
            .map_err(|_| "Invalid verification response. Try again.")?;
        if !status.verified {
            return Err("Account verification did not succeed. Try again.");
        }
        Ok(())
    };
    let timeout = gloo::timers::future::TimeoutFuture::new(15_000);
    futures_util::pin_mut!(request, timeout);
    match futures_util::future::select(request, timeout).await {
        futures_util::future::Either::Left((result, _)) => result,
        futures_util::future::Either::Right(_) => Err("Verification timed out. Try again."),
    }
}
