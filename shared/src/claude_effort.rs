//! Claude composer capability and resolved effort, separate from preferences.

use crate::{ClaudeOutput, ReasoningEffort};
use serde::{Deserialize, Serialize};

/// Portal-owned annotation on init frames from drivers that apply overrides.
/// Native init fields alone cannot prove an older portal driver honors them.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ClaudeEffortCapability {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub portal_effort_control: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeEffortStatus {
    pub control_supported: bool,
    pub reported_effort: Option<String>,
    pub ultrathink_available: bool,
}

impl ClaudeEffortStatus {
    pub fn observe(&mut self, output: &ClaudeOutput, capability: ClaudeEffortCapability) {
        if let ClaudeOutput::System(system) = output {
            if let Some(init) = system.as_init() {
                self.control_supported = capability.portal_effort_control;
                self.reported_effort = init.effort;
                self.ultrathink_available = init.ultrathink_available == Some(true);
            } else if system.is_per_turn_effort_changed() {
                self.ultrathink_available = false;
            }
        }
    }

    pub fn permits(&self, effort: ReasoningEffort) -> bool {
        self.control_supported
            && match effort {
                ReasoningEffort::Minimal => false,
                ReasoningEffort::Xhigh => self.ultrathink_available,
                ReasoningEffort::Low | ReasoningEffort::Medium | ReasoningEffort::High => true,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() -> ClaudeOutput {
        serde_json::from_str(r#"{"type":"system","subtype":"init","session_id":"s","uuid":"u","cwd":"/tmp","tools":[],"mcp_servers":[],"model":"claude-opus-4-8","permissionMode":"default","apiKeySource":"user","effort":"high","ultrathink_available":true}"#).unwrap()
    }

    #[test]
    fn native_init_alone_does_not_enable_an_old_portal_driver() {
        let mut status = ClaudeEffortStatus::default();
        status.observe(&init(), ClaudeEffortCapability::default());
        assert_eq!(status.reported_effort.as_deref(), Some("high"));
        assert!(!status.permits(ReasoningEffort::High));
    }

    #[test]
    fn downgrade_disables_xhigh_until_the_next_init() {
        let mut status = ClaudeEffortStatus::default();
        let capable = ClaudeEffortCapability {
            portal_effort_control: true,
        };
        status.observe(&init(), capable);
        assert!(status.permits(ReasoningEffort::High));
        assert!(status.permits(ReasoningEffort::Xhigh));
        assert!(!status.permits(ReasoningEffort::Minimal));
        let changed = serde_json::from_str(r#"{"type":"system","subtype":"per_turn_effort_changed","session_id":"s","uuid":"u","per_turn_effort_active":false}"#).unwrap();
        status.observe(&changed, ClaudeEffortCapability::default());
        assert!(!status.permits(ReasoningEffort::Xhigh));
        assert!(status.permits(ReasoningEffort::High));
        status.observe(&init(), capable);
        assert!(status.permits(ReasoningEffort::Xhigh));
        status.observe(&init(), ClaudeEffortCapability::default());
        assert!(!status.permits(ReasoningEffort::High));
    }
}
