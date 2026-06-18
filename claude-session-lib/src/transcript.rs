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

use std::path::{Path, PathBuf};
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

/// Encode a working directory the way the `claude` CLI names its project dir:
/// every `/` and `.` becomes `-`.
fn encode_project_dir(working_directory: &Path) -> String {
    working_directory
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// Resolve `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`.
fn transcript_path(home: &Path, working_directory: &Path, session_id: Uuid) -> PathBuf {
    home.join(".claude")
        .join("projects")
        .join(encode_project_dir(working_directory))
        .join(format!("{}.jsonl", session_id))
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
    let file = transcript_path(home, working_directory, session_id);
    if file.is_file() {
        return TranscriptStatus::Present;
    }
    // The transcript is gone. Only call it "missing" when the project dir
    // exists — otherwise our encoding may just be wrong, and we shouldn't
    // discard a resume on a guess.
    let project_dir = projects.join(encode_project_dir(working_directory));
    if project_dir.is_dir() {
        TranscriptStatus::Missing
    } else {
        TranscriptStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}").unwrap();
    }

    #[test]
    fn encodes_cwd_like_claude() {
        assert_eq!(
            encode_project_dir(Path::new("/home/meawoppl/repos/meawoppl.github.io")),
            "-home-meawoppl-repos-meawoppl-github-io"
        );
        assert_eq!(
            encode_project_dir(Path::new("/home/u/repos/agent-portal")),
            "-home-u-repos-agent-portal"
        );
    }

    #[test]
    fn present_when_transcript_exists() {
        let home = tempfile::tempdir().unwrap();
        let wd = Path::new("/home/u/repos/site.io");
        let id = Uuid::new_v4();
        write(&transcript_path(home.path(), wd, id));
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
        write(&transcript_path(home.path(), wd, Uuid::new_v4()));
        assert_eq!(
            status_in_home(home.path(), wd, Uuid::new_v4()),
            TranscriptStatus::Missing
        );
    }

    #[test]
    fn unknown_when_project_dir_absent() {
        let home = tempfile::tempdir().unwrap();
        // projects/ exists (another project) but not ours.
        write(&transcript_path(
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
