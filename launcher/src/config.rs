use serde::{Deserialize, Serialize};
use shared::AgentType;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct LauncherConfig {
    pub backend_url: Option<String>,
    pub auth_token: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub sessions: Vec<ExpectedSession>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExpectedSession {
    pub working_directory: String,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default)]
    pub claude_args: Vec<String>,
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("claude-portal")
        .join("launcher.toml")
}

pub fn load_config() -> LauncherConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(config) => {
                tracing::info!("Loaded config from {}", path.display());
                config
            }
            Err(e) => {
                tracing::warn!("Failed to parse {}: {}", path.display(), e);
                LauncherConfig::default()
            }
        },
        Err(_) => LauncherConfig::default(),
    }
}

fn save_config(config: &LauncherConfig) -> anyhow::Result<()> {
    let path = config_path();
    let contents = toml::to_string_pretty(config)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, contents)?;
    tracing::debug!("Saved config to {}", path.display());
    Ok(())
}

pub fn save_auth_token(token: &str) -> anyhow::Result<()> {
    let mut config = load_config();
    config.auth_token = Some(token.to_string());
    save_config(&config)?;
    tracing::info!("Saved auth token to {}", config_path().display());
    Ok(())
}

pub fn add_session(session: &ExpectedSession) -> anyhow::Result<()> {
    let mut config = load_config();
    if config
        .sessions
        .iter()
        .any(|s| s.working_directory == session.working_directory)
    {
        tracing::debug!("Session already in config: {}", session.working_directory);
        return Ok(());
    }
    config.sessions.push(session.clone());
    save_config(&config)
}

pub fn remove_session(working_directory: &str) -> anyhow::Result<()> {
    let mut config = load_config();
    let before = config.sessions.len();
    config
        .sessions
        .retain(|s| s.working_directory != working_directory);
    if config.sessions.len() < before {
        save_config(&config)?;
        tracing::debug!("Removed session from config: {}", working_directory);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let toml = r#"
backend_url = "wss://example.com"
auth_token = "tok_abc123"
name = "my-launcher"
"#;
        let config: LauncherConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.backend_url.unwrap(), "wss://example.com");
        assert_eq!(config.auth_token.unwrap(), "tok_abc123");
        assert_eq!(config.name.unwrap(), "my-launcher");
        assert!(config.sessions.is_empty());
    }

    #[test]
    fn parse_empty_config() {
        let config: LauncherConfig = toml::from_str("").unwrap();
        assert!(config.backend_url.is_none());
        assert!(config.auth_token.is_none());
        assert!(config.name.is_none());
        assert!(config.sessions.is_empty());
    }

    #[test]
    fn parse_partial_config() {
        let toml = r#"
auth_token = "secret"
"#;
        let config: LauncherConfig = toml::from_str(toml).unwrap();
        assert!(config.backend_url.is_none());
        assert_eq!(config.auth_token.unwrap(), "secret");
    }

    #[test]
    fn config_path_is_absolute() {
        let path = config_path();
        assert!(path.is_absolute());
    }

    #[test]
    fn roundtrip_config_serialization() {
        let config = LauncherConfig {
            backend_url: Some("wss://test.com".to_string()),
            auth_token: Some("tok_test".to_string()),
            name: Some("test-launcher".to_string()),
            sessions: vec![ExpectedSession {
                working_directory: "/home/user/project".to_string(),
                session_name: Some("my-session".to_string()),
                agent_type: AgentType::Claude,
                claude_args: vec!["--verbose".to_string()],
            }],
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: LauncherConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.backend_url, config.backend_url);
        assert_eq!(deserialized.auth_token, config.auth_token);
        assert_eq!(deserialized.name, config.name);
        assert_eq!(deserialized.sessions.len(), 1);
        assert_eq!(
            deserialized.sessions[0].working_directory,
            "/home/user/project"
        );
    }

    #[test]
    fn parse_config_with_sessions() {
        let toml = r#"
backend_url = "wss://example.com"
auth_token = "tok_abc"

[[sessions]]
working_directory = "/home/user/project-a"
session_name = "project-a"

[[sessions]]
working_directory = "/home/user/project-b"
agent_type = "codex"
claude_args = ["--model", "opus"]
"#;
        let config: LauncherConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.sessions.len(), 2);

        assert_eq!(config.sessions[0].working_directory, "/home/user/project-a");
        assert_eq!(
            config.sessions[0].session_name.as_deref(),
            Some("project-a")
        );
        assert_eq!(config.sessions[0].agent_type, AgentType::Claude);
        assert!(config.sessions[0].claude_args.is_empty());

        assert_eq!(config.sessions[1].working_directory, "/home/user/project-b");
        assert!(config.sessions[1].session_name.is_none());
        assert_eq!(config.sessions[1].agent_type, AgentType::Codex);
        assert_eq!(config.sessions[1].claude_args, vec!["--model", "opus"]);
    }
}
