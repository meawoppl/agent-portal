//! Agent-driven progress bars (`agent-portal progress`).
//!
//! The launcher CLI posts an [`AgentProgressRequest`]; the backend keeps the
//! session's live bars in memory and fans the full set out to web clients as
//! `ServerToClient::AgentProgress`. Bars are live status, never transcript.

use serde::{Deserialize, Serialize};

/// Bar id used when the agent doesn't pick one.
pub const DEFAULT_PROGRESS_ID: &str = "default";

/// Most bars a session may show at once.
pub const MAX_PROGRESS_BARS: usize = 8;

/// Longest accepted bar id, in bytes.
pub const MAX_PROGRESS_ID_LEN: usize = 64;

/// Longest accepted bar label, in bytes.
pub const MAX_PROGRESS_LABEL_LEN: usize = 200;

/// One live progress bar as shown to the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressBar {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Completion in `0.0..=1.0`; `None` renders an indeterminate bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f32>,
}

/// `POST /api/agent/sessions/{id}/progress` — create or update a bar, or
/// remove it when `clear` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentProgressRequest {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f32>,
    #[serde(default)]
    pub clear: bool,
}

/// Parse a CLI progress value into a completion fraction.
///
/// Accepts `37` (out of `max`), `37%`, `0.5%`, and `3/12`. The result is
/// clamped to `0.0..=1.0`; malformed or non-finite input, and a zero
/// denominator, are errors.
pub fn parse_progress_fraction(value: &str, max: f64) -> Result<f32, String> {
    let value = value.trim();
    let number = |s: &str| {
        s.trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .ok_or_else(|| format!("`{value}` is not a number, percentage, or n/m fraction"))
    };
    let fraction = if let Some(pct) = value.strip_suffix('%') {
        number(pct)? / 100.0
    } else if let Some((done, total)) = value.split_once('/') {
        let total = number(total)?;
        if total <= 0.0 {
            return Err("the denominator must be greater than zero".to_string());
        }
        number(done)? / total
    } else {
        if max <= 0.0 || !max.is_finite() {
            return Err("--max must be greater than zero".to_string());
        }
        number(value)? / max
    };
    Ok(fraction.clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_number_against_max() {
        assert_eq!(parse_progress_fraction("37", 100.0), Ok(0.37));
        assert_eq!(parse_progress_fraction("5", 20.0), Ok(0.25));
    }

    #[test]
    fn parses_percent_and_ratio() {
        assert_eq!(parse_progress_fraction("50%", 100.0), Ok(0.5));
        assert_eq!(parse_progress_fraction("3/12", 100.0), Ok(0.25));
        assert_eq!(parse_progress_fraction(" 1 / 4 ", 100.0), Ok(0.25));
    }

    #[test]
    fn clamps_out_of_range() {
        assert_eq!(parse_progress_fraction("150", 100.0), Ok(1.0));
        assert_eq!(parse_progress_fraction("-5", 100.0), Ok(0.0));
        assert_eq!(parse_progress_fraction("15/12", 100.0), Ok(1.0));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse_progress_fraction("abc", 100.0).is_err());
        assert!(parse_progress_fraction("NaN", 100.0).is_err());
        assert!(parse_progress_fraction("inf%", 100.0).is_err());
        assert!(parse_progress_fraction("3/0", 100.0).is_err());
        assert!(parse_progress_fraction("3", 0.0).is_err());
    }
}
