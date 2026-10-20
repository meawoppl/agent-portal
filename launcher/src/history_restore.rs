//! Read-only preflight for restoring History; native state stays on its host.
use anyhow::{bail, Context, Result};
use shared::{
    api::{HistoryRestoreLaunch, HistoryRestoreMode, MAX_HISTORY_CONTEXT_BYTES},
    AgentType,
};

pub async fn check(
    agent: AgentType,
    directory: &str,
    restore: &HistoryRestoreLaunch,
) -> Result<String> {
    let target = crate::path_policy::ensure_existing_dir_under_home(directory)?;
    if restore.mode == HistoryRestoreMode::Context {
        if restore
            .context
            .as_ref()
            .is_none_or(|c| c.trim().is_empty() || c.len() > MAX_HISTORY_CONTEXT_BYTES)
        {
            bail!("Archived context is missing or too large; restore again from History");
        }
        if agent == AgentType::Antigravity {
            bail!("History recovery is not supported for Antigravity");
        }
    } else {
        match agent {
            AgentType::Codex => {
                let thread = crate::process_manager::load_codex_thread_id(restore.source_session_id)
                    .context("No saved Codex thread mapping on this host. Choose archived context to recover the saved transcript.")?;
                codex_session_lib::imports::prepare(&thread, false).await
                    .context("Saved Codex conversation is unavailable. Finish any active turn and retry, or choose archived context.")?;
            }
            AgentType::Claude => {
                let source = crate::path_policy::ensure_existing_dir_under_home(&restore.source_directory)?;
                if target != source { bail!("Native Claude continuation requires its original directory. Use archived context to choose another directory."); }
                let id = crate::process_manager::resolved_transcript_id(restore.source_session_id);
                if claude_session_lib::claude_transcript_status(&source, id) != claude_session_lib::TranscriptStatus::Present {
                    bail!("Saved Claude conversation could not be found on this host. Choose archived context to recover the saved transcript.");
                }
            }
            _ => bail!("Native History continuation is supported for Claude and Codex. Choose archived context for this agent."),
        }
    }
    Ok(target.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recipe(mode: HistoryRestoreMode, context: Option<String>) -> HistoryRestoreLaunch {
        HistoryRestoreLaunch {
            source_session_id: uuid::Uuid::new_v4(),
            source_directory: crate::path_policy::home_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            mode,
            context,
        }
    }
    #[tokio::test]
    async fn context_requires_real_directory_and_nonempty_bounded_context() {
        let r = recipe(
            HistoryRestoreMode::Context,
            Some("Quoted archived context".into()),
        );
        assert!(check(AgentType::Codex, &r.source_directory, &r)
            .await
            .is_ok());
        assert!(check(AgentType::Muse, &r.source_directory, &r)
            .await
            .is_ok());
        assert!(
            check(AgentType::Claude, "/definitely-not-a-restore-directory", &r)
                .await
                .is_err()
        );
        assert!(check(AgentType::Antigravity, &r.source_directory, &r)
            .await
            .is_err());
        for text in [
            None,
            Some(String::new()),
            Some("x".repeat(MAX_HISTORY_CONTEXT_BYTES + 1)),
        ] {
            let r = recipe(HistoryRestoreMode::Context, text);
            assert!(check(AgentType::Codex, &r.source_directory, &r)
                .await
                .is_err());
        }
    }
    #[tokio::test]
    async fn missing_native_state_and_unsupported_agents_never_become_context_recovery() {
        let r = recipe(HistoryRestoreMode::Native, None);
        for agent in [AgentType::Codex, AgentType::Claude, AgentType::Muse] {
            assert!(check(agent, &r.source_directory, &r).await.is_err());
        }
    }
}
