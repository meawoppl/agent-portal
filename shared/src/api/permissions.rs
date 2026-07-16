//! Permission request payload types.

use std::collections::BTreeMap;

use codex_codes::{
    FileChange, ParsedCommand, RequestPermissionProfile, ToolRequestUserInputQuestion,
};
use serde::{Deserialize, Serialize};

/// Typed envelope for a Codex permission request's `input` payload.
///
/// Replaces the prior `serde_json::Value` envelope shared between the proxy
/// (Codex app-server bridge in `codex-session-lib/src/handler.rs`) and the
/// frontend's permission dialog (`frontend/src/pages/dashboard/types.rs`).
/// Both sides used to JSON-poke field names ("itemId", "fileChanges",
/// "serverName", …); this enum makes the contract a compile-time one.
///
/// Closes #725 (proxy-side typed write) and #731 (frontend-side typed read).
///
/// # Wire shape
///
/// Serializes with a `tool` discriminant in camelCase:
///
/// ```json
/// {"tool": "fileChange", "itemId": "call_…", "paths": ["src/main.rs"], "reason": null, "grantRoot": null}
/// {"tool": "applyPatch", "fileChanges": {…}, "grantRoot": null, "reason": null}
/// {"tool": "bash", "command": "ls -la", "cwd": "/tmp"}
/// {"tool": "execCommand", "command": "ls -la", "cwd": "/tmp", "parsedCmd": [...]}
/// {"tool": "permissions", "cwd": "/tmp", "permissions": {…}, "reason": null}
/// {"tool": "mcpElicitation", "serverName": "…"}
/// {"tool": "askUserQuestion", "questions": [...]}
/// ```
///
/// The variant is the only authoritative source of the tool kind — the
/// human-readable string ("FileChange", "Bash", etc.) the frontend used to
/// dispatch on is derived from the variant via [`Self::tool_name`] so callers
/// that still want a stringly-typed CSS / sorting key can keep getting one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "camelCase")]
pub enum CodexPermissionInput {
    /// Codex `item/fileChange/requestApproval` — file-modification approval.
    /// The actual diff streamed earlier under the matching `itemId`; the
    /// frontend may enrich the request with `paths` resolved from that item.
    FileChange {
        #[serde(rename = "itemId")]
        item_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(rename = "grantRoot", default, skip_serializing_if = "Option::is_none")]
        grant_root: Option<String>,
    },
    /// Codex `applyPatchApproval` — apply-patch approval (0.130+).
    ApplyPatch {
        #[serde(rename = "fileChanges", default)]
        file_changes: BTreeMap<String, FileChange>,
        #[serde(rename = "grantRoot", default, skip_serializing_if = "Option::is_none")]
        grant_root: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Codex `item/commandExecution/requestApproval` — single-string command form.
    Bash {
        #[serde(default)]
        command: String,
        #[serde(default)]
        cwd: String,
        #[serde(rename = "parsedCmd", default, skip_serializing_if = "Option::is_none")]
        parsed_cmd: Option<Vec<ParsedCommand>>,
    },
    /// Codex `execCommandApproval` (0.130+) — argv-vector command form.
    ExecCommand {
        #[serde(default)]
        command: String,
        #[serde(default)]
        cwd: String,
        #[serde(rename = "parsedCmd", default, skip_serializing_if = "Option::is_none")]
        parsed_cmd: Option<Vec<ParsedCommand>>,
    },
    /// Codex `item/permissions/requestApproval` — broader permission profile.
    Permissions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permissions: Option<RequestPermissionProfile>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Codex `mcpServer/elicitation/request` — MCP server prompt.
    /// Wire shape today only carries `serverName`; the upstream typed enum
    /// is richer (Form / Url variants) but the proxy hasn't surfaced those
    /// fields yet — preserved as-is to avoid changing user-visible output.
    McpElicitation {
        #[serde(rename = "serverName", default)]
        server_name: String,
    },
    /// Codex `item/tool/requestUserInput` — reuses the Claude
    /// AskUserQuestion renderer; the Codex question shape is structurally
    /// compatible with Claude's so the frontend keeps a single dispatch.
    AskUserQuestion {
        #[serde(default)]
        questions: Vec<ToolRequestUserInputQuestion>,
    },
}

impl CodexPermissionInput {
    /// Human-readable tool name matching the historical `tool_name` strings
    /// the wire used to carry alongside this payload. Kept stable for the
    /// frontend's CSS / sort keys and for the existing `PendingPermission`
    /// debug logs.
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::FileChange { .. } => "FileChange",
            Self::ApplyPatch { .. } => "ApplyPatch",
            Self::Bash { .. } => "Bash",
            Self::ExecCommand { .. } => "ExecCommand",
            Self::Permissions { .. } => "Permissions",
            Self::McpElicitation { .. } => "McpElicitation",
            Self::AskUserQuestion { .. } => "AskUserQuestion",
        }
    }
}
