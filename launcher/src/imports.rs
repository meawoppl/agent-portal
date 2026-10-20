//! Launcher-side import boundary: native reads, home policy, and durable binding.
use anyhow::{bail, Context, Result};
use shared::api::{
    ExternalSessionList, ExternalSessionQuery, ImportSessionRequest, PreparedSessionImport,
};
use shared::AgentType;
use uuid::Uuid;

// Browsing/importing must not spawn an unbounded number of native processes.
static IMPORT_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

pub async fn list(
    agent: AgentType,
    mut query: ExternalSessionQuery,
) -> Result<ExternalSessionList> {
    require_codex(agent)?;
    let _slot = IMPORT_SLOTS
        .try_acquire()
        .context("Another session discovery or import is running; retry shortly")?;
    if let Some(path) = query
        .working_directory
        .as_deref()
        .filter(|p| !p.trim().is_empty())
    {
        query.working_directory = Some(
            crate::path_policy::ensure_existing_dir_under_home(path)?
                .to_string_lossy()
                .into_owned(),
        );
    } else {
        query.working_directory = None;
    }
    query.search = query.search.filter(|s| !s.trim().is_empty());
    let mut page = codex_session_lib::imports::list(query).await?;
    page.sessions.retain(|s| {
        crate::path_policy::ensure_existing_dir_under_home(&s.working_directory).is_ok()
    });
    for session in &mut page.sessions {
        session.portal_session_id =
            crate::process_manager::session_id_for_codex_thread(&session.source_id);
    }
    Ok(page)
}

pub async fn prepare(
    agent: AgentType,
    session_id: Uuid,
    request: ImportSessionRequest,
) -> Result<PreparedSessionImport> {
    require_codex(agent)?;
    let _slot = IMPORT_SLOTS
        .try_acquire()
        .context("Another session discovery or import is running; retry shortly")?;
    Uuid::parse_str(&request.source_id).context("Invalid Codex thread ID")?;
    if let Some(existing) = crate::process_manager::session_id_for_codex_thread(&request.source_id)
    {
        if existing != session_id {
            bail!("This conversation already belongs to another Portal session. Open that session instead.");
        }
    }
    let prepared =
        codex_session_lib::imports::prepare(&request.source_id, request.include_history).await?;
    crate::path_policy::ensure_existing_dir_under_home(&prepared.session.working_directory)?;
    crate::process_manager::bind_imported_codex_thread(session_id, &request.source_id)?;
    Ok(prepared)
}

fn require_codex(agent: AgentType) -> Result<()> {
    if agent != AgentType::Codex {
        bail!("Session import is currently supported for Codex only");
    }
    Ok(())
}
