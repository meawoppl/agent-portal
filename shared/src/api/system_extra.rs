//! Settings and typed `SystemMessage.extra` per-subtype views.

use serde::{Deserialize, Serialize};

/// Response for GET /api/settings/sound
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundSettingsResponse {
    pub sound_config: Option<serde_json::Value>,
}

// =============================================================================
// Typed shapes for `SystemMessage.extra` per-subtype dispatch
//
// Closes agent-portal #752. The local lenient `SystemMessage` type used by the
// frontend renderer carries a `#[serde(flatten)] extra: Option<serde_json::Value>`
// for ad-hoc per-subtype metadata. Renderers previously poked `extra.get("…")`
// by field name; these structs are the typed mirrors so renderers can
// `serde_json::from_value::<T>(extra.clone())` once per branch and read named
// fields. The wire shape is unchanged — these structs are deserialize-only
// views over the same JSON bytes.
//
// Where a subtype is already fully covered upstream by `claude-codes`, the
// renderer uses the SDK type directly (`TaskNotificationMessage`,
// `TaskStartedMessage`). The remaining structs below cover gaps not yet
// represented in `claude-codes`:
//
// - `CompactionExtra` — `summary`, `leaf_message_count`/`message_count`,
//   `duration_ms`, `content`, `text`. Filed upstream as
//   `rust-code-agent-sdks#141`; the SDK's `CompactBoundaryMessage` currently
//   only exposes `compact_metadata { pre_tokens, trigger }`. Once upstream
//   lands these, this struct can be deleted and the renderer can switch to
//   `SystemMessage::as_compact_boundary()`.
// - `InitExtra` — `fast_mode_state`. The SDK's `InitMessage::fast_mode_state`
//   is already typed `Option<String>`; we keep a narrow local mirror because
//   `InitMessage` has many required fields and a single-field shape is
//   friendlier to partial frames the renderer encounters in practice.
// =============================================================================

/// Typed view of the `compact_boundary` subtype's `SystemMessage.extra`.
///
/// All fields are optional with `#[serde(default)]` so any wire shape that
/// omits them still deserializes (yielding `None`). Read priority for the
/// summary text is `summary` → `content` → `text` to match the historical
/// renderer fallback chain.
//
// TODO(SDK rust-code-agent-sdks#141): drop this struct once
// `claude_codes::CompactBoundaryMessage` exposes these fields directly and
// switch `render_compaction_completed` to `SystemMessage::as_compact_boundary`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionExtra {
    #[serde(default)]
    pub summary: Option<String>,
    /// Primary "messages summarized" count. CLI variants spell this
    /// `leaf_message_count` (preferred) or `message_count` (legacy).
    #[serde(default)]
    pub leaf_message_count: Option<u32>,
    #[serde(default)]
    pub message_count: Option<u32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Legacy aliases for the summary text — older CLI builds emitted under
    /// `content` or `text` instead of `summary`.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

impl CompactionExtra {
    /// First-non-empty summary text, mirroring the historical renderer fallback
    /// chain `summary` → `content` → `text`.
    pub fn summary_text(&self) -> Option<&str> {
        self.summary
            .as_deref()
            .or(self.content.as_deref())
            .or(self.text.as_deref())
    }

    /// First-set message count, preferring `leaf_message_count` over the
    /// legacy `message_count` spelling.
    pub fn message_count(&self) -> Option<u32> {
        self.leaf_message_count.or(self.message_count)
    }
}

/// Typed view of the `init` subtype's `SystemMessage.extra` for fields the
/// renderer needs that aren't already top-level on the local lenient
/// `SystemMessage`. `fast_mode_state` matches `claude_codes::InitMessage::fast_mode_state`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InitExtra {
    #[serde(default)]
    pub fast_mode_state: Option<String>,
}

/// Typed view of the `task_notification` subtype's `SystemMessage.extra`.
///
/// Mirrors the renderable subset of `claude_codes::TaskNotificationMessage`
/// (which requires `session_id` + `summary` — both already consumed by the
/// outer `SystemMessage`'s typed top-level fields, so they would not appear
/// in `extra` if we deserialized the SDK type directly). All fields optional
/// so partial frames (e.g. `failed` notifications without `usage` or
/// `tool_use_id`) still parse.
///
/// The nested `status` and `usage` types are re-used from `claude-codes` so
/// the wire shape stays in lockstep with the SDK enum.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskNotificationExtra {
    #[serde(default)]
    pub status: Option<claude_codes::io::TaskStatus>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub usage: Option<claude_codes::io::TaskUsage>,
}
