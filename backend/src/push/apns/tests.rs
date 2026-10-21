use super::*;
use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode, Version},
    routing::post,
    Router,
};
use chrono::Utc;
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use tokio::sync::mpsc;
use uuid::Uuid;
const TEST_APNS_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg8g/n6j9roKvnUkwu
lCEIvbDqlUhA5FOzcakkG90E8L+hRANCAATKS2ZExEybUvchRDuKBftotMwVEus3
jDwmlD1Gg0yJt1e38djFwsxsfr5q2hv0Rj9fTEqAPr8H7mGm0wKxZ7iQ
-----END PRIVATE KEY-----";
const TEST_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEyktmRMRMm1L3IUQ7igX7aLTMFRLr
N4w8JpQ9RoNMibdXt/HYxcLMbH6+atob9EY/X0xKgD6/B+5hptMCsWe4kA==
-----END PUBLIC KEY-----";

fn transport() -> ApnsTransport {
    ApnsTransport {
        http: ApnsTransport::http_client().unwrap(),
        bundle_id: "io.txcl.agentportal".into(),
        key_id: "TESTKEY123".into(),
        team_id: "TESTTEAM12".into(),
        encoding_key: EncodingKey::from_ec_pem(TEST_APNS_KEY.as_bytes()).unwrap(),
        token: Mutex::new(None),
        endpoint: "https://api.push.apple.com".into(),
    }
}

fn subscription() -> PushSubscription {
    PushSubscription {
        id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        platform: PushPlatform::Apns.as_wire().into(),
        endpoint_or_token: "ab12".repeat(16),
        p256dh: None,
        auth: None,
        device_label: None,
        created_at: Utc::now(),
        last_success_at: None,
        disabled_at: None,
    }
}

fn payload() -> PushPayload {
    let id = Uuid::new_v4();
    PushPayload {
        session_id: id,
        event_kind: "permission_request".into(),
        title: "session".into(),
        body: "Permission needed: Bash".into(),
        collapse_key: id.to_string(),
    }
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    iat: u64,
}

fn verify(token: &str) -> Claims {
    let header = decode_header(token).unwrap();
    assert_eq!(header.alg, Algorithm::ES256);
    assert_eq!(header.kid.as_deref(), Some("TESTKEY123"));
    let mut validation = Validation::new(Algorithm::ES256);
    validation.required_spec_claims.clear(); // APNs requires iat, not exp.
    validation.validate_exp = false;
    decode::<Claims>(
        token,
        &DecodingKey::from_ec_pem(TEST_PUBLIC_KEY.as_bytes()).unwrap(),
        &validation,
    )
    .unwrap()
    .claims
}

#[test]
fn provider_token_is_signed_reused_and_refreshed_at_fifty_minutes() {
    let client = transport();
    let first = client.provider_token(10_000).unwrap();
    let claims = verify(&first);
    assert_eq!(claims.iss, "TESTTEAM12");
    assert_eq!(claims.iat, 10_000);
    assert_eq!(client.provider_token(12_999).unwrap(), first);
    let renewed = client.provider_token(13_000).unwrap();
    assert_eq!(verify(&renewed).iat, 13_000);
    assert_ne!(renewed, first);
    // Clock rollback must not keep a token issued in the future.
    assert_eq!(verify(&client.provider_token(12_000).unwrap()).iat, 12_000);
}

#[test]
fn malformed_target_and_oversized_payload_are_rejected_before_io() {
    let mut sub = subscription();
    let mut body = payload();
    sub.platform = "fcm".into();
    assert!(ApnsTransport::build_payload(&sub, &body).is_err());
    sub = subscription();
    sub.endpoint_or_token = "../other?target=x".into();
    assert!(ApnsTransport::build_payload(&sub, &body).is_err());
    sub = subscription();
    body.collapse_key = "x".repeat(65);
    assert!(ApnsTransport::build_payload(&sub, &body).is_err());
    body = payload();
    body.body = "x".repeat(4096);
    assert!(ApnsTransport::build_payload(&sub, &body).is_err());
}

#[tokio::test]
async fn http2_request_has_provider_auth_headers_and_existing_payload() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let app = Router::new().route(
        "/3/device/{token}",
        post(
            move |version: Version, headers: HeaderMap, bytes: Bytes| async move {
                tx.send((version, headers, bytes)).unwrap();
                StatusCode::OK
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = transport();
    client.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let sub = subscription();
    let body = payload();
    assert_eq!(
        client.send(&sub, &body).await.unwrap(),
        SendOutcome::Delivered
    );
    let (version, headers, bytes) = rx.recv().await.unwrap();
    assert_eq!(version, Version::HTTP_2);
    assert_eq!(headers["apns-topic"], "io.txcl.agentportal");
    assert_eq!(headers["apns-push-type"], "alert");
    assert_eq!(headers["apns-collapse-id"], body.collapse_key);
    assert_eq!(headers["content-type"], "application/json");
    let auth = headers["authorization"].to_str().unwrap();
    assert_eq!(
        verify(auth.strip_prefix("Bearer ").unwrap()).iss,
        "TESTTEAM12"
    );
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["aps"]["alert"]["title"], body.title);
    assert_eq!(value["aps"]["alert"]["body"], body.body);
    assert_eq!(value["aps"]["sound"], "default");
    assert_eq!(
        value["agent_portal"]["session_id"],
        body.session_id.to_string()
    );
    assert_eq!(value["agent_portal"]["event_kind"], body.event_kind);
    server.abort();
}

#[tokio::test]
async fn http_errors_keep_dead_tokens_distinct_from_credentials_and_server_failures() {
    #[derive(serde::Serialize)]
    struct ErrorResponse {
        reason: &'static str,
    }

    for (status, reason, dead) in [
        (410, "Unregistered", true),
        (400, "BadDeviceToken", true),
        (400, "DeviceTokenNotForTopic", true),
        (403, "ExpiredProviderToken", false),
        (403, "InvalidProviderToken", false),
        (429, "TooManyRequests", false),
        (500, "InternalServerError", false),
    ] {
        let app = Router::new().route(
            "/3/device/{token}",
            post(move || async move {
                (
                    StatusCode::from_u16(status).unwrap(),
                    axum::Json(ErrorResponse { reason }),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = transport();
        client.endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let result = client.send(&subscription(), &payload()).await;
        if dead {
            assert_eq!(result.unwrap(), SendOutcome::GoneDeadEndpoint);
        } else {
            assert!(
                matches!(result, Err(PushError::Transport(message)) if message.contains(reason))
            );
        }
        server.abort();
    }
}
