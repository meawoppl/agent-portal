//! Long polling waits on backend activity, not a client peek timer. Output
//! checkpoints advance only after a whole batch has been written and flushed.
use anyhow::{Context, Result};
use shared::api::{FollowEvent, FollowPage, FollowRecord, FollowRecordKind, SessionActivityState};
use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

#[derive(clap::Args, Debug)]
pub struct Options {
    #[arg(long)]
    pub since: Option<String>,
    /// Comma-separated kinds. Tool bodies and thinking are excluded by default.
    #[arg(long, default_value = "text,status,send,error")]
    pub only: String,
    #[arg(long)]
    pub grep: Option<String>,
    #[arg(long, default_value="2s", value_parser=duration)]
    pub batch_quiet: Duration,
    #[arg(long, default_value_t = 20)]
    pub batch_count: usize,
    #[arg(long, default_value_t = 4000)]
    pub batch_chars: usize,
    #[arg(long, default_value="15s", value_parser=duration)]
    pub max_wait: Duration,
    #[arg(long, default_value="60s", value_parser=duration)]
    pub heartbeat: Duration,
    #[arg(long)]
    pub json: bool,
    /// Exit on a connected peer's observed active → idle transition
    #[arg(long)]
    pub until_idle: bool,
}
fn duration(s: &str) -> Result<Duration, String> {
    let d = humantime::parse_duration(s).map_err(|e| e.to_string())?;
    if d < Duration::from_millis(100) || d > Duration::from_secs(3600) {
        return Err("duration must be between 100ms and 1h".into());
    }
    Ok(d)
}

#[derive(Default)]
struct IdleCompletion {
    pending: bool,
}
impl IdleCompletion {
    fn observe(&mut self, previous: Option<SessionActivityState>, page: &FollowPage) -> bool {
        if !page.connected || page.state.is_active() {
            self.pending = false;
        } else if previous.is_some_and(SessionActivityState::is_active) {
            self.pending = true;
        }
        self.pending && !page.more
    }
}

struct Batch {
    events: Vec<FollowEvent>,
    first: Instant,
    last: Instant,
    chars: usize,
}
impl Batch {
    fn new() -> Self {
        Self {
            events: vec![],
            first: Instant::now(),
            last: Instant::now(),
            chars: 0,
        }
    }
    fn push(&mut self, event: FollowEvent) {
        if self.events.is_empty() {
            self.first = Instant::now();
        }
        self.last = Instant::now();
        self.chars += event.text.chars().count();
        self.events.push(event);
    }
    fn full(&self, options: &Options) -> bool {
        self.events.len() >= options.batch_count || self.chars >= options.batch_chars
    }
    fn deadline(&self, options: &Options, heartbeat: Instant) -> Instant {
        if self.events.is_empty() {
            heartbeat
        } else {
            heartbeat
                .min(self.first + options.max_wait)
                .min(self.last + options.batch_quiet)
        }
    }
}

fn emit(
    kind: FollowRecordKind,
    cursor: &str,
    state: SessionActivityState,
    connected: bool,
    events: &[FollowEvent],
    json: bool,
) -> Result<()> {
    // One record is capped at 100 summaries of at most 240 characters each.
    let record = FollowRecord {
        kind,
        cursor: cursor.to_owned(),
        start_cursor: events.first().map(|e| e.cursor.clone()),
        state,
        connected,
        events: events.to_vec(),
    };
    let mut out = io::stdout().lock();
    if json {
        serde_json::to_writer(&mut out, &record)?;
        writeln!(out)?;
    } else {
        let digest = events
            .iter()
            .map(|e| format!("{}: {}", e.kind, e.text))
            .collect::<Vec<_>>()
            .join(" | ");
        let kind = kind.as_str();
        writeln!(
            out,
            "[{kind} {state:?} connected={connected} cursor={cursor}] {digest}"
        )?;
    }
    out.flush()?;
    Ok(())
}

pub async fn run(agent_id: &str, options: Options) -> Result<()> {
    anyhow::ensure!(
        (1..=100).contains(&options.batch_count),
        "batch-count must be 1–100"
    );
    anyhow::ensure!(
        (1..=24000).contains(&options.batch_chars),
        "batch-chars must be 1–24000"
    );
    let kinds: Vec<_> = options.only.split(',').collect();
    anyhow::ensure!(
        kinds.iter().all(|k| [
            "text",
            "send",
            "status",
            "error",
            "tool",
            "tool_result",
            "thinking",
            "input",
            "system",
            "unknown"
        ]
        .contains(k)),
        "unsupported kind; use text,status,send,error,tool,tool_result,thinking,input,system,unknown"
    );
    let regex = options
        .grep
        .as_ref()
        .map(|p| regex::Regex::new(p))
        .transpose()
        .context("invalid grep expression")?;
    let (base, token) = super::api_base()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(40))
        .build()?;
    let sessions = super::fetch_sessions(&client, &base, &token).await?;
    let id = super::resolve_session_id(agent_id, &sessions.sessions)?;
    let mut cursor = options.since.clone();
    let mut state = None;
    let mut connected = None;
    let mut batch = Batch::new();
    let mut heartbeat = Instant::now() + options.heartbeat;
    let mut failures = 0u32;
    let mut more = false;
    let mut idle_completion = IdleCompletion::default();
    loop {
        let deadline = batch.deadline(&options, heartbeat);
        let wait = if more {
            0
        } else {
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(30_000) as u64
        };
        let mut query = vec![("wait_ms", wait.to_string())];
        if let Some(ref cursor) = cursor {
            query.push(("since", cursor.clone()));
        }
        if let Some(state) = state {
            query.push((
                "state",
                serde_json::to_value(state)?
                    .as_str()
                    .context("state not string")?
                    .to_string(),
            ));
        }
        if let Some(connected) = connected {
            query.push(("connected", format!("{connected}")));
        }
        let response = client
            .get(format!("{base}/api/agent/sessions/{id}/follow"))
            .bearer_auth(&token)
            .query(&query)
            .timeout(Duration::from_millis(wait + 5000))
            .send()
            .await;
        let page: FollowPage = match response {
            Ok(response) if response.status().is_success() => {
                response.json().await.context("invalid follow response")?
            }
            Ok(response) if response.status().is_client_error() => anyhow::bail!(
                "follow stopped ({}): {}",
                response.status(),
                response.text().await?
            ),
            other => {
                failures += 1;
                eprintln!(
                    "follow disconnected; retry {failures}; last fetched cursor (not a checkpoint) {:?}: {}",
                    cursor,
                    match other {
                        Ok(r) => r.status().to_string(),
                        Err(e) => e.to_string(),
                    }
                );
                // Never print a healthy heartbeat while the server is unreachable.
                tokio::time::sleep(Duration::from_secs((1u64 << failures.min(5)).min(30))).await;
                continue;
            }
        };
        failures = 0;
        let changed = state != Some(page.state) || connected != Some(page.connected);
        let completed_idle = idle_completion.observe(state, &page);
        for event in page.events {
            cursor = Some(event.cursor.clone());
            if kinds.contains(&event.kind.as_str())
                && regex.as_ref().is_none_or(|r| r.is_match(&event.text))
            {
                batch.push(event);
            }
            if batch.full(&options) {
                emit(
                    FollowRecordKind::Batch,
                    cursor.as_deref().unwrap(),
                    page.state,
                    page.connected,
                    &batch.events,
                    options.json,
                )?;
                batch = Batch::new();
            }
        }
        cursor = Some(page.cursor.clone());
        state = Some(page.state);
        connected = Some(page.connected);
        more = page.more;
        let now = Instant::now();
        let due = now >= batch.deadline(&options, heartbeat);
        if changed || now >= heartbeat || (!batch.events.is_empty() && due) {
            if !batch.events.is_empty() {
                emit(
                    FollowRecordKind::Batch,
                    &page.cursor,
                    page.state,
                    page.connected,
                    &batch.events,
                    options.json,
                )?;
                batch = Batch::new();
            }
            if changed && kinds.contains(&"status") {
                emit(
                    FollowRecordKind::Status,
                    &page.cursor,
                    page.state,
                    page.connected,
                    &[],
                    options.json,
                )?;
            }
            if now >= heartbeat {
                emit(
                    FollowRecordKind::Heartbeat,
                    &page.cursor,
                    page.state,
                    page.connected,
                    &[],
                    options.json,
                )?;
                heartbeat = now + options.heartbeat;
            }
        }
        if options.until_idle && completed_idle {
            if !batch.events.is_empty() {
                emit(
                    FollowRecordKind::Batch,
                    &page.cursor,
                    page.state,
                    page.connected,
                    &batch.events,
                    options.json,
                )?;
            }
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[derive(Parser)]
    struct Command {
        #[command(flatten)]
        options: Options,
    }
    fn options() -> Options {
        Command::parse_from(["follow"]).options
    }
    fn event(text: &str) -> FollowEvent {
        FollowEvent {
            cursor: "cursor".into(),
            message_id: uuid::Uuid::new_v4(),
            kind: "text".into(),
            text: text.into(),
        }
    }
    #[test]
    fn defaults_are_bounded_and_exclude_tool_bodies() {
        let options = options();
        assert_eq!(options.only, "text,status,send,error");
        assert_eq!(options.batch_count, 20);
        assert_eq!(options.heartbeat, Duration::from_secs(60));
        assert!(Command::try_parse_from(["follow", "--batch-quiet", "0s"]).is_err());
        assert!(Command::try_parse_from(["follow", "--heartbeat", "2h"]).is_err());
    }
    #[test]
    fn quiet_and_max_wait_deadlines_and_count_character_caps() {
        let mut options = options();
        let mut batch = Batch::new();
        let heartbeat = Instant::now() + Duration::from_secs(60);
        assert_eq!(batch.deadline(&options, heartbeat), heartbeat);
        batch.push(event("Hello"));
        assert_eq!(
            batch.deadline(&options, heartbeat),
            batch.last + options.batch_quiet
        );
        batch.first = Instant::now() - options.max_wait;
        assert_eq!(
            batch.deadline(&options, heartbeat),
            batch.first + options.max_wait
        );
        options.batch_count = 2;
        assert!(!batch.full(&options));
        batch.push(event("world"));
        assert!(batch.full(&options));
        options.batch_count = 100;
        options.batch_chars = 10;
        assert!(batch.full(&options));
    }
    #[test]
    fn idle_completion_drains_more_than_one_page_and_does_not_exit_on_initial_idle() {
        let mut completion = IdleCompletion::default();
        let mut page = FollowPage {
            cursor: "100".into(),
            events: (0..100).map(|_| event("line")).collect(),
            state: SessionActivityState::Idle,
            connected: true,
            more: true,
        };
        assert!(!completion.observe(Some(SessionActivityState::Busy), &page));
        page.cursor = "125".into();
        page.events.truncate(25);
        page.more = false;
        assert!(completion.observe(Some(SessionActivityState::Idle), &page));
        assert!(!IdleCompletion::default().observe(None, &page));
        page.connected = false;
        assert!(!completion.observe(Some(SessionActivityState::Busy), &page));
    }
}
