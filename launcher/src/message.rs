//! `agent-portal message` subcommands: list your sessions and send a message
//! into one. A thin client over the backend's `/api/agent/*` endpoints,
//! authenticated with the launcher's stored proxy token (`launcher.json`) — so
//! an agent can just shell out to `agent-portal message send …` with no extra
//! credentials. The message is delivered to the target session's agent as an
//! input turn, attributed with the sender's session id.

use anyhow::{anyhow, Context, Result};

use shared::api::{AgentSessionsResponse, SendAgentMessageRequest, SendAgentMessageResponse};

const SHORT_SESSION_ID_LEN: usize = 8;

/// The calling agent's own portal session id, read from whatever the agent
/// already exposes:
///
/// - `PORTAL_SESSION_ID` is the explicit portal-side override and therefore
///   always wins. In particular, Claude keeps its process environment across
///   `/clear`, so `CLAUDE_CODE_SESSION_ID` can still name the pre-clear
///   conversation while a caller deliberately supplies the current portal id.
/// - Claude Code sets `CLAUDE_CODE_SESSION_ID` to the id we spawn it with. It
///   is a useful fallback, but is only a spawn-time value.
/// - Codex sets `CODEX_THREAD_ID`, which is *not* the portal id, so we reverse
///   it through the launcher's `codex_threads.json` map to the portal session.
///
/// Returns `None` when none apply (e.g. a human shell), in which case the
/// recipient falls back to user attribution.
pub(crate) fn sender_session_id(
    sessions: &[shared::api::AgentSessionInfo],
) -> Result<Option<String>> {
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());

    if let Some(id) = env("PORTAL_SESSION_ID") {
        return Ok(Some(id));
    }
    if let Some(thread_id) = env("CODEX_THREAD_ID") {
        return Ok(
            crate::process_manager::session_id_for_codex_thread(&thread_id)
                .map(|id| id.to_string()),
        );
    }
    let Some(claude_id) = env("CLAUDE_CODE_SESSION_ID") else {
        return Ok(None);
    };
    let cwd = std::env::current_dir()
        .context("could not determine the current working directory")?
        .to_string_lossy()
        .into_owned();
    let hostname = claude_session_lib::hostname_or_unknown();
    resolve_claude_session_id(&claude_id, &cwd, &hostname, sessions).map(Some)
}

/// Validate Claude's spawn-time id against the authenticated session list. If
/// `/clear` replaced it, recover only when the caller's host + working
/// directory identify exactly one live Claude session. Refusing ambiguity is
/// important: guessing could mutate or attribute output to another session.
fn resolve_claude_session_id(
    claude_id: &str,
    cwd: &str,
    hostname: &str,
    sessions: &[shared::api::AgentSessionInfo],
) -> Result<String> {
    if sessions
        .iter()
        .any(|session| session.id.to_string() == claude_id)
    {
        return Ok(claude_id.to_string());
    }
    let candidates: Vec<_> = sessions
        .iter()
        .filter(|session| {
            session.agent_type == "claude"
                && session.status == shared::SessionStatus::Active.as_str()
                && session.working_directory == cwd
                && session.hostname == hostname
        })
        .collect();
    match candidates.as_slice() {
        [session] => Ok(session.id.to_string()),
        [] => Err(anyhow!(
            "Claude's session id {claude_id} is stale and no active portal session matches this host and working directory; set PORTAL_SESSION_ID explicitly"
        )),
        _ => Err(anyhow!(
            "Claude's session id {claude_id} is stale and {} active portal sessions match this host and working directory; set PORTAL_SESSION_ID explicitly",
            candidates.len()
        )),
    }
}

pub(crate) async fn current_session_id(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<String> {
    if let Some(id) = std::env::var("PORTAL_SESSION_ID")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return Ok(id);
    }
    let sessions = fetch_sessions(client, base, token).await?;
    sender_session_id(&sessions.sessions)?.ok_or_else(|| {
        anyhow!("run this from inside an agent session (no portal session id found)")
    })
}

/// Resolve the HTTP API base URL and auth token from the launcher config.
pub(crate) fn api_base() -> Result<(String, String)> {
    let config = crate::config::load_config();
    let token = config
        .auth_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("Not authenticated — run `agent-portal login` first"))?;
    let ws_url = config
        .backend_url
        .unwrap_or_else(|| shared::default_backend_url().to_string());
    // The config stores the WebSocket URL; the HTTP API shares the host.
    let http = ws_url
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    Ok((http.trim_end_matches('/').to_string(), token))
}

/// Max HTTP attempts (1 initial + retries) for the agent-messaging calls.
const MAX_ATTEMPTS: u32 = 4;
/// Base backoff before the first retry; doubles each attempt.
const BASE_BACKOFF_MS: u64 = 200;

/// Whether a failed attempt is worth retrying (#1388). Kept as a pure function,
/// separate from the async loop, so the classification is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryDecision {
    Retry,
    Stop,
}

/// Classify an HTTP status for retry.
///
/// `idempotent` gates 5xx retries: a GET (session list) is safe to replay on a
/// server error, but a POST that may already have delivered the message is not,
/// so a send only replays on the pre-delivery transient (a 404 from a stale
/// session index / read-after-write race, where the server found nothing and
/// did nothing) — never a 5xx that might double-send.
///
/// 400/401/403 are permanent (validation / auth / permission) and stop
/// immediately; retrying them just hides the real error behind a delay.
fn classify_status(status: reqwest::StatusCode, idempotent: bool) -> RetryDecision {
    if status.is_success() {
        return RetryDecision::Stop;
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return RetryDecision::Retry;
    }
    if status.is_server_error() && idempotent {
        return RetryDecision::Retry;
    }
    RetryDecision::Stop
}

/// Small non-cryptographic jitter in `0..=max` ms, seeded from the wall clock —
/// enough to spread concurrent agents' retries, no `rand` dependency needed.
fn jitter_ms(max: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    nanos % (max + 1)
}

/// Turn a permanent (non-retried) HTTP status into a user-facing error, with
/// login/permission guidance for auth failures.
fn permanent_error(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    use reqwest::StatusCode;
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => anyhow!(
            "not authorized ({status}) — run `agent-portal login`, or check you own the target session"
        ),
        _ => anyhow!("backend returned {status}: {}", body.trim()),
    }
}

/// Run `build` (which builds and sends a fresh request each call) with bounded
/// exponential backoff + jitter, retrying transient failures (transport errors,
/// 404, and — when `idempotent` — 5xx). Returns the successful response, or a
/// concise error including the attempt count and last status/body once retries
/// are exhausted or a permanent status is hit.
async fn request_with_retry<F, Fut>(idempotent: bool, mut build: F) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = reqwest::Result<reqwest::Response>>,
{
    let mut attempt = 1;
    loop {
        match build().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp);
                }
                if classify_status(status, idempotent) == RetryDecision::Stop {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(permanent_error(status, &body));
                }
                if attempt >= MAX_ATTEMPTS {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(anyhow!(
                        "backend returned {status} after {attempt} attempts: {}",
                        body.trim()
                    ));
                }
            }
            Err(e) => {
                // A transport error usually means the request never completed;
                // replay it up to the bound.
                if attempt >= MAX_ATTEMPTS {
                    return Err(anyhow::Error::new(e).context(format!(
                        "request to backend failed after {attempt} attempts"
                    )));
                }
            }
        }
        let base = BASE_BACKOFF_MS * (1u64 << (attempt - 1));
        tokio::time::sleep(std::time::Duration::from_millis(base + jitter_ms(base / 2))).await;
        attempt += 1;
    }
}

/// `agent-portal message list` — print the caller's sessions (agents).
pub async fn list() -> Result<()> {
    let (base, token) = api_base()?;
    let client = reqwest::Client::new();
    let data = fetch_sessions(&client, &base, &token).await?;
    if data.sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    // Self-marking is cosmetic; a stale/ambiguous caller must still be able to
    // inspect the session list and use an explicit id.
    let self_id = sender_session_id(&data.sessions).unwrap_or_else(|error| {
        eprintln!("warning: could not identify this session: {error}");
        None
    });
    for s in &data.sessions {
        println!(
            "{}",
            format_session_row(s, &data.sessions, self_id.as_deref())
        );
    }
    Ok(())
}

/// One `message list` row. Hostname sits before the working directory: hosts
/// discriminate rows that share a checkout path across machines.
fn format_session_row(
    session: &shared::api::AgentSessionInfo,
    sessions: &[shared::api::AgentSessionInfo],
    self_id: Option<&str>,
) -> String {
    let marker = if self_id == Some(&session.id.to_string()) {
        " (this session)"
    } else {
        ""
    };
    format!(
        "{}  {} / {} / {}  {}  {}  {}{}",
        display_session_id(session, sessions),
        full_agent_name(session),
        if session_is_connected(session) {
            "connected"
        } else {
            "disconnected"
        },
        if session.busy.unwrap_or(false) {
            "busy"
        } else {
            "idle"
        },
        session.session_name,
        session.hostname,
        session.working_directory,
        marker
    )
}

fn full_agent_name(session: &shared::api::AgentSessionInfo) -> String {
    let Some(model) = session.model.as_deref().filter(|model| !model.is_empty()) else {
        return session.agent_type.clone();
    };
    if model.starts_with(&session.agent_type) {
        model.to_string()
    } else {
        format!("{}-{model}", session.agent_type)
    }
}

/// New backends report live proxy presence explicitly. Falling back to the
/// legacy status keeps a newly-updated CLI useful against an older backend
/// during rolling deploys.
fn session_is_connected(session: &shared::api::AgentSessionInfo) -> bool {
    session
        .connected
        .unwrap_or_else(|| session.status == shared::SessionStatus::Active.as_str())
}

async fn fetch_sessions(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<AgentSessionsResponse> {
    // Idempotent GET: safe to replay on transport errors, 404 (stale session
    // index), and 5xx.
    let resp = request_with_retry(true, || {
        client
            .get(format!("{base}/api/agent/sessions"))
            .bearer_auth(token)
            .send()
    })
    .await?;
    resp.json().await.context("malformed response")
}

/// `agent-portal message send <agent-id> <message>` — deliver a message into a
/// session as an input turn.
pub async fn send(agent_id: &str, message: &str) -> Result<()> {
    let (base, token) = api_base()?;
    let client = reqwest::Client::new();
    let sessions = fetch_sessions(&client, &base, &token).await?;
    let resolved_agent_id = resolve_session_id(agent_id, &sessions.sessions)?;
    // Sender attribution is best-effort and must not block delivery when more
    // than one live session shares this host and working directory. But it
    // must never degrade SILENTLY: an unattributed agent send falls back to
    // the backend's plain "[portal message from <user>]" string and renders
    // in the recipient's transcript as if the human typed it.
    let from = match sender_session_id(&sessions.sessions) {
        Ok(Some(id)) => Some(id),
        Ok(None) => {
            eprintln!(
                "warning: sender session not identified (no PORTAL_SESSION_ID or \
                 recognized agent env) — this message will be attributed to your \
                 user account, not to this agent session"
            );
            None
        }
        Err(error) => {
            eprintln!("warning: could not attribute sender session: {error}");
            None
        }
    };
    // Non-idempotent POST: retry the pre-delivery transient (404 target lookup
    // against a stale session index) and transport errors, but NOT 5xx — the
    // server may already have delivered, and a replay would double-send.
    let resp = request_with_retry(false, || {
        client
            .post(format!(
                "{base}/api/agent/sessions/{resolved_agent_id}/message"
            ))
            .bearer_auth(&token)
            .json(&SendAgentMessageRequest {
                message: message.to_string(),
                from: from.clone(),
            })
            .send()
    })
    .await?;
    let data: SendAgentMessageResponse = resp.json().await.context("malformed response")?;
    if data.delivered {
        println!("Delivered (seq {}).", data.seq);
    } else {
        println!("Queued for the session's reconnect (seq {}).", data.seq);
    }
    if !data.persisted {
        println!("warning: message was not durably persisted for reconnect replay");
    }
    println!("Recipient pending input backlog: {}.", data.pending_inputs);
    Ok(())
}

fn short_session_id(id: &uuid::Uuid) -> String {
    id.simple()
        .to_string()
        .chars()
        .take(SHORT_SESSION_ID_LEN)
        .collect()
}

fn display_session_id(
    session: &shared::api::AgentSessionInfo,
    sessions: &[shared::api::AgentSessionInfo],
) -> String {
    let short = short_session_id(&session.id);
    let collision = sessions
        .iter()
        .filter(|candidate| short_session_id(&candidate.id) == short)
        .count()
        > 1;
    if collision {
        session.id.to_string()
    } else {
        short
    }
}

fn resolve_session_id(
    input: &str,
    sessions: &[shared::api::AgentSessionInfo],
) -> Result<uuid::Uuid> {
    let prefix = normalize_session_id_prefix(input)?;
    let matches = sessions
        .iter()
        .filter(|session| session.id.simple().to_string().starts_with(&prefix))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [session] => Ok(session.id),
        [] => Err(anyhow!("no session id matches `{}`", input.trim())),
        matches => {
            let ids = matches
                .iter()
                .map(|session| session.id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow!(
                "session id prefix `{}` is ambiguous; use more characters or a full id (matches: {})",
                input.trim(),
                ids
            ))
        }
    }
}

fn normalize_session_id_prefix(input: &str) -> Result<String> {
    let prefix = input.trim().replace('-', "").to_ascii_lowercase();
    if prefix.is_empty() {
        return Err(anyhow!("session id prefix cannot be empty"));
    }
    if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "session id prefix `{}` must contain only hex digits",
            input.trim()
        ));
    }
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::api::AgentSessionInfo;
    use uuid::Uuid;

    fn session(id: &str) -> AgentSessionInfo {
        AgentSessionInfo {
            id: Uuid::parse_str(id).expect("valid uuid"),
            session_name: "session".to_string(),
            working_directory: "/repo".to_string(),
            agent_type: "claude".to_string(),
            status: "active".to_string(),
            hostname: "host".to_string(),
            model: None,
            connected: Some(true),
            busy: Some(false),
            awaiting_permission: false,
            last_activity: String::new(),
        }
    }

    #[test]
    fn stale_claude_id_resolves_to_unique_live_session_on_same_host_and_cwd() {
        let sessions = vec![session("0c24805b-0000-0000-0000-000000000000")];
        assert_eq!(
            resolve_claude_session_id(
                "80740e1d-0000-0000-0000-000000000000",
                "/repo",
                "host",
                &sessions
            )
            .expect("unique replacement"),
            sessions[0].id.to_string()
        );
    }

    #[test]
    fn existing_claude_id_wins_without_inference() {
        let sessions = vec![session("80740e1d-0000-0000-0000-000000000000")];
        assert_eq!(
            resolve_claude_session_id(&sessions[0].id.to_string(), "/other", "other", &sessions)
                .expect("existing id"),
            sessions[0].id.to_string()
        );
    }

    #[test]
    fn stale_claude_id_refuses_ambiguous_live_sessions() {
        let sessions = vec![
            session("0c24805b-0000-0000-0000-000000000000"),
            session("1c24805b-0000-0000-0000-000000000000"),
        ];
        let error = resolve_claude_session_id(
            "80740e1d-0000-0000-0000-000000000000",
            "/repo",
            "host",
            &sessions,
        )
        .expect_err("ambiguous sessions must not be guessed");
        assert!(error.to_string().contains("2 active portal sessions"));
    }

    #[test]
    fn display_session_id_uses_short_prefix_without_collision() {
        let sessions = vec![
            session("12345678-0000-0000-0000-000000000000"),
            session("abcdef12-0000-0000-0000-000000000000"),
        ];

        assert_eq!(display_session_id(&sessions[0], &sessions), "12345678");
    }

    #[test]
    fn full_agent_name_keeps_full_model_and_avoids_duplicate_prefix() {
        let mut codex = session("12345678-0000-0000-0000-000000000000");
        codex.agent_type = "codex".to_string();
        codex.model = Some("gpt-5.4-sol".to_string());
        assert_eq!(full_agent_name(&codex), "codex-gpt-5.4-sol");

        let mut claude = codex;
        claude.agent_type = "claude".to_string();
        claude.model = Some("claude-opus-4-8".to_string());
        assert_eq!(full_agent_name(&claude), "claude-opus-4-8");
    }

    #[test]
    fn connection_presence_falls_back_to_legacy_status() {
        let mut old_wire = session("12345678-0000-0000-0000-000000000000");
        old_wire.connected = None;
        assert!(session_is_connected(&old_wire));
        old_wire.status = "disconnected".to_string();
        assert!(!session_is_connected(&old_wire));
    }

    #[test]
    fn session_row_lists_hostname_before_working_directory() {
        let sessions = vec![session("12345678-0000-0000-0000-000000000000")];

        assert_eq!(
            format_session_row(&sessions[0], &sessions, None),
            "12345678  claude / connected / idle  session  host  /repo"
        );
    }

    #[test]
    fn session_row_marks_callers_own_session() {
        let sessions = vec![session("12345678-0000-0000-0000-000000000000")];

        assert!(
            format_session_row(&sessions[0], &sessions, Some(&sessions[0].id.to_string()))
                .ends_with(" (this session)")
        );
    }

    #[test]
    fn display_session_id_uses_full_uuid_for_short_prefix_collision() {
        let sessions = vec![
            session("12345678-0000-0000-0000-000000000000"),
            session("12345678-ffff-0000-0000-000000000000"),
        ];

        assert_eq!(
            display_session_id(&sessions[0], &sessions),
            "12345678-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn resolve_session_id_accepts_unique_short_prefix() {
        let sessions = vec![
            session("12345678-0000-0000-0000-000000000000"),
            session("abcdef12-0000-0000-0000-000000000000"),
        ];

        assert_eq!(
            resolve_session_id("12345678", &sessions).expect("resolved"),
            sessions[0].id
        );
    }

    #[test]
    fn resolve_session_id_accepts_full_uuid() {
        let sessions = vec![session("12345678-0000-0000-0000-000000000000")];

        assert_eq!(
            resolve_session_id("12345678-0000-0000-0000-000000000000", &sessions)
                .expect("resolved"),
            sessions[0].id
        );
    }

    #[test]
    fn classify_status_retries_transient_for_idempotent_get() {
        use reqwest::StatusCode;
        // Stale index / read-after-write races and server errors are retryable
        // for a safe-to-replay GET.
        assert_eq!(
            classify_status(StatusCode::NOT_FOUND, true),
            RetryDecision::Retry
        );
        assert_eq!(
            classify_status(StatusCode::INTERNAL_SERVER_ERROR, true),
            RetryDecision::Retry
        );
        assert_eq!(
            classify_status(StatusCode::BAD_GATEWAY, true),
            RetryDecision::Retry
        );
        assert_eq!(
            classify_status(StatusCode::SERVICE_UNAVAILABLE, true),
            RetryDecision::Retry
        );
    }

    #[test]
    fn classify_status_does_not_replay_5xx_for_non_idempotent_send() {
        use reqwest::StatusCode;
        // A POST that may already have delivered must not replay a 5xx…
        assert_eq!(
            classify_status(StatusCode::INTERNAL_SERVER_ERROR, false),
            RetryDecision::Stop
        );
        assert_eq!(
            classify_status(StatusCode::BAD_GATEWAY, false),
            RetryDecision::Stop
        );
        // …but a pre-delivery 404 target lookup is still safe to retry.
        assert_eq!(
            classify_status(StatusCode::NOT_FOUND, false),
            RetryDecision::Retry
        );
    }

    #[test]
    fn classify_status_stops_on_permanent_and_success() {
        use reqwest::StatusCode;
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::OK,
        ] {
            assert_eq!(classify_status(status, true), RetryDecision::Stop);
            assert_eq!(classify_status(status, false), RetryDecision::Stop);
        }
    }

    #[test]
    fn resolve_session_id_rejects_ambiguous_prefix() {
        let sessions = vec![
            session("12345678-0000-0000-0000-000000000000"),
            session("12345678-ffff-0000-0000-000000000000"),
        ];

        let err = resolve_session_id("12345678", &sessions)
            .expect_err("ambiguous prefix should fail")
            .to_string();
        assert!(err.contains("ambiguous"));
        assert!(err.contains("12345678-0000-0000-0000-000000000000"));
        assert!(err.contains("12345678-ffff-0000-0000-000000000000"));
    }
}
