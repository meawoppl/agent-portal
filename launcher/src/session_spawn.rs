//! Agent child lifecycle commands use only the process's session credential.
//! Never fall back to launcher.json: that is an owner's broader credential.
use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use shared::api::{
    LaunchSpec, SpawnChildrenResponse, SpawnErrorResponse, SpawnRequestInfo, SpawnRequestState,
    SpawnSessionRequest,
};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Request a child. First spawn needs the owner's Children approval.
    Start(Box<Start>),
    /// List only this session's direct children.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Stop only this session's direct child (full UUID required).
    Stop {
        child: Uuid,
        #[arg(long)]
        json: bool,
    },
}
#[derive(Debug, Args)]
pub struct Start {
    #[arg(long)]
    dir: String,
    /// Launcher UUID or exact hostname; defaults to the parent's launcher.
    #[arg(long)]
    host: Option<String>,
    #[arg(long, default_value = "claude")]
    agent: String,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, default_value = "none")]
    worktree: String,
    #[arg(
        long,
        conflicts_with = "prompt_file",
        required_unless_present = "prompt_file"
    )]
    prompt: Option<String>,
    #[arg(long, conflicts_with = "prompt")]
    prompt_file: Option<std::path::PathBuf>,
    /// Comma-separated plugin names; use -NAME to explicitly disable a plugin.
    #[arg(long)]
    plugins: Option<String>,
    /// Reuse an id when retrying an identical request; never launches twice.
    #[arg(long)]
    request_id: Option<Uuid>,
    #[arg(long)]
    json: bool,
    /// Wait up to 120 seconds for the approved child to connect.
    #[arg(long)]
    wait: bool,
}
#[derive(Debug)]
struct Failure(SpawnErrorResponse);
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0.code, self.0.message)
    }
}
impl std::error::Error for Failure {}
struct Api {
    client: reqwest::Client,
    base: String,
    token: String,
    parent: Uuid,
}
impl Api {
    fn from_env() -> Result<Self> {
        let token = std::env::var("PORTAL_SESSION_TOKEN").ok().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("Run inside an updated Portal agent session; no session credential is available"))?;
        let base = std::env::var("PORTAL_SESSION_BACKEND")?
            .trim_end_matches('/')
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1);
        let parent = std::env::var("PORTAL_SESSION_ID")?.parse()?;
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base,
            token,
            parent,
        })
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }
    async fn children(&self) -> Result<SpawnChildrenResponse> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("/api/agent/sessions/{}/children", self.parent),
            )
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }
}
fn worktree(value: &str) -> Result<shared::WorktreeMode> {
    match value {
        "none" => Ok(shared::WorktreeMode::None),
        "scratch" => Ok(shared::WorktreeMode::Scratch { branch: None }),
        v if v.starts_with("new-branch:") && v.len() > 11 => Ok(shared::WorktreeMode::Repo {
            branch: Some(v[11..].into()),
        }),
        _ => Err(anyhow!(
            "--worktree must be none, scratch, or new-branch:NAME"
        )),
    }
}
fn plugins(value: Option<&str>) -> Result<Option<BTreeMap<String, bool>>> {
    value
        .map(|v| {
            v.split(',')
                .map(|name| {
                    let name = name.trim();
                    let (name, enabled) =
                        name.strip_prefix('-').map_or((name, true), |n| (n, false));
                    if name.is_empty() {
                        return Err(anyhow!("Plugin names cannot be empty"));
                    }
                    Ok((name.to_string(), enabled))
                })
                .collect()
        })
        .transpose()
}
fn report_error(json: bool, code: &str, message: &str, request_id: Option<Uuid>) {
    if json {
        println!(
            "{}",
            serde_json::to_string(&SpawnErrorResponse {
                code: code.into(),
                message: message.into(),
                request_id
            })
            .expect("serializable error")
        );
    }
}
pub async fn run(action: Action) -> Result<()> {
    let json = match &action {
        Action::Start(s) => s.json,
        Action::List { json } | Action::Stop { json, .. } => *json,
    };
    let result = run_inner(action).await;
    if let Err(e) = &result {
        if let Some(failure) = e.downcast_ref::<Failure>() {
            report_error(
                json,
                &failure.0.code,
                &failure.0.message,
                failure.0.request_id,
            );
        } else {
            report_error(json, "command_failed", &e.to_string(), None);
        }
    }
    result
}
async fn run_inner(action: Action) -> Result<()> {
    let api = Api::from_env()?;
    match action {
        Action::List { json } => {
            let children = api.children().await?;
            if json {
                println!("{}", serde_json::to_string(&children)?);
            } else {
                for child in children.children {
                    println!("{}  {}  {:?}", child.id, child.session_name, child.status);
                }
            }
        }
        Action::Stop { child, json } => {
            api.request(
                reqwest::Method::POST,
                &format!("/api/agent/sessions/{}/children/{child}/stop", api.parent),
            )
            .send()
            .await?
            .error_for_status()?;
            if json {
                #[derive(serde::Serialize)]
                struct Stopped {
                    session_id: Uuid,
                    stopped: bool,
                }
                println!(
                    "{}",
                    serde_json::to_string(&Stopped {
                        session_id: child,
                        stopped: true
                    })?
                );
            } else {
                println!("Stopped {child}");
            }
        }
        Action::Start(args) => {
            let agent_type = args
                .agent
                .parse::<shared::AgentType>()
                .map_err(|_| anyhow!("Unknown agent {}", args.agent))?;
            let prompt = match args.prompt {
                Some(p) => p,
                None => std::fs::read_to_string(args.prompt_file.context("missing prompt")?)?,
            };
            let launcher_id = match args.host {
                None => None,
                Some(host) => {
                    if let Ok(id) = host.parse::<Uuid>() {
                        Some(id)
                    } else {
                        let list: Vec<shared::LauncherInfo> = api
                            .request(reqwest::Method::GET, "/api/launchers")
                            .send()
                            .await?
                            .error_for_status()?
                            .json()
                            .await?;
                        let matches: Vec<_> = list.iter().filter(|l| l.hostname == host).collect();
                        if matches.len() != 1 {
                            return Err(anyhow!(
                                "Host must identify exactly one launcher; use its UUID"
                            ));
                        }
                        Some(matches[0].launcher_id)
                    }
                }
            };
            let request = SpawnSessionRequest {
                request_id: args.request_id.unwrap_or_else(Uuid::new_v4),
                launcher_id,
                prompt,
                plugins: plugins(args.plugins.as_deref())?,
                launch: LaunchSpec {
                    working_directory: args.dir,
                    session_name: args.name,
                    claude_args: args
                        .model
                        .map(|m| vec!["--model".into(), m])
                        .unwrap_or_default(),
                    agent_type,
                    worktree: worktree(&args.worktree)?,
                },
            };
            let response = api
                .request(
                    reqwest::Method::POST,
                    &format!("/api/agent/sessions/{}/children", api.parent),
                )
                .json(&request)
                .send()
                .await?;
            if !response.status().is_success() {
                let error = response.json::<SpawnErrorResponse>().await?;
                return Err(Failure(error).into());
            }
            let info: SpawnRequestInfo = response.json().await?;
            if matches!(info.state, SpawnRequestState::Pending) {
                return Err(Failure(SpawnErrorResponse {
                    code: "approval_required".into(),
                    message: "Open Children in the parent session to approve this request".into(),
                    request_id: Some(info.request_id),
                })
                .into());
            }
            if matches!(
                info.state,
                SpawnRequestState::Denied | SpawnRequestState::Failed
            ) {
                return Err(anyhow!(
                    "Spawn request {:?}: {}",
                    info.state,
                    info.error_code.unwrap_or_default()
                ));
            }
            if args.wait {
                let child = info.child_session_id.context(
                    "Child dispatch has not reserved its id yet; retry the same request id",
                )?;
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
                loop {
                    if let Some(s) = api
                        .children()
                        .await?
                        .children
                        .into_iter()
                        .find(|s| s.id == child)
                    {
                        if s.paused {
                            return Err(anyhow!("Child stopped before connecting"));
                        }
                        if s.status == shared::SessionStatus::Active {
                            break;
                        }
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(anyhow!(
                            "Timed out waiting for child {child}; request {} remains durable",
                            info.request_id
                        ));
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
            if args.json {
                println!("{}", serde_json::to_string(&info)?);
            } else {
                println!(
                    "Request {}: {:?}; child {}",
                    info.request_id,
                    info.state,
                    info.child_session_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "pending".into())
                );
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_launch_modes_and_plugin_choices() {
        assert!(
            matches!(worktree("new-branch:feature").unwrap(),shared::WorktreeMode::Repo{branch:Some(b)} if b=="feature")
        );
        assert!(worktree("new-branch:").is_err());
        assert!(worktree("other").is_err());
        let p = plugins(Some("yapcad,-visilog")).unwrap().unwrap();
        assert_eq!(p.get("yapcad"), Some(&true));
        assert_eq!(p.get("visilog"), Some(&false));
        assert!(plugins(Some("a,,b")).is_err());
    }
}
