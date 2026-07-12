//! The `/privacy` page (mobile-apps plan §15, H1).
//!
//! Server-rendered, unauthenticated HTML — registered ahead of the SPA
//! fallback so crawlers, store reviewers, and logged-out users get a real
//! document without executing WASM. Every number on the page comes from the
//! deployment's live configuration (`AppState`), so the policy cannot drift
//! from what the server actually does; when the config changes, the page
//! changes with it.

use crate::AppState;
use axum::{extract::State, response::Html};
use std::sync::Arc;

/// Minimal HTML escape for config-sourced strings interpolated into the page.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// GET /privacy — how this deployment handles user data.
pub async fn privacy_policy(State(app_state): State<Arc<AppState>>) -> Html<String> {
    Html(render(
        &app_state.app_title,
        app_state.message_retention_count,
        app_state.message_retention_days,
        app_state.session_max_age_days,
        app_state.archive.is_some(),
    ))
}

/// Pure renderer, split from the handler so the config-to-copy mapping is
/// unit-testable (both enabled and disabled branches of each window).
fn render(
    app_title: &str,
    message_retention_count: i64,
    message_retention_days: u32,
    session_max_age_days: u32,
    archive_enabled: bool,
) -> String {
    let title = escape(app_title);

    let message_age_clause = if message_retention_days > 0 {
        format!(
            "and messages older than <strong>{message_retention_days} days</strong> are deleted"
        )
    } else {
        "with no age-based deletion configured".to_string()
    };

    let session_age_clause = if session_max_age_days > 0 {
        format!(
            "Sessions are deleted <strong>{session_max_age_days} days</strong> after their \
             last activity."
        )
    } else {
        "Automatic session deletion is not enabled on this deployment; sessions are \
         kept until you delete them."
            .to_string()
    };

    let archive_section = if archive_enabled {
        "<p>This deployment archives completed sessions to operator-controlled \
         long-term storage before the retention windows above remove them from the \
         live database. Archived data includes session metadata, usage totals, and \
         (when transcript archival is enabled) message content, and persists beyond \
         the live retention windows until the operator removes it.</p>"
    } else {
        "<p>Long-term session archival is <strong>not enabled</strong> on this \
         deployment: once the retention windows above remove data from the live \
         database, it is gone.</p>"
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Privacy — {title}</title>
<style>
  body {{ font-family: system-ui, -apple-system, sans-serif; background: #1a1b26;
         color: #c0caf5; max-width: 44rem; margin: 0 auto; padding: 2rem 1.25rem 4rem;
         line-height: 1.6; }}
  h1, h2 {{ color: #7aa2f7; line-height: 1.25; }}
  a {{ color: #7dcfff; }}
  strong {{ color: #e0af68; }}
  .meta {{ color: #565f89; font-size: 0.85rem; }}
</style>
</head>
<body>
<h1>{title} — Privacy</h1>
<p class="meta">This page is generated from this deployment's live configuration;
the retention numbers below are the values currently in effect.</p>

<h2>What this service stores</h2>
<ul>
  <li><strong>Account data</strong> — your Google account email and display name,
      used for sign-in and access control (allowlisting).</li>
  <li><strong>Session metadata</strong> — session names, working directories,
      hostnames, git branches, and pull-request URLs reported by connected
      agents.</li>
  <li><strong>Session content</strong> — the full conversation between you and
      your agents, including tool inputs and outputs (commands, file contents,
      code) that you or the agent surface in a session.</li>
  <li><strong>Usage metrics</strong> — per-turn token counts, model names,
      durations, and estimated cost.</li>
  <li><strong>Push subscriptions</strong> — if you enable notifications: a push
      endpoint or device token plus a device label, until you disable that
      device.</li>
</ul>

<h2>Retention</h2>
<p>Each session keeps its most recent <strong>{message_retention_count}</strong> messages
{message_age_clause}. {session_age_clause}</p>
{archive_section}

<h2>Push notifications</h2>
<p>Notifications are opt-in per device. A settings control determines how much
content they carry, from generic ("Permission needed") up to a short excerpt of
the request. Browser (Web Push) notification payloads are end-to-end encrypted
to your browser; Apple (APNs) and Google (FCM) notification payloads are
<strong>not</strong> end-to-end encrypted and transit those companies'
infrastructure in a form they can read.</p>

<h2>Visibility and sharing</h2>
<p>Your sessions are visible to you and to anyone you explicitly add as a
session member. Server administrators can see account data and usage metrics
across users, and — as operators of the database — have technical access to
stored session content.</p>

<h2>Third parties</h2>
<p>Google provides sign-in (OAuth). Apple and Google carry native push
notifications when configured. There is no advertising, no analytics service,
and no sale or sharing of data beyond the services above.</p>

<h2>Your controls</h2>
<ul>
  <li>Delete individual sessions (and their messages) from the dashboard.</li>
  <li>Disable push per device, and tune or silence notification content, in
      Settings.</li>
  <li>For account removal or questions about this deployment's data handling,
      contact the operator of this server.</li>
</ul>

<p class="meta">{title} is open source:
<a href="https://github.com/meawoppl/agent-portal">github.com/meawoppl/agent-portal</a>
· server version {version}</p>
</body>
</html>
"#,
        version = shared::VERSION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_configured_windows_and_archive_notice() {
        let html = render("Agent Portal", 100, 30, 14, true);
        assert!(html.contains("<strong>100</strong> messages"));
        assert!(html.contains("older than <strong>30 days</strong>"));
        assert!(html.contains("deleted <strong>14 days</strong> after"));
        assert!(html.contains("archives completed sessions"));
        assert!(html.contains(shared::VERSION));
    }

    #[test]
    fn renders_disabled_windows_and_no_archive() {
        let html = render("Agent Portal", 100, 0, 0, false);
        assert!(html.contains("no age-based deletion configured"));
        assert!(html.contains("kept until you delete them"));
        assert!(html.contains("not enabled</strong> on this"));
    }

    #[test]
    fn escapes_config_sourced_title() {
        let html = render("<script>alert(1)</script>", 1, 1, 1, false);
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
