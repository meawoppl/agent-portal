//! The injected agent identity can request approval and execute approved bindings.
//! Plaintext access requires a scoped binding ticket; no human credential fallback.
use super::sensitive::SecretString;
use super::{client, input};
use anyhow::{bail, Context};
use reqwest::Method;
use shared::api::*;
use std::ffi::OsString;
use uuid::Uuid;
use zeroize::Zeroize;

pub(super) struct BoundAgent {
    pub(super) client: client::Client,
    pub(super) session_id: Uuid,
}
impl BoundAgent {
    pub(super) fn from_values(
        backend: Option<String>,
        token: Option<String>,
        session: Option<String>,
        dev: bool,
    ) -> anyhow::Result<Self> {
        let backend = backend.filter(|value| !value.is_empty()).context(
            "Bound execution requires PORTAL_SESSION_BACKEND; no launcher credential fallback",
        )?;
        let token = SecretString::new(token.filter(|value| !value.is_empty()).context(
            "Bound execution requires PORTAL_SESSION_TOKEN; no other credential fallback",
        )?);
        let session_id: Uuid = session
            .context("Bound execution requires PORTAL_SESSION_ID")?
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid bound session context"))?;
        Ok(Self {
            client: client::Client::new(&backend, dev, Some(token))?,
            session_id,
        })
    }
    pub(super) fn from_env(dev: bool) -> anyhow::Result<Self> {
        Self::from_values(
            std::env::var("PORTAL_SESSION_BACKEND").ok(),
            std::env::var("PORTAL_SESSION_TOKEN").ok(),
            std::env::var("PORTAL_SESSION_ID").ok(),
            dev,
        )
    }
    async fn redeem_all(&self, bindings: &[Uuid]) -> anyhow::Result<Vec<(String, SecretString)>> {
        if bindings.is_empty() || bindings.len() > MAX_SECRET_ACCESS_GROUP_ITEMS {
            bail!("Provide between 1 and 16 approved bindings");
        }
        let mut seen = std::collections::HashSet::new();
        if bindings.iter().any(|id| !seen.insert(*id)) {
            bail!("Duplicate binding ID");
        }
        let mut values = Vec::with_capacity(bindings.len());
        let mut envs = std::collections::HashSet::new();
        for binding in bindings {
            let value = self.redeem(*binding).await?;
            if !envs.insert(value.0.to_ascii_uppercase()) {
                bail!("Bindings use duplicate environment variables");
            }
            values.push(value);
        }
        Ok(values)
    }
    async fn redeem(&self, binding: Uuid) -> anyhow::Result<(String, SecretString)> {
        self.redeem_version(binding, None).await
    }
    pub(super) async fn redeem_version(
        &self,
        binding: Uuid,
        expected: Option<i32>,
    ) -> anyhow::Result<(String, SecretString)> {
        let response: SecretBindingTicketResponse = self
            .client
            .request::<_, ()>(Method::POST, &format!("/bindings/{binding}/ticket"), None)
            .await
            .map_err(binding_error)?;
        let ticket = SecretString::new(response.ticket.0);
        if response.expires_in == 0 || response.expires_in > 30 {
            bail!("Invalid short-lived binding ticket policy");
        }
        let mut request = SecretBindingRedeemRequest {
            ticket: SecretValue(ticket.as_str().to_owned()),
        };
        let response = self
            .client
            .request::<SecretBindingRedeemResponse, _>(
                Method::POST,
                &format!("/bindings/{binding}/redeem"),
                Some(&request),
            )
            .await;
        request.ticket.0.zeroize();
        let response = response.map_err(binding_error)?;
        let value = SecretString::new(response.value.0);
        if value.is_empty()
            || value.len() > input::MAX_VALUE_BYTES
            || response.version < 1
            || expected.is_some_and(|v| v != response.version)
        {
            bail!("Binding returned an invalid version or value size");
        }
        let env_name = input::env_name(&response.env_name).map_err(|_| {
            anyhow::anyhow!("Binding returned an invalid approved environment name")
        })?;
        if value.contains('\0') {
            bail!("Bound value contains NUL and cannot be represented in an environment variable");
        }
        Ok((env_name, value))
    }
}
fn binding_error(error: anyhow::Error) -> anyhow::Error {
    match error
        .downcast_ref::<client::ServiceError>()
        .map(|error| error.status)
    {
        Some(401) => anyhow::anyhow!(
            "Bound session credential expired or rejected; reconnect this portal session"
        ),
        Some(403) => anyhow::anyhow!(
            "Binding denied, revoked, or no longer valid for this session, launcher, and secret"
        ),
        Some(404) => anyhow::anyhow!("Approved binding not found"),
        Some(409) => anyhow::anyhow!(
            "Binding ticket expired, already redeemed, or changed; request a fresh ticket"
        ),
        _ => error,
    }
}

pub(super) async fn execute(
    bindings: &[Uuid],
    command: &[OsString],
    dev: bool,
) -> anyhow::Result<()> {
    command
        .first()
        .context("Missing child executable after --")?;
    let agent = BoundAgent::from_env(dev)?;
    let values = agent.redeem_all(bindings).await?;
    // Never start the child with a partially redeemed credential set.
    super::run_child(command, values)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn redeem_all_checks_every_binding_and_rejects_duplicate_environments() {
        use super::super::requests::tests::mock_server;
        for duplicate in [false, true] {
            let replies = vec![
                serde_json::json!({"ticket":"ticket-one","expires_in":30}),
                serde_json::json!({"env_name":"FIRST","value":"one","version":1}),
                serde_json::json!({"ticket":"ticket-two","expires_in":30}),
                serde_json::json!({"env_name":if duplicate {"FIRST"} else {"SECOND"},"value":"two","version":1}),
            ];
            let (url, server) = mock_server(replies);
            let agent = BoundAgent::from_values(
                Some(url),
                Some("scoped-token".into()),
                Some(Uuid::new_v4().to_string()),
                true,
            )
            .unwrap();
            let result = agent.redeem_all(&[Uuid::new_v4(), Uuid::new_v4()]).await;
            if duplicate {
                assert!(result.is_err());
            } else {
                let values = result.unwrap();
                assert_eq!(values.len(), 2);
                assert_eq!(values[0].0, "FIRST");
                assert_eq!(values[1].0, "SECOND");
            }
            assert_eq!(server.join().unwrap().len(), 4);
        }
    }
    #[test]
    fn context_requires_injected_session_fields_without_fallbacks() {
        let session = Uuid::new_v4().to_string();
        assert!(BoundAgent::from_values(
            Some("https://example.test".into()),
            None,
            Some(session.clone()),
            false
        )
        .is_err());
        assert!(BoundAgent::from_values(
            None,
            Some("proxy-token".into()),
            Some(session.clone()),
            false
        )
        .is_err());
        assert!(BoundAgent::from_values(
            Some("https://example.test".into()),
            Some("proxy-token".into()),
            None,
            false
        )
        .is_err());
        assert!(BoundAgent::from_values(
            Some("https://example.test".into()),
            Some("proxy-token".into()),
            Some(session),
            false
        )
        .is_ok());
    }
    #[tokio::test]
    async fn rejected_agent_token_and_invalid_ticket_policy_never_fall_back_or_echo_body() {
        use std::io::{Read, Write};
        for (status, body) in [
            ("403 Forbidden", r#"{"error":"DO-NOT-ECHO-TOKEN-OR-VALUE"}"#),
            (
                "200 OK",
                r#"{"ticket":"DO-NOT-ECHO-TICKET","expires_in":3600}"#,
            ),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0u8; 1024];
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0 && bytes.len() < 16384);
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                String::from_utf8(bytes).unwrap()
            });
            let agent = BoundAgent::from_values(
                Some(format!("http://{address}")),
                Some("REJECTED-NON-SESSION-TOKEN".into()),
                Some(Uuid::new_v4().to_string()),
                true,
            )
            .unwrap();
            let error = agent.redeem(Uuid::new_v4()).await.unwrap_err();
            let message = format!("{error:?}");
            assert!(!message.contains("DO-NOT-ECHO"));
            assert!(!message.contains("secret authorize"));
            let request = server.join().unwrap();
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer rejected-non-session-token"));
            assert!(!request.contains("aps_secret_"));
            assert!(!request.contains("DO-NOT-ECHO"));
        }
    }
    #[tokio::test]
    async fn ticket_and_value_only_travel_through_bound_protocol_not_argv_or_metadata_output() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let binding = Uuid::new_v4();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            let responses = [
                r#"{"ticket":"ONE-TIME-TICKET","expires_in":30}"#,
                r#"{"value":"BOUND-VALUE-$(literal)","env_name":"TOKEN","version":2}"#,
            ];
            for body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0u8; 1024];
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0 && bytes.len() < 16384);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:").map(str::trim))
                            .unwrap_or("0")
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(bytes).unwrap());
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
            requests
        });
        let agent = BoundAgent::from_values(
            Some(format!("http://{address}")),
            Some("SESSION-PROXY-JWT".into()),
            Some(Uuid::new_v4().to_string()),
            true,
        )
        .unwrap();
        let (name, value) = agent.redeem(binding).await.unwrap();
        assert_eq!(name, "TOKEN");
        assert_eq!(value.as_str(), "BOUND-VALUE-$(literal)");
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with(&format!("POST /api/secrets/bindings/{binding}/ticket ")));
        assert!(requests[1].starts_with(&format!("POST /api/secrets/bindings/{binding}/redeem ")));
        for request in &requests {
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer session-proxy-jwt"));
            assert!(!request.contains("BOUND-VALUE"));
            assert!(!request.contains("aps_secret_"));
        }
        assert!(!requests[0].contains("ONE-TIME-TICKET"));
        assert!(requests[1].ends_with(r#"{"ticket":"ONE-TIME-TICKET"}"#));
        #[cfg(unix)]
        {
            let command = [OsString::from("/usr/bin/true")];
            let mut child = super::super::child_command(&command, &[(name, value)]).unwrap();
            assert_eq!(child.get_args().count(), 0);
            let output = child.output().unwrap();
            assert!(output.status.success());
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
        }
    }
}
