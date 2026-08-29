//! Locating a `claude` CLI conversation transcript on disk.
//!
//! The CLI stores each session's transcript at
//! `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`, where `<encoded-cwd>`
//! is the working directory with `/` and `.` replaced by `-`
//! (e.g. `/home/u/repos/site.io` → `-home-u-repos-site-io`).
//!
//! This lets the launcher check, *before* spawning `claude --resume <id>`,
//! whether the resume target still exists. A missing transcript otherwise makes
//! `claude` exit near-instantly (often with exit code 0), which reconcile reads
//! as a clean exit and relaunches every heartbeat — the crash loop this guards
//! against. See `launcher::process_manager`.

use std::path::Path;
use uuid::Uuid;

/// Whether a `claude --resume` target transcript exists on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptStatus {
    /// The transcript file exists — resume is safe.
    Present,
    /// The project directory exists but this session's transcript does not —
    /// resume will fail. Confidently missing.
    Missing,
    /// Couldn't determine (no home dir, or the projects/project dir is absent —
    /// which may just mean a path-encoding mismatch). Callers should fall back
    /// to spawning rather than assume missing, to avoid discarding a resume
    /// whose transcript lives under a name we failed to compute.
    Unknown,
}

/// Which id names claude's transcript on disk: the conversation it has diverged
/// onto, else the portal session id.
///
/// `/clear` rolls claude to a new conversation while the portal session id stays
/// put, so from that point the two differ. This is the single rule for resolving
/// that — [`claude_cli_args`](crate::claude_cli_args) applies it to pick the
/// `--resume` target, and every "does the transcript exist?" gate must apply it
/// too. Callers that gate on the portal id while the spawn opens the
/// conversation id get a check of a *different file than the one about to be
/// used*, and both directions of that mismatch hurt: `Missing` on a stale
/// pre-clear file rotates away a live, resumable conversation, while `Present`
/// on one suppresses the rotation that breaks a doomed `--resume` crash-loop.
pub fn claude_transcript_id(session_id: Uuid, conversation_id: Option<Uuid>) -> Uuid {
    conversation_id.unwrap_or(session_id)
}

/// The rotated conversation id carried by a frame, or `None` while claude is
/// still on the portal session's own id.
///
/// This is the *capture* half of the `/clear` handoff, the counterpart to
/// [`claude_transcript_id`]'s *use* half. `/clear` makes the same claude process
/// re-init onto a new conversation, and from that point every frame carries the
/// new id — which is the successor identity. Deliberately reads the frame's own
/// `session_id` and **not** the `conversation_reset` frame's
/// `new_conversation_id`: that field was measured live (claude-codes #316) to
/// match no session and no transcript on disk, and is never referenced again.
pub fn diverged_conversation_id(frame_session_id: Option<&str>, portal_id: Uuid) -> Option<Uuid> {
    frame_session_id
        .and_then(|id| Uuid::parse_str(id).ok())
        .filter(|id| *id != portal_id)
}

/// Classify the resume target for `session_id` in `working_directory`, resolving
/// the home directory via `dirs::home_dir`.
pub fn claude_transcript_status(working_directory: &Path, session_id: Uuid) -> TranscriptStatus {
    match dirs::home_dir() {
        Some(home) => status_in_home(&home, working_directory, session_id),
        None => TranscriptStatus::Unknown,
    }
}

/// Core logic with an explicit home dir, so tests don't depend on the real one.
fn status_in_home(home: &Path, working_directory: &Path, session_id: Uuid) -> TranscriptStatus {
    let projects = home.join(".claude").join("projects");
    if !projects.is_dir() {
        return TranscriptStatus::Unknown;
    }
    let session_id = session_id.to_string();
    let file = claude_codes::transcript::transcript_path(home, working_directory, &session_id);
    if file.is_file() {
        return TranscriptStatus::Present;
    }
    // The transcript is gone. Only call it "missing" when the project dir
    // exists — otherwise our encoding may just be wrong, and we shouldn't
    // discard a resume on a guess.
    let project_dir = projects.join(claude_codes::transcript::encode_project_dir(
        working_directory,
    ));
    if project_dir.is_dir() {
        TranscriptStatus::Missing
    } else {
        TranscriptStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `/clear` handoff, walked end to end over the frame sequence measured
    /// live in claude-codes #316: init on the portal id, a `conversation_reset`
    /// advertising a `new_conversation_id` that matches nothing, then every
    /// later frame carrying the true successor.
    ///
    /// Pins all three links at once — capture takes the successor and ignores
    /// the decoy, the argv resumes what capture learned, and the launcher's
    /// existence gates address the same file the spawn opens.
    #[test]
    fn clear_handoff_captures_the_successor_and_uses_it_everywhere() {
        let portal = Uuid::from_u128(1);
        let successor = Uuid::from_u128(2);
        let decoy = Uuid::from_u128(999); // the reset frame's new_conversation_id

        // Before the clear: frames carry the portal id, nothing has diverged.
        assert_eq!(
            diverged_conversation_id(Some(&portal.to_string()), portal),
            None,
            "no divergence while claude is still on the portal id"
        );

        // The decoy must never be adopted, even though it is a valid uuid that
        // differs from the portal id — it is simply never a frame's session_id.
        // After the clear: subsequent frames carry the successor.
        let learned = diverged_conversation_id(Some(&successor.to_string()), portal)
            .expect("post-clear frames announce the successor");
        assert_eq!(learned, successor);
        assert_ne!(learned, decoy, "new_conversation_id names nothing on disk");

        // Use half: both the resume target and the existence gates resolve
        // through the same rule, so they cannot address different files.
        assert_eq!(claude_transcript_id(portal, Some(learned)), successor);
        assert_eq!(
            claude_transcript_id(portal, None),
            portal,
            "a session that never cleared still resumes its own id"
        );
    }

    /// Garbage in the id position must not be adopted as a conversation: a
    /// resume keyed on it would fail, and a gate keyed on it would report
    /// Missing and rotate away a live session.
    #[test]
    fn unparseable_or_absent_frame_ids_do_not_diverge() {
        let portal = Uuid::from_u128(1);
        assert_eq!(diverged_conversation_id(None, portal), None);
        assert_eq!(diverged_conversation_id(Some("not-a-uuid"), portal), None);
    }

    fn write(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}").unwrap();
    }

    fn sdk_transcript_path(home: &Path, wd: &Path, id: Uuid) -> std::path::PathBuf {
        claude_codes::transcript::transcript_path(home, wd, id.to_string())
    }

    #[test]
    fn present_when_transcript_exists() {
        let home = tempfile::tempdir().unwrap();
        let wd = Path::new("/home/u/repos/site.io");
        let id = Uuid::new_v4();
        write(&sdk_transcript_path(home.path(), wd, id));
        assert_eq!(
            status_in_home(home.path(), wd, id),
            TranscriptStatus::Present
        );
    }

    #[test]
    fn missing_when_project_dir_exists_but_file_absent() {
        let home = tempfile::tempdir().unwrap();
        let wd = Path::new("/home/u/repos/site.io");
        // Create the project dir (via a sibling session) but not our id.
        write(&sdk_transcript_path(home.path(), wd, Uuid::new_v4()));
        assert_eq!(
            status_in_home(home.path(), wd, Uuid::new_v4()),
            TranscriptStatus::Missing
        );
    }

    #[test]
    fn unknown_when_project_dir_absent() {
        let home = tempfile::tempdir().unwrap();
        // projects/ exists (another project) but not ours.
        write(&sdk_transcript_path(
            home.path(),
            Path::new("/some/other/proj"),
            Uuid::new_v4(),
        ));
        assert_eq!(
            status_in_home(
                home.path(),
                Path::new("/home/u/repos/site.io"),
                Uuid::new_v4()
            ),
            TranscriptStatus::Unknown
        );
    }

    #[test]
    fn unknown_when_projects_root_absent() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            status_in_home(
                home.path(),
                Path::new("/home/u/repos/site.io"),
                Uuid::new_v4()
            ),
            TranscriptStatus::Unknown
        );
    }
}
