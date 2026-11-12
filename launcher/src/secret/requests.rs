//! Agents request approval metadata, never plaintext or a human credential.
use super::{bindings::BoundAgent, client, input};
use anyhow::{bail, Context};
use clap::Args;
use reqwest::Method;
use shared::api::{
    SecretAccessCreateRequest, SecretAccessRequestInfo, SecretAccessState, SecretNamespace,
};
use std::{io::Write, time::Duration};
use uuid::Uuid;

const MAX_WAIT: Duration = Duration::from_secs(600);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Args)]
pub struct RequestArgs {
    #[arg(
        required_unless_present = "credentials",
        conflicts_with = "credentials"
    )]
    reference: Option<input::Reference>,
    #[arg(long = "env", value_parser = input::env_name, required_unless_present = "credentials", conflicts_with = "credentials")]
    env_name: Option<String>,
    /// Request a credential set in one approval card; repeat NAME=personal/SECRET.
    #[arg(long = "credential", conflicts_with_all = ["reference", "env_name", "version"])]
    credentials: Vec<input::EnvMapping>,
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..))]
    version: Option<i32>,
    /// Approved binding lifetime in seconds, starting when the human approves.
    #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..=3600))]
    expires_in: u64,
    /// Reuse this UUID and identical arguments when retrying the same request.
    #[arg(long)]
    request_id: Option<Uuid>,
    /// Wait for in-thread approval, bounded by the request's ten-minute expiry.
    #[arg(long)]
    wait: bool,
}
impl RequestArgs {
    fn payload(&self) -> SecretAccessCreateRequest {
        SecretAccessCreateRequest {
            request_id: self.request_id.unwrap_or_else(Uuid::new_v4),
            reference: self
                .reference
                .as_ref()
                .expect("clap requires a reference for single requests")
                .to_string(),
            env_name: self
                .env_name
                .clone()
                .expect("clap requires an environment for single requests"),
            version: self.version,
            expires_in_seconds: self.expires_in,
        }
    }
}

fn validate_scope(
    agent: &BoundAgent,
    request: &SecretAccessCreateRequest,
    info: &SecretAccessRequestInfo,
) -> anyhow::Result<()> {
    let reference: input::Reference = request.reference.parse().map_err(anyhow::Error::msg)?;
    let target_matches = match reference {
        input::Reference::Id(id) => info.secret_id == id,
        input::Reference::Personal(name) => {
            info.namespace == SecretNamespace::Personal && info.name == name
        }
        input::Reference::Organization(name) => {
            info.namespace == SecretNamespace::Organization && info.name == name
        }
    };
    if !target_matches
        || info.id != request.request_id
        || info.session_id != agent.session_id
        || info.env_name != request.env_name
        || info.expires_in_seconds != request.expires_in_seconds
        || info.version < 1
        || request
            .version
            .is_some_and(|version| version != info.version)
        || (info.state == SecretAccessState::Approved && info.binding_id.is_none())
    {
        bail!("Secret access response does not match the requested session and scope");
    }
    Ok(())
}
fn request_error(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<client::ServiceError>().map(|error| error.status) {
        Some(401) => anyhow::anyhow!("Session credential expired or rejected; reconnect this portal session"),
        Some(403) => anyhow::anyhow!("Secret request is not authorized for this bound session and process"),
        Some(404) => anyhow::anyhow!("Secret or access request not found"),
        Some(409) => anyhow::anyhow!("Request ID belongs to different arguments or process; inspect its original status or start a new request"),
        _ => error,
    }
}
async fn create(
    agent: &BoundAgent,
    request: &SecretAccessCreateRequest,
) -> anyhow::Result<SecretAccessRequestInfo> {
    let info = agent
        .client
        .request(Method::POST, "/requests", Some(request))
        .await
        .map_err(request_error)?;
    validate_scope(agent, request, &info)?;
    Ok(info)
}
async fn status(
    agent: &BoundAgent,
    request: &SecretAccessCreateRequest,
) -> anyhow::Result<SecretAccessRequestInfo> {
    let info = agent
        .client
        .request::<_, ()>(
            Method::GET,
            &format!("/requests/{}", request.request_id),
            None,
        )
        .await
        .map_err(request_error)?;
    validate_scope(agent, request, &info)?;
    Ok(info)
}

fn validate_pinned_scope(
    previous: &SecretAccessRequestInfo,
    next: &SecretAccessRequestInfo,
) -> anyhow::Result<()> {
    if previous.id != next.id
        || previous.secret_id != next.secret_id
        || previous.owner_user_id != next.owner_user_id
        || previous.namespace != next.namespace
        || previous.name != next.name
        || previous.version != next.version
        || previous.session_id != next.session_id
        || previous.launcher_id != next.launcher_id
        || previous.env_name != next.env_name
        || previous.expires_in_seconds != next.expires_in_seconds
    {
        bail!("Secret access request changed its pinned scope; request a new approval");
    }
    Ok(())
}

struct WaitResult {
    info: SecretAccessRequestInfo,
    timed_out: bool,
}
async fn wait_for_decision(
    agent: &BoundAgent,
    request: &SecretAccessCreateRequest,
    mut info: SecretAccessRequestInfo,
    interval: Duration,
    maximum: Duration,
) -> anyhow::Result<WaitResult> {
    let expires = chrono::DateTime::parse_from_rfc3339(&info.expires_at)
        .context("Invalid access request expiry")?;
    let remaining = expires
        .with_timezone(&chrono::Utc)
        .signed_duration_since(chrono::Utc::now())
        .to_std()
        .unwrap_or(Duration::ZERO);
    let deadline = tokio::time::Instant::now() + remaining.min(maximum).min(MAX_WAIT);
    while info.state == SecretAccessState::Pending {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(WaitResult {
                info,
                timed_out: true,
            });
        }
        tokio::time::sleep(interval.min(remaining)).await;
        match tokio::time::timeout_at(deadline, status(agent, request)).await {
            Ok(next) => {
                let next = next?;
                validate_pinned_scope(&info, &next)?;
                info = next;
            }
            Err(_) => {
                return Ok(WaitResult {
                    info,
                    timed_out: true,
                })
            }
        }
    }
    Ok(WaitResult {
        info,
        timed_out: false,
    })
}
/// One typed metadata object owns stdout even when terminal denial makes the
/// command fail. Unknown wire fields (including accidental values) stay omitted.
fn report(
    info: &SecretAccessRequestInfo,
    timed_out: bool,
    mut writer: impl Write,
) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(&mut writer, info)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    match info.state {
        SecretAccessState::Denied => bail!("Secret access request denied by the human"),
        SecretAccessState::Expired => bail!("Secret access request expired; request approval again"),
        SecretAccessState::Invalidated => bail!("Secret access request invalidated; create a new request for the current session and secret"),
        _ if timed_out => bail!("Approval wait ended while pending; retry with the same --request-id to inspect its status"),
        _ => Ok(()),
    }
}
mod group;
pub(super) async fn run(args: &RequestArgs, dev: bool) -> anyhow::Result<()> {
    if !args.credentials.is_empty() {
        return group::run(args, dev).await;
    }
    let agent = BoundAgent::from_env(dev)?;
    let request = args.payload();
    eprintln!("Secret access request {}: approval appears in this session's chat. Reuse --request-id {} with identical arguments after a tool retry.", request.request_id, request.request_id);
    let info = create(&agent, &request).await?;
    let result = if args.wait && info.state == SecretAccessState::Pending {
        wait_for_decision(&agent, &request, info, POLL_INTERVAL, MAX_WAIT).await?
    } else {
        WaitResult {
            info,
            timed_out: false,
        }
    };
    report(&result.info, result.timed_out, std::io::stdout().lock())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use clap::Parser;
    fn payload() -> SecretAccessCreateRequest {
        SecretAccessCreateRequest {
            request_id: Uuid::new_v4(),
            reference: "personal/DEPLOY_TOKEN".into(),
            env_name: "DEPLOY_TOKEN".into(),
            version: None,
            expires_in_seconds: 900,
        }
    }
    fn info(
        request: &SecretAccessCreateRequest,
        session: Uuid,
        state: SecretAccessState,
    ) -> SecretAccessRequestInfo {
        SecretAccessRequestInfo {
            id: request.request_id,
            secret_id: Uuid::new_v4(),
            owner_user_id: Uuid::new_v4(),
            namespace: SecretNamespace::Personal,
            name: "DEPLOY_TOKEN".into(),
            version: 1,
            session_id: session,
            launcher_id: Uuid::new_v4(),
            env_name: request.env_name.clone(),
            expires_in_seconds: request.expires_in_seconds,
            state,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339(),
            binding_id: if state == SecretAccessState::Approved {
                Some(Uuid::new_v4())
            } else {
                None
            },
        }
    }
    pub(crate) fn mock_server(
        responses: Vec<serde_json::Value>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            responses
                .into_iter()
                .map(|body| {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    loop {
                        let mut chunk = [0u8; 4096];
                        let count = socket.read(&mut chunk).unwrap();
                        assert!(count > 0 && bytes.len() + count <= 65536);
                        bytes.extend_from_slice(&chunk[..count]);
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers =
                                String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                            let length: usize = headers
                                .lines()
                                .find_map(|line| {
                                    line.strip_prefix("content-length:").map(str::trim)
                                })
                                .unwrap_or("0")
                                .parse()
                                .unwrap();
                            if bytes.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let body = body.to_string();
                    write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .unwrap();
                    String::from_utf8(bytes).unwrap()
                })
                .collect()
        });
        (format!("http://{address}"), server)
    }
    #[test]
    fn request_cli_requires_explicit_reference_valid_env_and_bounded_scope() {
        let id = Uuid::new_v4().to_string();
        let parsed = crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "request",
            "personal/DEPLOY_TOKEN",
            "--env",
            "DEPLOY_TOKEN",
            "--request-id",
            &id,
            "--wait",
        ])
        .unwrap();
        let Some(crate::Command::Secret {
            action: super::super::Action::Request(args),
        }) = parsed.command
        else {
            panic!("wrong request action");
        };
        assert_eq!(args.expires_in, 900);
        assert!(args.wait);
        assert_eq!(args.payload().request_id.to_string(), id);
        assert_eq!(args.payload().request_id, args.payload().request_id);
        for reference in ["TOKEN", "shared/TOKEN", "id/not-a-uuid"] {
            assert!(crate::Args::try_parse_from([
                "agent-portal",
                "secret",
                "request",
                reference,
                "--env",
                "TOKEN"
            ])
            .is_err());
        }
        for lifetime in ["0", "3601"] {
            assert!(crate::Args::try_parse_from([
                "agent-portal",
                "secret",
                "request",
                "org/TOKEN",
                "--env",
                "TOKEN",
                "--expires-in",
                lifetime
            ])
            .is_err());
        }
        for env in ["", "1TOKEN", "TOKEN;exec"] {
            assert!(crate::Args::try_parse_from([
                "agent-portal",
                "secret",
                "request",
                "personal/TOKEN",
                "--env",
                env
            ])
            .is_err());
        }
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "request",
            "personal/TOKEN",
            "--env",
            "TOKEN",
            "--version",
            "0"
        ])
        .is_err());
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "request",
            "personal/TOKEN",
            "--env",
            "TOKEN",
            "--value",
            "NEVER-IN-ARGV"
        ])
        .is_err());
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "request",
            "org/TOKEN",
            "--env",
            "TOKEN",
            "--version",
            "2",
            "--expires-in",
            "3600"
        ])
        .is_ok());
    }
    #[test]
    fn request_cannot_fall_back_when_bound_session_context_is_missing() {
        assert!(BoundAgent::from_values(
            Some("https://example.test".into()),
            None,
            Some(Uuid::new_v4().to_string()),
            false
        )
        .is_err());
        assert!(BoundAgent::from_values(
            None,
            Some("owner-or-launcher-token-must-not-be-found".into()),
            Some(Uuid::new_v4().to_string()),
            false
        )
        .is_err());
        assert!(BoundAgent::from_values(
            Some("https://example.test".into()),
            Some("session-proxy-token".into()),
            None,
            false
        )
        .is_err());
    }
    #[test]
    fn stdout_is_one_metadata_object_with_nonzero_terminal_rejections() {
        let request = payload();
        let session = Uuid::new_v4();
        for state in [
            SecretAccessState::Pending,
            SecretAccessState::Approved,
            SecretAccessState::Denied,
            SecretAccessState::Expired,
            SecretAccessState::Invalidated,
        ] {
            let info = info(&request, session, state);
            let mut stdout = Vec::new();
            let result = report(&info, false, &mut stdout);
            assert_eq!(
                result.is_err(),
                matches!(
                    state,
                    SecretAccessState::Denied
                        | SecretAccessState::Expired
                        | SecretAccessState::Invalidated
                )
            );
            let decoded: SecretAccessRequestInfo = serde_json::from_slice(&stdout).unwrap();
            assert_eq!(decoded, info);
            let wire = String::from_utf8(stdout).unwrap();
            assert!(!wire.contains("access_token"));
            assert!(!wire.contains("ticket"));
            assert!(!wire.contains("\"value\""));
        }
    }
    #[tokio::test]
    async fn proxy_bound_request_retries_and_polling_return_only_approved_binding_metadata() {
        let request = payload();
        let session = Uuid::new_v4();
        let pending = info(&request, session, SecretAccessState::Pending);
        let mut approved = pending.clone();
        approved.state = SecretAccessState::Approved;
        approved.binding_id = Some(Uuid::new_v4());
        let mut approved_wire = serde_json::to_value(&approved).unwrap();
        approved_wire["value"] = "NEVER-PRINT-THIS".into();
        approved_wire["ticket"] = "NEVER-PRINT-TICKET".into();
        let (base, server) = mock_server(vec![
            serde_json::to_value(&pending).unwrap(),
            serde_json::to_value(&pending).unwrap(),
            approved_wire,
        ]);
        let agent = BoundAgent::from_values(
            Some(base),
            Some("BOUND-PROXY-JWT".into()),
            Some(session.to_string()),
            true,
        )
        .unwrap();
        let first = create(&agent, &request).await.unwrap();
        assert_eq!(create(&agent, &request).await.unwrap(), first);
        let result = wait_for_decision(
            &agent,
            &request,
            first,
            Duration::from_millis(1),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert!(!result.timed_out);
        assert_eq!(result.info, approved);
        let mut stdout = Vec::new();
        report(&result.info, false, &mut stdout).unwrap();
        assert_eq!(
            serde_json::from_slice::<SecretAccessRequestInfo>(&stdout)
                .unwrap()
                .binding_id,
            approved.binding_id
        );
        assert!(!String::from_utf8(stdout).unwrap().contains("NEVER-PRINT"));
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("POST /api/secrets/requests "));
        assert!(requests[1].starts_with("POST /api/secrets/requests "));
        assert!(requests[2].starts_with(&format!(
            "GET /api/secrets/requests/{} ",
            request.request_id
        )));
        for wire in &requests {
            assert!(wire
                .to_ascii_lowercase()
                .contains("authorization: bearer bound-proxy-jwt"));
            assert!(!wire.contains("aps_secret_"));
            assert!(!wire.contains("NEVER-PRINT"));
        }
        let body = |wire: &String| {
            serde_json::from_str::<serde_json::Value>(wire.split_once("\r\n\r\n").unwrap().1)
                .unwrap()
        };
        assert_eq!(body(&requests[0]), body(&requests[1]));
        assert_eq!(
            body(&requests[0])["request_id"],
            request.request_id.to_string()
        );
    }
    #[tokio::test]
    async fn wait_is_bounded_and_does_not_invent_approval_or_an_expired_server_state() {
        let request = payload();
        let session = Uuid::new_v4();
        let pending = info(&request, session, SecretAccessState::Pending);
        let agent = BoundAgent::from_values(
            Some("https://example.test".into()),
            Some("bound-token".into()),
            Some(session.to_string()),
            false,
        )
        .unwrap();
        let result = wait_for_decision(
            &agent,
            &request,
            pending.clone(),
            Duration::from_secs(2),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(result.timed_out);
        assert_eq!(result.info.state, SecretAccessState::Pending);
        let mut stdout = Vec::new();
        assert!(report(&result.info, true, &mut stdout).is_err());
        assert_eq!(
            serde_json::from_slice::<SecretAccessRequestInfo>(&stdout).unwrap(),
            pending
        );
        let mut changed_version = pending.clone();
        changed_version.version += 1;
        assert!(validate_pinned_scope(&pending, &changed_version).is_err());
        let mut invalid = pending.clone();
        invalid.session_id = Uuid::new_v4();
        assert!(validate_scope(&agent, &request, &invalid).is_err());
        invalid = pending;
        invalid.state = SecretAccessState::Approved;
        assert!(validate_scope(&agent, &request, &invalid).is_err());
    }
}
