//! Native Claude hooks: context must reach Claude, not just Portal's output mirror.

const MANIFEST: &str = include_str!("../hooks/portal-agent-discovery/.claude-plugin/plugin.json");
const HOOKS: &str = include_str!("../hooks/portal-agent-discovery/hooks/hooks.json");

/// Owns the private, session-only plugin directory. Keep this alive until the
/// Claude process ends: hooks can be loaded lazily or again after a clear.
pub struct PortalToolReminders {
    _directory: tempfile::TempDir,
}

/// Add an independent plugin rather than merging --settings: settings files
/// may contain secrets and must never be copied into process arguments.
/// Claude's repeatable --plugin-dir keeps the caller's plugins/settings intact;
/// explicit hook-disabling policy is left for Claude to enforce.
pub fn add_portal_tool_reminders(args: &mut Vec<String>) -> std::io::Result<PortalToolReminders> {
    let directory = tempfile::Builder::new()
        .prefix("portal-agent-discovery-")
        .tempdir()?;
    std::fs::create_dir(directory.path().join(".claude-plugin"))?;
    std::fs::create_dir(directory.path().join("hooks"))?;
    std::fs::write(
        directory.path().join(".claude-plugin/plugin.json"),
        MANIFEST,
    )?;
    std::fs::write(directory.path().join("hooks/hooks.json"), HOOKS)?;
    // Prepend rather than parsing arbitrary caller flags: even a value equal
    // to `--` (for example a system prompt) must remain untouched.
    args.splice(
        0..0,
        [
            "--plugin-dir".to_owned(),
            directory.path().to_string_lossy().into_owned(),
        ],
    );
    Ok(PortalToolReminders {
        _directory: directory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn hook_emits_context_without_changing_tool_result_or_permissions() {
        let hooks: Value = serde_json::from_str(HOOKS).unwrap();
        let events = hooks["hooks"].as_object().unwrap();
        assert_eq!(events.len(), 1);
        let hook = &events["PostToolUse"][0];
        assert_eq!(hook["matcher"], "^ListAgents$");
        assert_eq!(hook["hooks"][0]["type"], "command");
        let result = std::process::Command::new("sh")
            .args(["-c", hook["hooks"][0]["command"].as_str().unwrap()])
            .output()
            .unwrap();
        assert!(result.status.success());
        let output: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(output.as_object().unwrap().len(), 1);
        let context = &output["hookSpecificOutput"];
        assert_eq!(context.as_object().unwrap().len(), 2);
        assert_eq!(context["hookEventName"], "PostToolUse");
        let reminder = context["additionalContext"].as_str().unwrap();
        assert!(reminder.contains("not a complete Agent Portal inventory"));
        assert!(reminder.contains("other computers"));
        assert!(reminder.contains("Codex or Muse"));
        assert!(reminder.contains("agent-portal message list"));
        assert!(reminder.contains("permissions still apply"));
    }

    #[test]
    fn preserves_settings_plugins_and_prompt_arguments() {
        let original = vec![
            "--settings".into(),
            "private-settings.json".into(),
            "--append-system-prompt".into(),
            "--".into(),
            "--plugin-dir".into(),
            "user-plugin".into(),
            "--".into(),
            "prompt".into(),
        ];
        let mut args = original.clone();
        let plugin = add_portal_tool_reminders(&mut args).unwrap();
        assert_eq!(args[0], "--plugin-dir");
        assert_eq!(std::path::Path::new(&args[1]), plugin._directory.path());
        args.drain(0..2);
        assert_eq!(args, original);
    }

    #[test]
    fn plugin_files_live_until_guard_drops_and_are_private_per_process() {
        let first = add_portal_tool_reminders(&mut Vec::new()).unwrap();
        let second = add_portal_tool_reminders(&mut Vec::new()).unwrap();
        let root = first._directory.path().to_owned();
        assert_ne!(root, second._directory.path());
        assert_eq!(
            std::fs::read_to_string(root.join(".claude-plugin/plugin.json")).unwrap(),
            MANIFEST
        );
        assert_eq!(
            std::fs::read_to_string(root.join("hooks/hooks.json")).unwrap(),
            HOOKS
        );
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "portal-agent-discovery");
        drop(first);
        assert!(!root.exists());
        assert!(second._directory.path().exists());
    }
}
