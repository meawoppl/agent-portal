//! Native Claude hooks: context must reach Claude, not just Portal's output mirror.

use std::path::Path;

use serde_json::{json, Value};

const LIST_AGENTS_REMINDER: &str = "Claude's ListAgents results are not a complete Agent Portal inventory. They can omit Portal sessions on other computers or using other providers (such as Codex or Muse). `agent-portal message list` lists Portal sessions accessible to this caller across computers and providers; normal Portal permissions still apply.";

/// Add a session-only PostToolUse hook without modifying settings on disk.
/// Claude accepts either inline JSON or a file for --settings; preserve the
/// effective (last) value and its existing hooks. A bare `--` ends flag parsing.
/// Explicit hook-disabling policy is left intact for Claude to enforce.
pub fn add_portal_tool_reminders(args: &mut Vec<String>, cwd: &Path) -> Result<(), String> {
    let end = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut setting = None;
    let mut i = 0;
    while i < end {
        if args[i] == "--settings" {
            if i + 1 >= end {
                return Err("Claude --settings requires a file or JSON object".into());
            }
            setting = Some((i + 1, false, args[i + 1].clone()));
            i += 2;
        } else {
            if let Some(value) = args[i].strip_prefix("--settings=") {
                setting = Some((i, true, value.to_owned()));
            }
            i += 1;
        }
    }
    let mut settings = match &setting {
        None => json!({}),
        Some((_, _, value)) => {
            let contents = if value.trim_start().starts_with('{') {
                value.clone()
            } else {
                std::fs::read_to_string(cwd.join(value))
                    .map_err(|_| "Could not read Claude --settings file".to_string())?
            };
            // Settings may contain credentials. Never include their contents
            // (or a JSON parser's source excerpt) in diagnostics.
            serde_json::from_str::<Value>(&contents)
                .map_err(|_| "Claude --settings must contain a JSON object".to_string())?
        }
    };
    let object = settings
        .as_object_mut()
        .ok_or("Claude --settings must contain a JSON object")?;
    let hooks = object.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or("Claude hooks must be an object")?;
    let post = hooks.entry("PostToolUse").or_insert_with(|| json!([]));
    let post = post
        .as_array_mut()
        .ok_or("Claude PostToolUse hooks must be an array")?;
    let output = json!({"hookSpecificOutput": {
        "hookEventName": "PostToolUse",
        // Claude itself wraps additionalContext in a system-reminder tag.
        "additionalContext": LIST_AGENTS_REMINDER,
    }})
    .to_string();
    // Static data only; POSIX single-quote escaping keeps apostrophes and any
    // future shell metacharacters literal. No tool input enters this command.
    let command = format!("printf '%s\\n' '{}'", output.replace('\'', "'\\''"));
    post.push(json!({"matcher": "^ListAgents$", "hooks": [{
        "type": "command", "command": command, "timeout": 5,
    }]}));
    let encoded = settings.to_string();
    match setting {
        Some((index, true, _)) => args[index] = format!("--settings={encoded}"),
        Some((index, false, _)) => args[index] = encoded,
        None => {
            args.splice(end..end, ["--settings".to_owned(), encoded]);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_emits_context_without_changing_tool_result_or_permissions() {
        let mut args = vec!["--print".into()];
        add_portal_tool_reminders(&mut args, Path::new(".")).unwrap();
        let settings: Value = serde_json::from_str(&args[2]).unwrap();
        let hook = &settings["hooks"]["PostToolUse"][0];
        assert_eq!(hook["matcher"], "^ListAgents$");
        let result = std::process::Command::new("sh")
            .args(["-c", hook["hooks"][0]["command"].as_str().unwrap()])
            .output()
            .unwrap();
        assert!(result.status.success());
        let output: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            output,
            json!({"hookSpecificOutput": {
                "hookEventName": "PostToolUse", "additionalContext": LIST_AGENTS_REMINDER,
            }})
        );
    }

    #[test]
    fn preserves_inline_settings_and_existing_hooks() {
        let existing = json!({"model": "sonnet", "disableAllHooks": true,
            "hooks": {"PostToolUse": [{"matcher": "Bash", "hooks": []}],
                      "PreToolUse": [{"matcher": "Read", "hooks": []}]}});
        let mut args = vec![format!("--settings={existing}")];
        add_portal_tool_reminders(&mut args, Path::new(".")).unwrap();
        let updated: Value =
            serde_json::from_str(args[0].strip_prefix("--settings=").unwrap()).unwrap();
        assert_eq!(updated["model"], existing["model"]);
        assert_eq!(updated["disableAllHooks"], true);
        assert_eq!(
            updated["hooks"]["PreToolUse"],
            existing["hooks"]["PreToolUse"]
        );
        assert_eq!(
            updated["hooks"]["PostToolUse"][0],
            existing["hooks"]["PostToolUse"][0]
        );
        assert_eq!(updated["hooks"]["PostToolUse"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn resolves_settings_file_from_session_directory_without_writing_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = r#"{"env":{"EXAMPLE":"value"}}"#;
        std::fs::write(&path, original).unwrap();
        let mut args = vec!["--settings".into(), "settings.json".into()];
        add_portal_tool_reminders(&mut args, dir.path()).unwrap();
        let updated: Value = serde_json::from_str(&args[1]).unwrap();
        assert_eq!(updated["env"]["EXAMPLE"], "value");
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn uses_last_settings_and_respects_end_of_options() {
        let mut args = vec![
            "--settings".into(),
            "missing.json".into(),
            "--settings={\"model\":\"sonnet\"}".into(),
            "--".into(),
            "--settings=prompt".into(),
        ];
        add_portal_tool_reminders(&mut args, Path::new(".")).unwrap();
        assert_eq!(args[1], "missing.json");
        assert!(args[2].contains("PostToolUse"));
        assert_eq!(args[4], "--settings=prompt");
    }

    #[test]
    fn malformed_settings_fail_without_disclosing_contents() {
        for value in [
            "{\"secret\": private",
            "{\"hooks\":false}",
            "{\"hooks\":{\"PostToolUse\":{}}}",
        ] {
            let mut args = vec!["--settings".into(), value.into()];
            let original = args.clone();
            let error = add_portal_tool_reminders(&mut args, Path::new(".")).unwrap_err();
            assert!(!error.contains("private"));
            assert_eq!(args, original);
        }
    }
}
