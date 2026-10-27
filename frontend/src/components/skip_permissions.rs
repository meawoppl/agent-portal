//! Per-agent permission-preset CLI argument tables ("skip permissions" and
//! Claude's auto mode), shared by the launch and schedule dialogs.
//!
//! The args, the human-readable checkbox labels, and the strip routines that
//! recognize them when editing an existing task live together so they can't
//! drift apart: each `strip_*` routine must keep recognizing exactly what its
//! matching `*_args` function emits.

use shared::AgentType;

const CLAUDE_SKIP_PERMISSIONS_ARGS: &[&str] = &["--dangerously-skip-permissions"];
const CODEX_SKIP_PERMISSIONS_ARGS: &[&str] = &[
    "-c",
    "approval_policy=never",
    "-c",
    "sandbox_mode=danger-full-access",
];
const MUSE_SKIP_PERMISSIONS_ARGS: &[&str] = &["--yolo"];
const ANTIGRAVITY_SKIP_PERMISSIONS_ARGS: &[&str] = &[];
const CLAUDE_AUTO_MODE_ARGS: &[&str] = &["--permission-mode", "auto"];

/// CLI arguments appended when the user checks the skip-permissions box.
pub fn skip_permissions_args(agent_type: AgentType) -> &'static [&'static str] {
    match agent_type {
        AgentType::Claude => CLAUDE_SKIP_PERMISSIONS_ARGS,
        AgentType::Codex => CODEX_SKIP_PERMISSIONS_ARGS,
        AgentType::Muse => MUSE_SKIP_PERMISSIONS_ARGS,
        AgentType::Antigravity => ANTIGRAVITY_SKIP_PERMISSIONS_ARGS,
    }
}

/// Human-readable label for the skip-permissions checkbox.
pub fn skip_permissions_label(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::Claude => "--dangerously-skip-permissions",
        AgentType::Codex => "-c approval_policy=never -c sandbox_mode=danger-full-access",
        AgentType::Muse => "YOLO mode (--yolo: disables approval and sandbox)",
        AgentType::Antigravity => "read-only preview (no permission widening)",
    }
}

/// CLI arguments appended when the user checks the auto-mode box, or `None`
/// when the agent has no auto mode (the checkbox is hidden).
///
/// Claude only honors auto mode on models that support it; on others (e.g.
/// Haiku) the CLI silently falls back to the default mode and prompts as usual.
pub fn auto_mode_args(agent_type: AgentType) -> Option<&'static [&'static str]> {
    match agent_type {
        AgentType::Claude => Some(CLAUDE_AUTO_MODE_ARGS),
        AgentType::Codex | AgentType::Muse | AgentType::Antigravity => None,
    }
}

/// Human-readable label for the auto-mode checkbox.
pub const AUTO_MODE_LABEL: &str = "Auto mode (--permission-mode auto)";

/// Split `args` into (had auto-mode args, remaining args). Recognizes both the
/// two-token form `auto_mode_args` emits and the `--permission-mode=auto`
/// spelling a user may have typed into extra args.
pub fn strip_auto_mode_args(args: &[String], agent_type: AgentType) -> (bool, Vec<String>) {
    if auto_mode_args(agent_type).is_none() {
        return (false, args.to_vec());
    }
    let mut has_auto = false;
    let mut other_args = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--permission-mode=auto" {
            has_auto = true;
            i += 1;
            continue;
        }
        if arg == "--permission-mode" && args.get(i + 1).map(String::as_str) == Some("auto") {
            has_auto = true;
            i += 2;
            continue;
        }
        other_args.push(args[i].clone());
        i += 1;
    }
    (has_auto, other_args)
}

/// Split `args` into (had skip-permissions args, remaining args), recognizing
/// the current per-agent tables plus legacy spellings.
pub fn strip_skip_permissions_args(args: &[String], agent_type: AgentType) -> (bool, Vec<String>) {
    let mut has_skip = false;
    let mut other_args = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].as_str();

        if arg == "--dangerously-skip-permissions" {
            has_skip = true;
            i += 1;
            continue;
        }

        if agent_type == AgentType::Muse && arg == "--yolo" {
            has_skip = true;
            i += 1;
            continue;
        }

        // Legacy Codex UI value. Keep recognizing it so editing an existing
        // task rewrites the removed flag into the current config overrides.
        if agent_type == AgentType::Codex && arg == "--full-auto" {
            has_skip = true;
            i += 1;
            continue;
        }

        if i + 1 < args.len()
            && arg == "-c"
            && matches!(
                args[i + 1].as_str(),
                "approval_policy=never" | "sandbox_mode=danger-full-access"
            )
        {
            has_skip = true;
            i += 2;
            continue;
        }

        other_args.push(args[i].clone());
        i += 1;
    }

    (has_skip, other_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn strips_legacy_codex_full_auto() {
        let (has_skip, other_args) =
            strip_skip_permissions_args(&args(&["--model", "o3", "--full-auto"]), AgentType::Codex);

        assert!(has_skip);
        assert_eq!(other_args, args(&["--model", "o3"]));
    }

    #[test]
    fn strips_codex_config_override_skip_args() {
        let (has_skip, other_args) = strip_skip_permissions_args(
            &args(&[
                "-c",
                "model=o3",
                "-c",
                "approval_policy=never",
                "-c",
                "sandbox_mode=danger-full-access",
                "--strict-config",
            ]),
            AgentType::Codex,
        );

        assert!(has_skip);
        assert_eq!(other_args, args(&["-c", "model=o3", "--strict-config"]));
    }

    #[test]
    fn strip_recognizes_exactly_what_auto_mode_args_emits() {
        let emitted: Vec<String> = auto_mode_args(AgentType::Claude)
            .unwrap()
            .iter()
            .map(|a| a.to_string())
            .collect();
        let (has_auto, other_args) = strip_auto_mode_args(&emitted, AgentType::Claude);
        assert!(has_auto);
        assert!(other_args.is_empty());
    }

    #[test]
    fn strips_auto_mode_equals_spelling_and_keeps_other_modes() {
        let (has_auto, other_args) = strip_auto_mode_args(
            &args(&["--permission-mode=auto", "--verbose"]),
            AgentType::Claude,
        );
        assert!(has_auto);
        assert_eq!(other_args, args(&["--verbose"]));

        let plan = args(&["--permission-mode", "plan"]);
        assert_eq!(
            strip_auto_mode_args(&plan, AgentType::Claude),
            (false, plan.clone())
        );
    }

    #[test]
    fn auto_mode_is_claude_only() {
        for agent_type in [AgentType::Codex, AgentType::Muse, AgentType::Antigravity] {
            assert!(auto_mode_args(agent_type).is_none());
            let passthrough = args(&["--permission-mode", "auto"]);
            assert_eq!(
                strip_auto_mode_args(&passthrough, agent_type),
                (false, passthrough.clone())
            );
        }
    }

    #[test]
    fn strip_recognizes_exactly_what_skip_permissions_args_emits() {
        for agent_type in [
            AgentType::Claude,
            AgentType::Codex,
            AgentType::Muse,
            AgentType::Antigravity,
        ] {
            let emitted: Vec<String> = skip_permissions_args(agent_type)
                .iter()
                .map(|a| a.to_string())
                .collect();
            let (has_skip, other_args) = strip_skip_permissions_args(&emitted, agent_type);
            assert_eq!(
                has_skip,
                !emitted.is_empty(),
                "{agent_type:?} args should be recognized when it emits args"
            );
            assert!(other_args.is_empty(), "{agent_type:?} args fully stripped");
        }
    }
}
