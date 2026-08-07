//! `agent-portal forward` subcommands: expose a local HTTP port through the
//! portal (docs/PORT_FORWARDING.md). A thin client over the backend's
//! `/api/agent/sessions/{id}/forwards` endpoints, authenticated with the
//! launcher's stored proxy token — so an agent can shell out to
//! `agent-portal forward <port>` with no extra credentials and paste the
//! printed URL for the user.

use anyhow::{anyhow, Context, Result};

use shared::api::{CreateForwardRequest, CreateForwardResponse, SessionForwardsResponse};

/// Resolve the API base URL and auth token from launcher config (shared with
/// `message`).
fn api_base() -> Result<(String, String)> {
    let config = crate::config::load_config();
    let token = config
        .auth_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("Not authenticated — run `agent-portal login` first"))?;
    let ws_url = config
        .backend_url
        .unwrap_or_else(|| shared::default_backend_url().to_string());
    let http = ws_url
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    Ok((http.trim_end_matches('/').to_string(), token))
}

/// The calling agent's own portal session id (reuses `message`'s resolver, so
/// Claude / Codex / explicit-override all work).
async fn session_id(client: &reqwest::Client, base: &str, token: &str) -> Result<String> {
    crate::message::current_session_id(client, base, token).await
}

/// Outcome of a best-effort local probe of the origin the forward points at.
enum OriginProbe {
    /// Nothing accepted a TCP connection on the port.
    Refused,
    /// Got an HTTP response back.
    Responded {
        status_line: String,
        bytes: usize,
        elapsed_ms: u128,
        /// The origin closed the connection after responding (`true`) or held
        /// it open past our short close-probe deadline (`false`).
        closed: bool,
    },
    /// Connected, but couldn't get a usable HTTP response (timeout / error).
    Inconclusive(String),
}

const PROBE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);
/// Generous wait for the first byte (slow first-byte servers).
const PROBE_FIRST_BYTE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);
/// Short wait, after the response is in hand, to see whether the origin closes.
/// Bounds the penalty for the common keep-alive origin to this.
const PROBE_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

/// Best-effort local HTTP probe of `127.0.0.1:<port>`. The forward CLI runs on
/// the same host as the origin, so this sees exactly what the tunnel's dial
/// sees — crucially the failure the tunnel *can't* tell apart from a broken
/// origin: a server that keeps the connection open after a complete,
/// Content-Length-delimited response (an EOF-reading consumer hangs; a browser
/// honoring Content-Length would not). Never fails the command — it only
/// reports what it found (#1476).
async fn probe_origin(port: u16) -> OriginProbe {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let start = std::time::Instant::now();
    let mut stream = match tokio::time::timeout(
        PROBE_CONNECT_TIMEOUT,
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(_)) => return OriginProbe::Refused,
        Err(_) => return OriginProbe::Inconclusive("connect timed out".to_string()),
    };

    let req = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: agent-portal-forward-probe\r\nAccept: */*\r\n\r\n"
    );
    if let Err(e) = stream.write_all(req.as_bytes()).await {
        return OriginProbe::Inconclusive(format!("write failed: {e}"));
    }

    let mut buf = vec![0u8; 8192];
    let mut total = 0usize;
    let mut status_line = String::new();
    let mut closed = false;
    let mut first = true;
    loop {
        let deadline = if first {
            PROBE_FIRST_BYTE_TIMEOUT
        } else {
            PROBE_CLOSE_TIMEOUT
        };
        match tokio::time::timeout(deadline, stream.read(&mut buf)).await {
            Ok(Ok(0)) => {
                closed = true;
                break;
            }
            Ok(Ok(n)) => {
                if status_line.is_empty() {
                    status_line = String::from_utf8_lossy(&buf[..n])
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                }
                total += n;
                first = false;
                // Enough to know the shape; don't slurp a large body.
                if total >= 64 * 1024 {
                    break;
                }
            }
            Ok(Err(e)) => return OriginProbe::Inconclusive(format!("read failed: {e}")),
            // Deadline: if we already have bytes the origin is holding open;
            // if not, it never answered.
            Err(_) => break,
        }
    }

    if total == 0 {
        return OriginProbe::Inconclusive(
            "connected but no HTTP response before timeout".to_string(),
        );
    }
    OriginProbe::Responded {
        status_line,
        bytes: total,
        elapsed_ms: start.elapsed().as_millis(),
        closed,
    }
}

/// Print the origin probe as a one-line diagnostic (to stderr, so stdout stays
/// the single relayable URL). This is the line that collapses the "forward open
/// timed out" investigation into an obvious cause.
fn report_origin_probe(port: u16, probe: OriginProbe) {
    match probe {
        OriginProbe::Responded {
            status_line,
            bytes,
            elapsed_ms,
            closed,
        } => {
            let status = if status_line.is_empty() {
                "(non-HTTP response)".to_string()
            } else {
                status_line
            };
            eprintln!("  origin: {status}, {bytes} B in {elapsed_ms} ms");
            if !closed {
                eprintln!(
                    "  warning: origin held the connection open after responding; if the browser reports a timeout, set a short idle/socket timeout on your server"
                );
            }
        }
        OriginProbe::Refused => eprintln!(
            "  origin: nothing is listening on 127.0.0.1:{port} — start your server, then re-run"
        ),
        OriginProbe::Inconclusive(why) => eprintln!("  origin: probe inconclusive ({why})"),
    }
}

/// `agent-portal forward <port>` — register a forward and print its URL.
pub async fn open(port: u16) -> Result<()> {
    if port == 0 {
        return Err(anyhow!("port must be 1-65535"));
    }
    let (base, token) = api_base()?;
    let client = reqwest::Client::new();
    let session = session_id(&client, &base, &token).await?;

    // Probe the origin locally first (same host as the CLI) so we can report
    // exactly what the tunnel will see, before the round-trip to register.
    let probe = probe_origin(port).await;

    let resp = client
        .post(format!("{base}/api/agent/sessions/{session}/forwards"))
        .bearer_auth(&token)
        .json(&CreateForwardRequest { port })
        .send()
        .await
        .context("request to backend failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("backend returned {}: {}", status, body.trim()));
    }
    let data: CreateForwardResponse = resp.json().await.context("malformed response")?;

    // Exactly one URL line, so agents can relay it verbatim.
    println!("{}", data.forward.url);
    // A session forwards one port at a time; tell the caller when this moved
    // an existing forward off its old port.
    if let Some(old) = data.replaced_port {
        eprintln!(
            "note: replaced the existing forward on port {old}; only port {port} is forwarded now (front multiple services behind your own reverse proxy)"
        );
    }
    // The local probe supersedes the backend's boolean listening check: it runs
    // on the origin host and reports status/bytes/elapsed and whether the origin
    // closed the connection (#1476).
    report_origin_probe(port, probe);
    Ok(())
}

/// `agent-portal forward list` — active forwards for this session.
pub async fn list() -> Result<()> {
    let (base, token) = api_base()?;
    let client = reqwest::Client::new();
    let session = session_id(&client, &base, &token).await?;

    let resp = client
        .get(format!("{base}/api/agent/sessions/{session}/forwards"))
        .bearer_auth(&token)
        .send()
        .await
        .context("request to backend failed")?;
    if !resp.status().is_success() {
        return Err(anyhow!("backend returned {}", resp.status()));
    }
    let data: SessionForwardsResponse = resp.json().await.context("malformed response")?;
    if data.forwards.is_empty() {
        println!("No active forwards.");
    } else {
        for f in &data.forwards {
            println!(":{}  {}", f.port, f.url);
        }
    }
    // Recent failures go to stderr (diagnostic) so they don't pollute the URL
    // list on stdout, but the agent still sees what the browser hit (#1476).
    if !data.recent_failures.is_empty() {
        eprintln!("\nrecent forward failures (newest first):");
        for fail in &data.recent_failures {
            eprintln!("  {}  :{}  {}", fail.at, fail.port, fail.code);
        }
    }
    Ok(())
}

/// `agent-portal forward close` — revoke the session's forward.
pub async fn close() -> Result<()> {
    let (base, token) = api_base()?;
    let client = reqwest::Client::new();
    let session = session_id(&client, &base, &token).await?;

    let resp = client
        .delete(format!("{base}/api/agent/sessions/{session}/forwards"))
        .bearer_auth(&token)
        .send()
        .await
        .context("request to backend failed")?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Err(anyhow!("this session has no active forward"));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("backend returned {}: {}", status, body.trim()));
    }
    println!("Forward closed.");
    Ok(())
}
