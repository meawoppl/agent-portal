//! Agent-only secret requests and approved use. People manage secrets in the web UI.
mod bindings;
mod client;
mod inject;
mod input;
mod requests;
mod sensitive;
use anyhow::{bail, Context};
use clap::Subcommand;
use sensitive::SecretString;
use std::ffi::OsString;

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Ask the session owner in the web UI to approve pinned secret access.
    Request(requests::RequestArgs),
    /// Render a template using an approved credential group; never prints values.
    Inject(inject::InjectArgs),
    /// Execute using approved bindings and this agent session's scoped credential.
    Run {
        #[arg(long, required = true)]
        binding: Vec<uuid::Uuid>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
}

fn output(value: &impl serde::Serialize) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(std::io::stdout().lock(), value)?;
    println!();
    Ok(())
}

pub async fn run(action: Action, backend: Option<String>, dev: bool) -> anyhow::Result<()> {
    if backend.is_some() {
        bail!("Agent secret access uses PORTAL_SESSION_BACKEND; --backend-url cannot override the bound session context");
    }
    // These paths require the injected session identity. Never load launcher
    // login/config credentials or a human vault token as a fallback.
    match action {
        Action::Request(args) => requests::run(&args, dev).await,
        Action::Run { binding, command } => bindings::execute(&binding, &command, dev).await,
        Action::Inject(args) => inject::bound(&args, dev).await,
    }
}

fn run_child(command: &[OsString], values: Vec<(String, SecretString)>) -> anyhow::Result<()> {
    // Command receives literal argv, never a shell command constructed from secret values.
    let mut child = child_command(command, &values)?;
    let status = child
        .status()
        .map_err(|_| anyhow::anyhow!("Could not execute child process"))?;
    drop(child);
    drop(values);
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn child_command(
    command: &[OsString],
    values: &[(String, SecretString)],
) -> anyhow::Result<std::process::Command> {
    let program = command
        .first()
        .context("Missing child executable after --")?;
    let mut child = std::process::Command::new(program);
    child.args(command.iter().skip(1));
    for (name, value) in values {
        child.env(name, value.as_str());
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn child_receives_literal_environment_and_argument_bytes() {
        let values = vec![(
            "TOKEN".into(),
            SecretString::new("$(printf INJECTED); * \n".into()),
        )];
        let args: Vec<OsString> = [
            "/bin/sh",
            "-c",
            "printf '%s|%s' \"$TOKEN\" \"$1\"",
            "--",
            "$(printf ARG_INJECTED)",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let output = child_command(&args, &values).unwrap().output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            b"$(printf INJECTED); * \n|$(printf ARG_INJECTED)"
        );
    }

    #[test]
    fn human_vault_commands_and_unapproved_execution_are_rejected() {
        use clap::Parser;
        for command in [
            "authorize",
            "login",
            "set",
            "rotate",
            "list",
            "members",
            "info",
            "versions",
            "read",
            "share",
            "unshare",
            "delete",
            "restore",
            "bind",
            "bindings",
            "unbind",
            "expunge",
            "job",
        ] {
            let error = crate::Args::try_parse_from(["agent-portal", "secret", command])
                .err()
                .expect("retired command must not parse");
            // Missing required arguments would also reject an old command;
            // require its actual removal from the CLI's command surface.
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::InvalidSubcommand,
                "{command}"
            );
        }
        assert!(
            crate::Args::try_parse_from(["agent-portal", "secret", "run", "--", "tool"]).is_err()
        );
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "run",
            "--env",
            "TOKEN=personal/T",
            "--",
            "tool"
        ])
        .is_err());
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "inject",
            "--in-file",
            "in",
            "--out-file",
            "out"
        ])
        .is_err());
        let id = "00000000-0000-0000-0000-000000000001";
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "run",
            "--binding",
            id,
            "--",
            "tool"
        ])
        .is_ok());
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "inject",
            "--approval",
            id,
            "--in-file",
            "in",
            "--out-file",
            "out"
        ])
        .is_ok());
    }

    #[tokio::test]
    async fn bound_context_cannot_be_redirected_by_backend_override() {
        let error = run(
            Action::Run {
                binding: vec![uuid::Uuid::new_v4()],
                command: vec![OsString::from("tool")],
            },
            Some("https://other.example.test".into()),
            false,
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot override the bound session context"));
    }

    #[test]
    fn insecure_backends_are_rejected_even_with_credentials() {
        assert!(client::Client::new("http://example.test", true, None).is_err());
        assert!(client::Client::new("http://127.0.0.1:3000", false, None).is_err());
        assert!(client::Client::new("http://127.0.0.1:3000", true, None).is_ok());
        assert!(client::Client::new("https://user:pass@example.test", false, None).is_err());
    }
}
