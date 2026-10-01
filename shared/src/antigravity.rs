//! Stable Portal envelopes around Antigravity's native protobuf-JSON updates.
//!
//! The native [`antigravity_codes::protocol::StepUpdate`] stays attached as
//! evidence, while Portal-owned discriminators and status/kind enums remain a
//! versioned contract shared by the launcher, backend, and WASM renderer.

use antigravity_codes::protocol::{
    StepUpdate, StepUpdateSource, StepUpdateState, StepUpdateTarget,
};
use serde::{Deserialize, Serialize};

pub const STEP_FRAME_TYPE: &str = "antigravity_step";
pub const TURN_COMPLETED_FRAME_TYPE: &str = "antigravity_turn_completed";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AntigravityStepKind {
    Message,
    ListDirectory,
    FindFile,
    SearchDirectory,
    ViewFile,
    CreateFile,
    EditFile,
    RunCommand,
    Compaction,
    InvokeSubagent,
    GenerateImage,
    SearchWeb,
    ReadUrlContent,
    McpTool,
    CustomTool,
    Finish,
    Error,
    ToolConfirmationRequest,
    QuestionsRequest,
    Other,
}

impl AntigravityStepKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Message => "Message",
            Self::ListDirectory => "List directory",
            Self::FindFile => "Find file",
            Self::SearchDirectory => "Search directory",
            Self::ViewFile => "View file",
            Self::CreateFile => "Create file",
            Self::EditFile => "Edit file",
            Self::RunCommand => "Run command",
            Self::Compaction => "Compaction",
            Self::InvokeSubagent => "Invoke subagent",
            Self::GenerateImage => "Generate image",
            Self::SearchWeb => "Search web",
            Self::ReadUrlContent => "Read URL",
            Self::McpTool => "MCP tool",
            Self::CustomTool => "Custom tool",
            Self::Finish => "Finish",
            Self::Error => "Error",
            Self::ToolConfirmationRequest => "Tool confirmation",
            Self::QuestionsRequest => "Questions",
            Self::Other => "Step",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AntigravityStepEnvelope {
    #[serde(rename = "type")]
    pub frame_type: String,
    pub trajectory_id: String,
    pub step_index: u32,
    pub kind: AntigravityStepKind,
    pub state: StepUpdateState,
    pub source: StepUpdateSource,
    pub target: StepUpdateTarget,
    pub text: String,
    pub thinking: String,
    pub error_message: Option<String>,
    pub update: StepUpdate,
}

impl AntigravityStepEnvelope {
    pub fn is_valid(&self) -> bool {
        self.frame_type == STEP_FRAME_TYPE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AntigravityTurnStatus {
    Completed,
    Cancelled,
    Failed,
}

impl AntigravityTurnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntigravityTurnCompletedEnvelope {
    #[serde(rename = "type")]
    pub frame_type: String,
    pub status: AntigravityTurnStatus,
}

impl AntigravityTurnCompletedEnvelope {
    pub fn new(status: AntigravityTurnStatus) -> Self {
        Self {
            frame_type: TURN_COMPLETED_FRAME_TYPE.to_string(),
            status,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.frame_type == TURN_COMPLETED_FRAME_TYPE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_wire_shape_is_stable() {
        let value = serde_json::to_value(AntigravityTurnCompletedEnvelope::new(
            AntigravityTurnStatus::Cancelled,
        ))
        .unwrap();
        assert_eq!(value["type"], TURN_COMPLETED_FRAME_TYPE);
        assert_eq!(value["status"], "cancelled");
        let parsed: AntigravityTurnCompletedEnvelope = serde_json::from_value(value).unwrap();
        assert!(parsed.is_valid());
    }
}
