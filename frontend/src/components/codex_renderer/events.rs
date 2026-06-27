use codex_codes::{
    io::items::{FileUpdateChange, ThreadItem},
    protocol::ThreadItem as AppServerThreadItem,
};
use serde::{Deserialize, Serialize};

// Local outer-`CodexEvent` enum is the project-specific wire shape: a hybrid
// of the codex exec JSONL format (`thread.started`, `item.started`, …) and
// the app-server JSON-RPC notifications (`turn/diff/updated`,
// `item/fileChange/patchUpdated`, …) projected into a single `tag = "type"`
// enum by the proxy. There is no upstream equivalent that covers both wire
// formats in one enum, so we keep this layer local.
//
// **Item-payload types and `FileUpdateChange` come from `codex-codes`** —
// the local `CodexItem` / `FileChange` / `TodoEntry` mirrors drifted from
// the actual wire shape and silently dropped `item.started{file_change}`
// frames (#827) because the local `kind` was typed as `Option<String>`
// while the wire ships `{"type": "update"}`. Reusing the SDK types makes
// the next schema bump a compile error rather than a silent runtime drop.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodexEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted {
        thread_id: Option<String>,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {},
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        usage: Option<CodexUsage>,
        #[serde(default, rename = "duration_ms", alias = "durationMs")]
        duration_ms: Option<u64>,
        #[serde(default, rename = "turn_id", alias = "turnId")]
        turn_id: Option<String>,
        #[serde(default)]
        status: Option<String>,
    },
    #[serde(rename = "turn.failed")]
    TurnFailed {
        error: Option<CodexError>,
    },
    #[serde(rename = "item.started")]
    ItemStarted {
        item: Option<CodexItem>,
    },
    #[serde(rename = "item.updated")]
    ItemUpdated {
        item: Option<CodexItem>,
    },
    #[serde(rename = "item.completed")]
    ItemCompleted {
        item: Option<CodexItem>,
    },
    Error {
        message: Option<String>,
    },
    // Streaming-delta / plan / diff notifications. The proxy forwards these as
    // `{"type": "<slash-named method>", "params": <inner-payload>}`. The
    // payload struct field names mirror codex-codes' generated types verbatim
    // (camelCase via the outer `rename_all = "snake_case"` would not match —
    // we provide aliases below for the camelCase wire format).
    #[serde(rename = "turn/diff/updated")]
    TurnDiffUpdated {
        params: Option<TurnDiffUpdatedParams>,
    },
    #[serde(rename = "item/fileChange/patchUpdated")]
    FileChangePatchUpdated {
        params: Option<FileChangePatchUpdatedParams>,
    },
    #[serde(rename = "turn/plan/updated")]
    TurnPlanUpdated {
        params: Option<TurnPlanUpdatedParams>,
    },
    #[serde(rename = "item/plan/delta")]
    PlanDelta {
        params: Option<PlanDeltaParams>,
    },
    #[serde(rename = "item/reasoning/summaryPartAdded")]
    ReasoningSummaryPartAdded {
        params: Option<serde_json::Value>,
    },
    #[serde(rename = "item/reasoning/textDelta")]
    ReasoningTextDelta {
        params: Option<serde_json::Value>,
    },
    #[serde(rename = "thread/compacted")]
    ThreadCompacted {
        params: Option<ContextCompactedParams>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodexItem {
    /// Exec-level item wrapper used by the stable codex renderer paths.
    Thread(ThreadItem),
    /// App-server item wrapper for item variants that the exec-level helper
    /// model intentionally does not expose yet, such as `contextCompaction`
    /// and `collabAgentToolCall`.
    AppServer(AppServerThreadItem),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnDiffUpdatedParams {
    #[serde(default)]
    pub diff: Option<String>,
    #[serde(default, rename = "threadId", alias = "thread_id")]
    pub thread_id: Option<String>,
    #[serde(default, rename = "turnId", alias = "turn_id")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileChangePatchUpdatedParams {
    /// Typed `Vec<FileUpdateChange>` from `codex-codes`; matches the
    /// `{"type": "update"}` object shape of `kind` on the wire — the local
    /// mirror's `Option<String>` for `kind` was the root cause of #827.
    #[serde(default)]
    pub changes: Option<Vec<FileUpdateChange>>,
    #[serde(default, rename = "itemId", alias = "item_id")]
    pub item_id: Option<String>,
    #[serde(default, rename = "threadId", alias = "thread_id")]
    pub thread_id: Option<String>,
    #[serde(default, rename = "turnId", alias = "turn_id")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnPlanUpdatedParams {
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub plan: Option<Vec<TurnPlanStep>>,
    #[serde(default, rename = "threadId", alias = "thread_id")]
    pub thread_id: Option<String>,
    #[serde(default, rename = "turnId", alias = "turn_id")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnPlanStep {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub step: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanDeltaParams {
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default, rename = "itemId", alias = "item_id")]
    pub item_id: Option<String>,
    #[serde(default, rename = "threadId", alias = "thread_id")]
    pub thread_id: Option<String>,
    #[serde(default, rename = "turnId", alias = "turn_id")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextCompactedParams {
    #[serde(default, rename = "threadId", alias = "thread_id")]
    pub thread_id: Option<String>,
    #[serde(default, rename = "turnId", alias = "turn_id")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexUsage {
    #[serde(default)]
    pub last: Option<CodexTokenUsage>,
    #[serde(default)]
    pub total: Option<CodexTokenUsage>,
    #[serde(default, rename = "model_context_window", alias = "modelContextWindow")]
    pub model_context_window: Option<u64>,
    // Legacy flat shape from older portal proxies.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub cached_input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexTokenUsage {
    #[serde(default, rename = "inputTokens", alias = "input_tokens")]
    pub input_tokens: Option<u64>,
    #[serde(default, rename = "cachedInputTokens", alias = "cached_input_tokens")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, rename = "outputTokens", alias = "output_tokens")]
    pub output_tokens: Option<u64>,
    #[serde(
        default,
        rename = "reasoningOutputTokens",
        alias = "reasoning_output_tokens"
    )]
    pub reasoning_output_tokens: Option<u64>,
    #[serde(default, rename = "totalTokens", alias = "total_tokens")]
    pub total_tokens: Option<u64>,
}

impl CodexUsage {
    pub(super) fn input_tokens(&self) -> u64 {
        self.last
            .as_ref()
            .and_then(|u| u.input_tokens)
            .or(self.input_tokens)
            .unwrap_or(0)
    }

    pub(super) fn cached_input_tokens(&self) -> u64 {
        self.last
            .as_ref()
            .and_then(|u| u.cached_input_tokens)
            .or(self.cached_input_tokens)
            .unwrap_or(0)
    }

    pub(super) fn output_tokens(&self) -> u64 {
        self.last
            .as_ref()
            .and_then(|u| u.output_tokens)
            .or(self.output_tokens)
            .unwrap_or(0)
    }

    pub(super) fn reasoning_output_tokens(&self) -> u64 {
        self.last
            .as_ref()
            .and_then(|u| u.reasoning_output_tokens)
            .unwrap_or(0)
    }

    pub(super) fn total_tokens(&self) -> u64 {
        self.last
            .as_ref()
            .and_then(|u| u.total_tokens)
            .unwrap_or_else(|| {
                self.input_tokens()
                    + self.cached_input_tokens()
                    + self.output_tokens()
                    + self.reasoning_output_tokens()
            })
    }

    pub(super) fn thread_total_tokens(&self) -> Option<u64> {
        self.total.as_ref().and_then(|u| u.total_tokens)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodexError {
    pub message: Option<String>,
}

/// Stable per-item identifier carried through the `started` → `updated` →
/// `completed` lifecycle. Used by the message-group dedup pass to collapse
/// progressive lifecycle events for the same item into a single rendered
/// card (#776 — bash commands were rendering twice as "running" +
/// "completed" cards).
///
/// Returns `&str` rather than wrapping the upstream `id: String` field
/// directly so the API mirrors the pre-typed-SDK `CodexItem::id()` method
/// for the existing call sites — no allocation, just a borrow.
pub fn thread_item_id(item: &ThreadItem) -> &str {
    match item {
        ThreadItem::UserMessage(it) => &it.id,
        ThreadItem::AgentMessage(it) => &it.id,
        ThreadItem::Reasoning(it) => &it.id,
        ThreadItem::CommandExecution(it) => &it.id,
        ThreadItem::FileChange(it) => &it.id,
        ThreadItem::McpToolCall(it) => &it.id,
        ThreadItem::WebSearch(it) => &it.id,
        ThreadItem::TodoList(it) => &it.id,
        ThreadItem::Error(it) => &it.id,
    }
}

pub fn codex_item_id(item: &CodexItem) -> Option<&str> {
    match item {
        CodexItem::Thread(it) => Some(thread_item_id(it)),
        CodexItem::AppServer(AppServerThreadItem::ContextCompaction { id }) => Some(id),
        CodexItem::AppServer(AppServerThreadItem::CollabAgentToolCall { id, .. }) => Some(id),
        CodexItem::AppServer(_) => None,
    }
}

/// Extract the `item_id` from a Codex event JSON, if it carries one. Returns
/// `None` for events without items (turn-level events, deltas, errors) or for
/// unparseable JSON. The group renderer uses this to dedupe progressive
/// `ItemStarted` / `ItemUpdated` / `ItemCompleted` events for the same item
/// into a single rendered card (#776).
pub fn codex_event_item_id(json: &str) -> Option<String> {
    let event: CodexEvent = serde_json::from_str(json).ok()?;
    match event {
        CodexEvent::ItemStarted { item }
        | CodexEvent::ItemUpdated { item }
        | CodexEvent::ItemCompleted { item } => codex_item_id(&item?).map(str::to_string),
        // Per-file patch updates (`item/fileChange/patchUpdated`) are cumulative
        // too — Codex re-sends the full file patch on every tick. Surfacing
        // their `item_id` here lets the group dedup keep only the final patch,
        // and collapses them against the matching `item.*{file_change}`
        // lifecycle events that carry the same id.
        CodexEvent::FileChangePatchUpdated { params } => params.and_then(|p| p.item_id),
        _ => None,
    }
}

/// Check if a Codex message indicates "awaiting" (turn complete or turn failed)
pub fn is_codex_terminal_event(json: &str) -> Option<bool> {
    let event: CodexEvent = serde_json::from_str(json).ok()?;
    match event {
        CodexEvent::TurnCompleted { .. } | CodexEvent::TurnFailed { .. } => Some(true),
        CodexEvent::ItemStarted { .. }
        | CodexEvent::ItemUpdated { .. }
        | CodexEvent::ItemCompleted { .. }
        | CodexEvent::TurnStarted { .. }
        | CodexEvent::ThreadStarted { .. } => Some(false),
        CodexEvent::Error { .. }
        | CodexEvent::TurnDiffUpdated { .. }
        | CodexEvent::FileChangePatchUpdated { .. }
        | CodexEvent::TurnPlanUpdated { .. }
        | CodexEvent::ThreadCompacted { .. }
        | CodexEvent::PlanDelta { .. }
        | CodexEvent::ReasoningSummaryPartAdded { .. }
        | CodexEvent::ReasoningTextDelta { .. }
        | CodexEvent::Unknown => None,
    }
}
