//! `cat` — print a readable transcript digest (or the raw NDJSON) for one
//! archived session.

use anyhow::{anyhow, Result};
use archive_format::{transcript_key, zstd_decode, ArchiveMessageLine, ArchiveStore};
use chrono::{Local, TimeZone, Utc};
use uuid::Uuid;

use crate::summarize::summarize_content;

/// Read the session's transcript and render it. With `raw`, dumps the stored
/// NDJSON verbatim; otherwise renders the one-line-per-message digest. Missing
/// transcripts return a friendly note rather than an error.
pub fn run(store: &ArchiveStore, user_id: Uuid, session_id: Uuid, raw: bool) -> Result<String> {
    if raw {
        return match store
            .get_object(&transcript_key(user_id, session_id))
            .map_err(|e| anyhow!("failed to read transcript: {e}"))?
        {
            Some(bytes) => {
                let ndjson = zstd_decode(&bytes)
                    .map_err(|e| anyhow!("failed to decompress transcript: {e}"))?;
                Ok(String::from_utf8_lossy(&ndjson).trim_end().to_string())
            }
            None => Ok("(no transcript archived for this session)".to_string()),
        };
    }

    match store
        .read_transcript_lines(user_id, session_id)
        .map_err(|e| anyhow!("failed to read transcript: {e}"))?
    {
        Some(lines) if !lines.is_empty() => Ok(digest(&lines)),
        Some(_) => Ok("(transcript archived but empty)".to_string()),
        None => Ok("(no transcript archived for this session)".to_string()),
    }
}

/// Render a full digest: one line per message.
pub fn digest(lines: &[ArchiveMessageLine]) -> String {
    lines.iter().map(digest_line).collect::<Vec<_>>().join("\n")
}

/// One digest line: `local-timestamp  role  summary`. The stored timestamp is
/// UTC-naive; it is converted to the viewer's local timezone for display.
fn digest_line(line: &ArchiveMessageLine) -> String {
    let ts = Utc
        .from_utc_datetime(&line.created_at)
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S");
    format!(
        "{ts}  {:<9}  {}",
        line.role,
        summarize_content(&line.content)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use serde_json::json;

    fn line(role: &str, content: serde_json::Value) -> ArchiveMessageLine {
        ArchiveMessageLine {
            id: Uuid::new_v4(),
            role: role.to_string(),
            created_at: NaiveDate::from_ymd_opt(2026, 7, 11)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            agent_type: "claude".to_string(),
            content,
        }
    }

    #[test]
    fn digest_has_one_line_per_message_with_role_and_summary() {
        let lines = vec![
            line("user", json!("please refactor this")),
            line(
                "assistant",
                json!({"content": [{"type": "text", "text": "on it"},
                                   {"type": "tool_use", "name": "Edit"}]}),
            ),
        ];
        let out = digest(&lines);
        let rendered: Vec<&str> = out.lines().collect();
        assert_eq!(rendered.len(), 2);
        assert!(rendered[0].contains("user"));
        assert!(rendered[0].contains("please refactor this"));
        assert!(rendered[1].contains("assistant"));
        assert!(rendered[1].contains("on it"));
        assert!(rendered[1].contains("[tool_use: Edit]"));
    }

    #[test]
    fn digest_skips_thinking_in_summary() {
        let lines = vec![line(
            "assistant",
            json!([{"type": "thinking", "thinking": "hidden"},
                   {"type": "text", "text": "shown"}]),
        )];
        let out = digest(&lines);
        assert!(out.contains("shown"));
        assert!(!out.contains("hidden"));
    }
}
