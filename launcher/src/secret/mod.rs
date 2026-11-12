//! Human CLI secret access uses a separately approved credential, never launcher auth.
mod bindings;
mod client;
mod credentials;
mod inject;
mod input;
mod requests;
mod sensitive;
use anyhow::{bail, Context};
use clap::Subcommand;
use input::{EnvMapping, Reference};
use reqwest::Method;
use sensitive::SecretString;
use shared::api::*;
use std::{ffi::OsString, io::Write};
use zeroize::Zeroize;

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Approve a dedicated secret capability in your browser.
    #[command(alias = "login")]
    Authorize {
        #[arg(long, default_value = "agent-portal CLI")]
        label: String,
    },
    /// Create a secret, or append a version when it exists. Values never go in arguments.
    Set {
        reference: Reference,
        #[arg(long)]
        stdin: bool,
        #[arg(long, requires = "stdin")]
        trim_newline: bool,
        #[arg(long, default_value = "")]
        description: String,
    },
    Rotate {
        reference: Reference,
        #[arg(long)]
        stdin: bool,
        #[arg(long, requires = "stdin")]
        trim_newline: bool,
    },
    List {
        #[arg(long)]
        deleted: bool,
        #[arg(long)]
        json: bool,
    },
    /// List registered members and their provider-qualified identities (metadata only).
    Members {
        #[arg(long)]
        json: bool,
    },
    /// Ask the human in this session to approve a pinned secret binding.
    Request(requests::RequestArgs),
    /// Render secret:// references in a template into a local file; never prints values.
    Inject(inject::InjectArgs),
    /// Approve a short-lived, pinned secret binding for one session and launcher.
    Bind(bindings::BindArgs),
    /// List your approved bindings (metadata only).
    Bindings,
    /// Revoke an approved binding and its outstanding tickets.
    Unbind { id: uuid::Uuid },
    Info {
        reference: Reference,
        /// Include a soft-deleted secret's metadata.
        #[arg(long)]
        deleted: bool,
        #[arg(long)]
        json: bool,
    },
    Versions {
        reference: Reference,
        #[arg(long)]
        json: bool,
    },
    /// Intentionally print exactly the selected value bytes to stdout.
    Read {
        reference: Reference,
        #[arg(long, value_parser = clap::value_parser!(i32).range(1..))]
        version: Option<i32>,
        #[arg(long)]
        raw: bool,
    },
    Share {
        reference: Reference,
        /// Existing member: google:EMAIL, HANDLE@github, github:HANDLE, verified email, or UUID.
        #[arg(long)]
        user: String,
        #[arg(long, default_value = "read,use")]
        access: String,
    },
    Unshare {
        reference: Reference,
        /// Existing member: google:EMAIL, HANDLE@github, github:HANDLE, verified email, or UUID.
        #[arg(long)]
        user: String,
    },
    /// Add the reserved deleted tag; retained versions are preserved.
    Delete { reference: Reference },
    /// Restore only after confirming the displayed sharing audience.
    Restore {
        reference: Reference,
        #[arg(long)]
        confirm_sharing: bool,
    },
    /// Inject secrets into a child environment; requires read AND use rights.
    Run {
        #[arg(
            long = "env",
            required_unless_present = "binding",
            conflicts_with = "binding"
        )]
        mappings: Vec<EnvMapping>,
        /// Execute approved bindings using this agent session's scoped credential; repeat for multiple credentials.
        #[arg(long, conflicts_with = "mappings")]
        binding: Vec<uuid::Uuid>,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    Expunge {
        reference: Reference,
        /// Curator only: redact across the organization rather than your own archives.
        #[arg(long)]
        global: bool,
        #[arg(long, default_value_t = true)]
        all_versions: bool,
        #[arg(long)]
        wait: bool,
    },
    Job {
        id: uuid::Uuid,
        #[arg(long)]
        wait: bool,
    },
}

async fn resolve(client: &client::Client, reference: &Reference) -> anyhow::Result<SecretMetadata> {
    client
        .request(
            Method::POST,
            "/resolve",
            Some(&SecretResolveRequest {
                reference: reference.to_string(),
            }),
        )
        .await
}
fn output(value: &impl serde::Serialize) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(std::io::stdout().lock(), value)?;
    println!();
    Ok(())
}
fn value(stdin: bool, trim: bool) -> anyhow::Result<SecretValue> {
    let text = if stdin {
        input::read_value(std::io::stdin().lock(), trim)?
    } else {
        let mut text = SecretString::new(
            rpassword::prompt_password("Secret value (hidden): ")
                .context("Could not read hidden input")?,
        );
        let result = input::read_value(text.as_bytes(), false);
        text.zeroize();
        result?
    };
    Ok(SecretValue(text.as_str().to_owned()))
}
async fn member(client: &client::Client, selector: &str) -> anyhow::Result<uuid::Uuid> {
    // A known opaque UUID needs no directory lookup. The grant endpoint still
    // validates the recipient and the caller's authority over the secret.
    if let Ok(user_id) = selector.trim().parse::<uuid::Uuid>() {
        return Ok(user_id);
    }
    let member: SecretMember = client
        .request(
            Method::POST,
            "/members/resolve",
            Some(&SecretMemberResolveRequest {
                selector: selector.into(),
            }),
        )
        .await?;
    eprintln!(
        "Resolved portal member {} (provider identities: {})",
        member.user_id,
        serde_json::to_string(&provider_labels(&member))?
    );
    Ok(member.user_id)
}

fn provider_labels(member: &SecretMember) -> Vec<String> {
    member
        .identities
        .iter()
        .map(|identity| {
            if identity.provider == "github" {
                if let Some(handle) = &identity.handle {
                    return format!("{handle}@github (last sign-in handle)");
                }
            }
            if identity.provider == "google" && identity.email_verified {
                if let Some(email) = &identity.email {
                    return format!("google:{email}");
                }
            }
            format!(
                "{} identity (select by portal UUID or sign in again)",
                identity.provider
            )
        })
        .collect()
}

pub async fn run(action: Action, backend: Option<String>, dev: bool) -> anyhow::Result<()> {
    if let Action::Request(args) = &action {
        if backend.is_some() {
            bail!("Secret requests use PORTAL_SESSION_BACKEND; --backend-url cannot override the bound context");
        }
        return requests::run(args, dev).await;
    }
    if let Action::Run {
        binding, command, ..
    } = &action
    {
        if !binding.is_empty() {
            if backend.is_some() {
                bail!("Bound secret execution uses PORTAL_SESSION_BACKEND; --backend-url cannot override the bound context");
            }
            return bindings::execute(binding, command, dev).await;
        }
    }
    if let Action::Inject(args) = &action {
        if args.approval.is_some()
            || [
                "PORTAL_SESSION_ID",
                "PORTAL_SESSION_TOKEN",
                "PORTAL_SESSION_BACKEND",
            ]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
        {
            if backend.is_some() {
                bail!(
                    "Bound injection uses PORTAL_SESSION_BACKEND; --backend-url cannot override it"
                );
            }
            return inject::bound(args, dev).await;
        }
    }
    let config = crate::config::load_config_checked()?;
    let backend = crate::resolve_backend_url(backend, config.backend_url);
    let path = session_lib::paths::config_dir().join("secret-credentials.json");
    let unauthenticated = client::Client::new(&backend, dev, None)?;
    if let Action::Authorize { label } = action {
        let token = SecretString::new(
            config
                .auth_token
                .context("Log in to the portal before approving a dedicated secret credential")?,
        );
        let mut challenge: SecretAuthorizeStartResponse = unauthenticated
            .request_with_token(
                Method::POST,
                "/authorize/start",
                Some(&SecretAuthorizeStartRequest { label }),
                Some(&token),
            )
            .await?;
        eprintln!(
            "Approve secret access in your browser: {}\nCode: {}",
            challenge.verification_uri, challenge.user_code
        );
        let device_code = SecretString::new(std::mem::take(&mut challenge.device_code.0));
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(challenge.expires_in.min(900));
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(
                challenge.interval.clamp(1, 30),
            ))
            .await;
            if std::time::Instant::now() >= deadline {
                bail!("Secret authorization expired");
            }
            let mut request = SecretAuthorizePollRequest {
                device_code: SecretValue(device_code.as_str().to_owned()),
            };
            let response = unauthenticated
                .request::<SecretAuthorizePollResponse, _>(
                    Method::POST,
                    "/authorize/poll",
                    Some(&request),
                )
                .await;
            request.device_code.0.zeroize();
            let response = response?;
            match response.state {
                SecretAuthorizeState::Pending => continue,
                SecretAuthorizeState::Approved => {
                    credentials::save(
                        &path,
                        &unauthenticated.base,
                        response
                            .credential
                            .context("Missing approved credential")?
                            .0,
                        response.expires_at,
                    )?;
                    eprintln!("Dedicated CLI secret access approved.");
                    return Ok(());
                }
                _ => {
                    bail!("Secret authorization denied or expired");
                }
            }
        }
    }
    let token = credentials::load(&path, &unauthenticated.base)?;
    let client = client::Client::new(&backend, dev, Some(token))?;
    match action {
        Action::Members { json } => {
            let response: SecretMembersResponse = client
                .request::<_, ()>(Method::GET, "/members", None)
                .await?;
            if json {
                output(&response)
            } else {
                for member in response.members {
                    println!(
                        "{}\t{}",
                        member.user_id,
                        serde_json::to_string(&provider_labels(&member))?
                    );
                }
                Ok(())
            }
        }
        Action::Request(_) => unreachable!(),
        Action::Inject(args) => inject::human(&client, &args).await,
        Action::Bind(request) => bindings::create(&client, request).await,
        Action::Bindings => {
            let response: SecretBindingListResponse = client
                .request::<_, ()>(Method::GET, "/bindings", None)
                .await?;
            output(&response)
        }
        Action::Unbind { id } => {
            let response: SecretBindingMetadata = client
                .request::<_, ()>(Method::POST, &format!("/bindings/{id}/revoke"), None)
                .await?;
            output(&response)
        }
        Action::List { deleted, json: _ } => {
            let result: SecretListResponse = client
                .request::<_, ()>(Method::GET, &format!("?deleted={deleted}"), None)
                .await?;
            output(&result)
        }
        Action::Info {
            reference,
            deleted,
            json: _,
        } => {
            let metadata = resolve(&client, &reference).await?;
            if is_deleted(&metadata) && !deleted {
                bail!("Secret is soft-deleted; use info --deleted to inspect retained metadata");
            }
            output(&metadata)
        }
        Action::Versions { reference, json: _ } => {
            let metadata = resolve(&client, &reference).await?;
            let result: SecretVersionsResponse = client
                .request::<_, ()>(Method::GET, &format!("/{}/versions", metadata.id), None)
                .await?;
            output(&result)
        }
        Action::Set {
            reference,
            stdin,
            trim_newline,
            description,
        } => write_value(&client, reference, stdin, trim_newline, Some(description)).await,
        Action::Rotate {
            reference,
            stdin,
            trim_newline,
        } => write_value(&client, reference, stdin, trim_newline, None).await,
        Action::Read {
            reference,
            version,
            raw: _,
        } => {
            let metadata = resolve(&client, &reference).await?;
            let mut result: SecretReadResponse = client
                .request(
                    Method::POST,
                    &format!("/{}/read", metadata.id),
                    Some(&SecretReadRequest {
                        version,
                        for_execution: false,
                    }),
                )
                .await?;
            let written = std::io::stdout()
                .lock()
                .write_all(result.value.0.as_bytes());
            result.value.0.zeroize();
            written.context("Could not write requested secret bytes")
        }
        Action::Share {
            reference,
            user,
            access,
        } => {
            let metadata = resolve(&client, &reference).await?;
            let rights = parse_rights(&access)?;
            let request = SecretGrantRequest {
                expected_generation: metadata.generation,
                user_id: member(&client, &user).await?,
                rights,
            };
            let result: SecretMetadata = client
                .request(
                    Method::POST,
                    &format!("/{}/grants", metadata.id),
                    Some(&request),
                )
                .await?;
            output(&result)
        }
        Action::Unshare { reference, user } => {
            let metadata = resolve(&client, &reference).await?;
            let id = member(&client, &user).await?;
            let result: SecretMetadata = client
                .request::<_, ()>(
                    Method::DELETE,
                    &format!(
                        "/{}/grants/{}?expected_generation={}",
                        metadata.id, id, metadata.generation
                    ),
                    None,
                )
                .await?;
            output(&result)
        }
        Action::Delete { reference } => {
            let metadata = resolve(&client, &reference).await?;
            let result: SecretMetadata = client
                .request(
                    Method::POST,
                    &format!("/{}/delete", metadata.id),
                    Some(&SecretMutationRequest {
                        expected_generation: metadata.generation,
                    }),
                )
                .await?;
            output(&result)
        }
        Action::Restore {
            reference,
            confirm_sharing,
        } => {
            let metadata = resolve(&client, &reference).await?;
            eprintln!(
                "Restore audience: owner {}{}; grants: {}",
                metadata.owner_user_id,
                if metadata.namespace == SecretNamespace::Organization {
                    ", all current and future organization members"
                } else {
                    ""
                },
                serde_json::to_string(&metadata.grants)?
            );
            if !confirm_sharing {
                bail!("Review the audience, then repeat with --confirm-sharing");
            }
            let result: SecretMetadata = client
                .request(
                    Method::POST,
                    &format!("/{}/restore", metadata.id),
                    Some(&SecretRestoreRequest {
                        expected_generation: metadata.generation,
                        confirm_sharing: true,
                    }),
                )
                .await?;
            output(&result)
        }
        Action::Run {
            mappings,
            command,
            binding: _,
        } => execute(&client, mappings, command).await,
        Action::Expunge {
            reference,
            global,
            all_versions,
            wait,
        } => {
            let metadata = resolve(&client, &reference).await?;
            let job: SecretExpungeJob = client
                .request(
                    Method::POST,
                    &format!("/{}/expunge", metadata.id),
                    Some(&SecretExpungeRequest {
                        all_versions,
                        global,
                    }),
                )
                .await?;
            job_status(&client, job.id, wait).await
        }
        Action::Job { id, wait } => job_status(&client, id, wait).await,
        Action::Authorize { .. } => unreachable!(),
    }
}

fn is_deleted(metadata: &SecretMetadata) -> bool {
    metadata.deleted_at.is_some() || metadata.tags.iter().any(|tag| tag == "deleted")
}

fn parse_rights(access: &str) -> anyhow::Result<Vec<SecretRight>> {
    let mut rights = Vec::new();
    for right in access.split(',') {
        let right = match right {
            "read" => SecretRight::Read,
            "use" => SecretRight::Use,
            "write" => SecretRight::Write,
            "manage" => SecretRight::Manage,
            _ => bail!("Access must be a comma-separated subset of read,use,write,manage"),
        };
        if rights.contains(&right) {
            bail!("Duplicate access right");
        }
        rights.push(right);
    }
    Ok(rights)
}
async fn write_value(
    client: &client::Client,
    reference: Reference,
    stdin: bool,
    trim: bool,
    create: Option<String>,
) -> anyhow::Result<()> {
    // Read metadata before collecting input so authentication failures do not consume a pipe.
    let existing = match resolve(client, &reference).await {
        Ok(metadata) => Some(metadata),
        Err(error)
            if error
                .downcast_ref::<client::ServiceError>()
                .is_some_and(|e| e.status == 404)
                && create.is_some() =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    if existing.is_none() && matches!(reference, Reference::Id(_)) {
        bail!("Creation requires personal/NAME or org/NAME");
    }
    let mut sensitive = value(stdin, trim)?;
    let response = if let Some(metadata) = existing {
        if is_deleted(&metadata) {
            sensitive.0.zeroize();
            bail!("Restore a deleted secret explicitly before writing");
        }
        let mut request = SecretWriteRequest {
            expected_generation: metadata.generation,
            value: sensitive,
        };
        let response = client
            .request::<SecretMetadata, _>(
                Method::POST,
                &format!("/{}/versions", metadata.id),
                Some(&request),
            )
            .await;
        request.value.0.zeroize();
        response
    } else {
        let (namespace, name) = match reference {
            Reference::Personal(name) => (SecretNamespace::Personal, name),
            Reference::Organization(name) => (SecretNamespace::Organization, name),
            Reference::Id(_) => unreachable!(),
        };
        let mut request = SecretCreateRequest {
            namespace,
            name,
            description: create.unwrap_or_default(),
            value: sensitive,
        };
        let response = client
            .request::<SecretMetadata, _>(Method::POST, "", Some(&request))
            .await;
        request.value.0.zeroize();
        response
    };
    output(&response?)
}
async fn job_status(client: &client::Client, id: uuid::Uuid, wait: bool) -> anyhow::Result<()> {
    loop {
        let job: SecretExpungeJob = client
            .request::<_, ()>(Method::GET, &format!("/jobs/{id}"), None)
            .await?;
        let finished = matches!(
            job.state,
            SecretExpungeState::CompleteActiveStorage
                | SecretExpungeState::AwaitingBackupExpiry
                | SecretExpungeState::Blocked
                | SecretExpungeState::Failed
        );
        if !wait || finished {
            output(&job)?;
            if matches!(
                job.state,
                SecretExpungeState::Blocked | SecretExpungeState::Failed
            ) {
                bail!("Expunge job is blocked or failed; receipt lists remaining boundaries");
            }
            return Ok(());
        }
        eprintln!(
            "Expunge {}: {:?}; scanned {} bytes",
            job.id, job.state, job.bytes_scanned
        );
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
fn validate_mappings(mappings: &[EnvMapping]) -> anyhow::Result<()> {
    let mut seen = std::collections::HashSet::new();
    for mapping in mappings {
        #[cfg(windows)]
        let name = mapping.name.to_ascii_uppercase();
        #[cfg(not(windows))]
        let name = mapping.name.clone();
        if !seen.insert(name) {
            bail!("Each environment variable must be mapped exactly once");
        }
    }
    Ok(())
}
async fn execute(
    client: &client::Client,
    mappings: Vec<EnvMapping>,
    command: Vec<OsString>,
) -> anyhow::Result<()> {
    validate_mappings(&mappings)?;
    command
        .first()
        .context("Missing child executable after --")?;
    let mut values = Vec::new();
    for mapping in mappings {
        let metadata = resolve(client, &mapping.reference).await?;
        if !metadata.can_read || !metadata.can_use || is_deleted(&metadata) {
            bail!("Child environment injection requires read and use rights on a live secret");
        }
        let result: SecretReadResponse = client
            .request(
                Method::POST,
                &format!("/{}/read", metadata.id),
                Some(&SecretReadRequest {
                    version: None,
                    for_execution: true,
                }),
            )
            .await?;
        let value = SecretString::new(result.value.0);
        if value.contains('\0') {
            bail!("Secret contains NUL and cannot be represented in an environment variable");
        }
        values.push((mapping.name, value));
    }
    run_child(&command, values)
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
    fn duplicate_env_and_unknown_permissions_fail() {
        let mappings = vec![
            "TOKEN=personal/A".parse().unwrap(),
            "TOKEN=org/B".parse().unwrap(),
        ];
        assert!(validate_mappings(&mappings).is_err());
        assert!(parse_rights("read,use").is_ok());
        assert!(parse_rights("read,read").is_err());
        assert!(parse_rights("admin").is_err());
    }
    #[test]
    fn value_arguments_are_never_accepted() {
        use clap::Parser;
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "set",
            "personal/T",
            "--value",
            "unsafe"
        ])
        .is_err());
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "set",
            "personal/T",
            "unsafe"
        ])
        .is_err());
        assert!(crate::Args::try_parse_from(["agent-portal", "secret", "read", "T"]).is_err());
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "read",
            "personal/T",
            "--version",
            "0"
        ])
        .is_err());
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "run",
            "--env",
            "TOKEN=personal/T",
            "--",
            "tool",
            "$(literal)"
        ])
        .is_ok());
    }
    #[test]
    fn insecure_backends_are_rejected_even_with_credentials() {
        assert!(client::Client::new("http://example.test", true, None).is_err());
        assert!(client::Client::new("http://127.0.0.1:3000", false, None).is_err());
        assert!(client::Client::new("http://127.0.0.1:3000", true, None).is_ok());
        assert!(client::Client::new("https://user:pass@example.test", false, None).is_err());
    }
}
