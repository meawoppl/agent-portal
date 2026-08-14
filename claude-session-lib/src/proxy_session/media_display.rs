//! Auto-display of images an agent reads.
//!
//! Reading an image used to render it inline; #1450 removed that in favor of the
//! explicit `agent-portal show` CLI. `show` is still the right tool when an
//! agent *generates* something it wants seen, but it does nothing for the common
//! case of an agent reading a screenshot or a diagram — the user watches it
//! reason about a picture they cannot see. This restores the implicit path on
//! top of the current media stack rather than the chunked uploader #1451 deleted:
//! detection lives here, and the launcher does the upload through the same
//! `POST /api/agent/sessions/{id}/media` endpoint `show` uses, so archiving,
//! replay, SVG sanitization (#1530) and eviction behavior are shared.
//!
//! Claude and Codex need separate detectors for the same reason their git
//! signals do (#1653): Claude has a typed `Read` tool, Codex shells out.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use claude_codes::ClaudeOutput;

/// Displays a local media file inline in the session transcript.
///
/// The detection half lives here; the launcher supplies the side effect because
/// only it holds the session's upload credentials. Same split — and same reason
/// — as [`ClaudeConversationIdSink`](super::ClaudeConversationIdSink). A
/// standalone proxy passes `None` and simply doesn't auto-display.
pub type MediaDisplaySink = Arc<dyn Fn(PathBuf) + Send + Sync>;

/// Extensions the portal can render inline. Deliberately the image half of
/// `shared::media`'s supported set: video is never auto-displayed, since nothing
/// an agent does to a video file reads as "look at this".
const DISPLAYABLE: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "svg"];

fn is_displayable_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            DISPLAYABLE
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// The image Claude's `Read` tool is opening in this frame, if any.
///
/// Keyed off the typed [`ToolInput::Read`](claude_codes::tool_inputs::ToolInput)
/// rather than `input["file_path"]`, so an upstream field rename is a compile
/// error here instead of a detector that silently stops matching.
pub(super) fn claude_output_image_read(output: &ClaudeOutput) -> Option<PathBuf> {
    let read = output.as_tool_use("Read")?;
    let Some(claude_codes::tool_inputs::ToolInput::Read(input)) = read.typed_input() else {
        return None;
    };
    let path = PathBuf::from(input.file_path);
    is_displayable_image(&path).then_some(path)
}

/// Best-effort Codex counterpart.
///
/// Codex has no typed read tool — it shells out — so this parses command argv
/// and is honestly weaker than the Claude path: it sees `cat diagram.png` but
/// not an image opened by a script or a shell function. Gated to `item.completed`
/// so the file exists by the time we upload it, and to a read-ish allowlist so
/// that `rm logo.png` never displays anything.
pub(super) fn codex_output_image_read(value: &serde_json::Value) -> Option<PathBuf> {
    if value.get("type").and_then(|t| t.as_str()) != Some("item.completed") {
        return None;
    }
    let item = value.get("item")?;
    let item_type = item.get("type").and_then(|t| t.as_str())?;
    if item_type != "commandExecution" && item_type != "command_execution" {
        return None;
    }
    image_path_from_read_command(item.get("command")?.as_str()?)
}

/// Commands that mean "look at this file". `cat` earns its place because it is
/// what an agent reaches for reflexively, even though the bytes are useless to
/// it — that reflex is exactly the case worth rendering for the human.
const READ_COMMANDS: [&str; 6] = ["cat", "open", "xdg-open", "imgcat", "display", "qlmanage"];

fn image_path_from_read_command(command: &str) -> Option<PathBuf> {
    let mut tokens = command.split_whitespace();
    let program = tokens.next()?;
    let program = Path::new(program).file_name()?.to_str()?;
    if !READ_COMMANDS.contains(&program) {
        return None;
    }
    tokens
        .filter(|token| !token.starts_with('-'))
        .map(PathBuf::from)
        .find(|path| is_displayable_image(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claude_read(file_path: &str) -> ClaudeOutput {
        serde_json::from_value(json!({
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-5",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "Read",
                    "input": { "file_path": file_path }
                }],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 1, "output_tokens": 1 }
            },
            "session_id": "00000000-0000-0000-0000-000000000001"
        }))
        .expect("valid claude assistant frame")
    }

    #[test]
    fn detects_a_claude_image_read() {
        assert_eq!(
            claude_output_image_read(&claude_read("/tmp/shot.png")),
            Some(PathBuf::from("/tmp/shot.png"))
        );
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        assert!(claude_output_image_read(&claude_read("/tmp/Diagram.SVG")).is_some());
    }

    /// The overwhelmingly common Read: source files must not upload anything.
    #[test]
    fn ignores_claude_reads_of_non_images() {
        assert!(claude_output_image_read(&claude_read("/src/main.rs")).is_none());
        assert!(claude_output_image_read(&claude_read("/notes/pngs.md")).is_none());
    }

    fn codex_command(command: &str) -> serde_json::Value {
        json!({
            "type": "item.completed",
            "item": { "type": "commandExecution", "command": command }
        })
    }

    #[test]
    fn detects_a_codex_shell_image_read() {
        assert_eq!(
            codex_output_image_read(&codex_command("cat out/chart.png")),
            Some(PathBuf::from("out/chart.png"))
        );
        assert_eq!(
            codex_output_image_read(&codex_command("/usr/bin/open -a Preview a.jpeg")),
            Some(PathBuf::from("a.jpeg"))
        );
    }

    /// Destructive and unrelated commands that merely mention an image must not
    /// trigger an upload.
    #[test]
    fn ignores_codex_commands_that_are_not_reads() {
        assert!(codex_output_image_read(&codex_command("rm logo.png")).is_none());
        assert!(codex_output_image_read(&codex_command("cp a.png b.png")).is_none());
        assert!(codex_output_image_read(&codex_command("cat main.rs")).is_none());
    }

    /// Only completed commands: uploading on `item.started` races the file into
    /// existence and would display a truncated or absent image.
    #[test]
    fn ignores_codex_commands_still_running() {
        let mut started = codex_command("cat out/chart.png");
        started["type"] = json!("item.started");
        assert!(codex_output_image_read(&started).is_none());
    }

    /// A muse journal record must not be read by the codex detector (#1653).
    #[test]
    fn ignores_muse_records() {
        let muse = json!({
            "type": "muse_record",
            "payload_type": "tool.result",
            "payload": { "output": "cat shot.png" }
        });
        assert!(codex_output_image_read(&muse).is_none());
    }
}
