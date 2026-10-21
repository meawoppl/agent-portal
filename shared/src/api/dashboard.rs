//! Typed requests on the authenticated user WebSocket. Side effects are never
//! automatically replayed after a disconnect: their outcome may be unknown.
use super::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "args", rename_all = "snake_case")]
pub enum DashboardRequest {
    ListLaunchers,
    ListExternalSessions {
        launcher_id: Uuid,
        agent_type: crate::AgentType,
        query: ExternalSessionQuery,
    },
    ImportSession {
        launcher_id: Uuid,
        agent_type: crate::AgentType,
        request: ImportSessionRequest,
    },
    ListDirectories {
        launcher_id: Uuid,
        path: String,
    },
    ProbeAgents {
        launcher_id: Uuid,
    },
    Launch(LaunchRequest),
    UpdateLauncher {
        launcher_id: Uuid,
    },
    RestartLauncher {
        launcher_id: Uuid,
    },
    StartAgentLogin {
        launcher_id: Uuid,
        request: StartAgentLoginRequest,
    },
    SubmitAgentLoginCode {
        launcher_id: Uuid,
        flow_id: Uuid,
        request: SubmitAgentLoginCodeRequest,
    },
    PollAgentLogin {
        launcher_id: Uuid,
        flow_id: Uuid,
    },
    CancelAgentLogin {
        launcher_id: Uuid,
        flow_id: Uuid,
    },
    InstallAgent {
        launcher_id: Uuid,
        agent_type: crate::AgentType,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResponse {
    pub request_id: Uuid,
    pub session_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "data", rename_all = "snake_case")]
pub enum DashboardResponse {
    Launchers(Vec<crate::LauncherInfo>),
    ExternalSessions(ExternalSessionList),
    Imported(ImportSessionResponse),
    Directories(DirectoryListingResponse),
    Agents(ProbeAgentsResponse),
    Launched(LaunchResponse),
    LoginStarted(StartAgentLoginResponse),
    LoginOutcome(crate::AgentLoginOutcome),
    AgentInstalled(InstallAgentResponse),
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardError {
    pub status: u16,
    pub message: String,
}
