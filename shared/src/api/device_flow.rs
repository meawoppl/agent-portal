//! Device-flow authentication request/response types.

use serde::{Deserialize, Serialize};

/// Device-flow client class. Existing CLI/proxy callers default to `cli`;
/// mobile shells opt in with `mobile` so their tokens can be expiring and
/// refreshable without regressing standalone proxy credentials.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClientType {
    #[default]
    Cli,
    Mobile,
}

/// Device flow code request response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Request body for device code creation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceCodeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub client_type: DeviceClientType,
}

/// Request body for polling device flow status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceFlowPollRequest {
    pub device_code: String,
}

/// Response for device flow approve/deny actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceFlowActionResponse {
    pub success: bool,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_code_request_defaults_to_cli_client_type() {
        let req: DeviceCodeRequest =
            serde_json::from_str(r#"{"hostname":"devbox"}"#).expect("request should parse");

        assert_eq!(req.hostname.as_deref(), Some("devbox"));
        assert_eq!(req.client_type, DeviceClientType::Cli);
    }

    #[test]
    fn device_code_request_accepts_mobile_client_type() {
        let req: DeviceCodeRequest =
            serde_json::from_str(r#"{"client_type":"mobile"}"#).expect("request should parse");

        assert_eq!(req.client_type, DeviceClientType::Mobile);
    }
}
