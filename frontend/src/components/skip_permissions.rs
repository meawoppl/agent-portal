//! Per-agent "skip permissions" CLI argument tables, shared by the launch
//! and schedule dialogs.
//!
//! The args, the human-readable checkbox label, and the strip routine that
//! recognizes them when editing an existing task live together so they can't
//! drift apart: `strip_skip_permissions_args` must keep recognizing exactly
//! what `skip_permissions_args` emits.

use shared::AgentType;

const CLAUDE_SKIP_PERMISSIONS_ARGS: &[&str] = &["--dangerously-skip-permissions"];
const CODEX_SKIP_PERMISSIONS_ARGS: &[&str] = &[
    "-c",
    "approval_policy=never",
    "-c",
    "sandbox_mode=danger-full-access",
];

/// CLI arguments appended when the user checks the skip-permissions box.
pub fn skip_permissions_args(agent_type: AgentType) -> &'static [&'static str] {
    match agent_type {
        AgentType::Claude => CLAUDE_SKIP_PERMISSIONS_ARGS,
        AgentType::Codex => CODEX_SKIP_PERMISSIONS_ARGS,
    }
}

/// Human-readable label for the skip-permissions checkbox.
pub fn skip_permissions_label(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::Claude => "--dangerously-skip-permissions",
        AgentType::Codex => "-c approval_policy=never -c sandbox_mode=danger-full-access",
    }
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
    fn strip_recognizes_exactly_what_skip_permissions_args_emits() {
        for agent_type in [AgentType::Claude, AgentType::Codex] {
            let emitted: Vec<String> = skip_permissions_args(agent_type)
                .iter()
                .map(|a| a.to_string())
                .collect();
            let (has_skip, other_args) = strip_skip_permissions_args(&emitted, agent_type);
            assert!(has_skip, "{agent_type:?} args should be recognized");
            assert!(other_args.is_empty(), "{agent_type:?} args fully stripped");
        }
    }
}
