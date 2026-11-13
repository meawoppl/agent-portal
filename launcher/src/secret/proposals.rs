//! Values enter through stdin/files, never argv or status output.
use super::{bindings::BoundAgent, input::Reference};
use anyhow::{bail, Context};
use clap::Args;
use reqwest::Method;
use shared::api::*;
use std::{io::Read, path::PathBuf};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, Args)]
pub struct ProposeArgs {
    /// Single new secret, e.g. personal/AWS_ACCESS_KEY_ID.
    #[arg(conflicts_with_all=["batch_stdin","batch_file"], required_unless_present_any=["batch_stdin","batch_file","status"])]
    reference: Option<Reference>,
    /// Read a single value from stdin without trimming bytes.
    #[arg(long, requires = "reference", conflicts_with = "value_file")]
    stdin: bool,
    #[arg(long, requires = "reference", conflicts_with = "stdin")]
    value_file: Option<PathBuf>,
    #[arg(long, default_value = "")]
    description: String,
    /// Registered recipient (google:EMAIL, verified email or Portal UUID); read/use only.
    #[arg(long)]
    share: Vec<String>,
    /// Read a JSON array of {namespace,name,description,value,share} from stdin.
    #[arg(long, conflicts_with_all=["batch_file","stdin","value_file","share"])]
    batch_stdin: bool,
    #[arg(long, conflicts_with_all=["batch_stdin","stdin","value_file","share"])]
    batch_file: Option<PathBuf>,
    /// Immutable submission ID; retries never replace the original proposal.
    #[arg(long)]
    request_id: Option<Uuid>,
    /// Fetch this proposal's metadata without submitting values.
    #[arg(long, conflicts_with_all=["reference","batch_stdin","batch_file","stdin","value_file","share","request_id"])]
    status: Option<Uuid>,
    #[arg(long)]
    wait: bool,
}
fn read(mut reader: impl Read) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .by_ref()
        .take(400_001)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Could not read proposal input"))?;
    if bytes.len() > 400_000 {
        bail!("Proposal input exceeds 400000 bytes");
    }
    Ok(bytes)
}
fn from_file(path: &PathBuf) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    read(
        std::fs::File::open(path)
            .map_err(|_| anyhow::anyhow!("Could not open proposal input file"))?,
    )
}
fn parse_batch(bytes: &[u8]) -> anyhow::Result<Vec<SecretDepositInput>> {
    serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("Invalid proposal JSON; expected an array of secret objects"))
}
fn payload(args: &ProposeArgs) -> anyhow::Result<SecretDepositCreate> {
    let items = if args.batch_stdin || args.batch_file.is_some() {
        let bytes = if let Some(path) = &args.batch_file {
            from_file(path)?
        } else {
            read(std::io::stdin().lock())?
        };
        parse_batch(&bytes)?
    } else {
        let reference = args
            .reference
            .as_ref()
            .context("Provide a new secret reference")?;
        let (namespace, name) = match reference {
            Reference::Personal(name) => (SecretNamespace::Personal, name.clone()),
            Reference::Organization(name) => (SecretNamespace::Organization, name.clone()),
            Reference::Id(_) => {
                bail!("Proposals create new named secrets; existing IDs cannot be overwritten")
            }
        };
        let bytes = if args.stdin {
            read(std::io::stdin().lock())?
        } else if let Some(path) = &args.value_file {
            from_file(path)?
        } else {
            bail!("Provide --stdin or --value-file; values are never accepted in arguments")
        };
        let value = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow::anyhow!("Secret values must be UTF-8"))?
            .to_owned();
        vec![SecretDepositInput {
            namespace,
            name,
            description: args.description.clone(),
            value: SecretValue(value),
            share: args.share.clone(),
        }]
    };
    if items.is_empty() || items.len() > 16 {
        bail!("Propose between 1 and 16 secrets");
    }
    Ok(SecretDepositCreate {
        request_id: args.request_id.unwrap_or_else(Uuid::new_v4),
        items,
    })
}
pub(super) async fn run(args: &ProposeArgs, dev: bool) -> anyhow::Result<()> {
    let agent = BoundAgent::from_env(dev)?;
    let (id, mut info) = if let Some(id) = args.status {
        let info: SecretDepositInfo = agent
            .client
            .request::<_, ()>(Method::GET, &format!("/deposits/{id}"), None)
            .await?;
        (id, info)
    } else {
        let request = payload(args)?;
        let id = request.request_id;
        // Print the recovery ID before sending; never include the input payload.
        eprintln!(
            "Secret proposal {id}; retry status with: agent-portal secret propose --status {id}"
        );
        let response = agent
            .client
            .request(Method::POST, "/deposits", Some(&request))
            .await;
        for mut item in request.items {
            zeroize::Zeroize::zeroize(&mut item.value.0);
        }
        (id, response?)
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(610);
    loop {
        if info.id != id || info.session_id != agent.session_id {
            bail!("Proposal response does not match this session");
        }
        if !args.wait || info.state != "pending" || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        info = agent
            .client
            .request::<_, ()>(Method::GET, &format!("/deposits/{id}"), None)
            .await?;
    }
    super::output(&info)?;
    if args.wait && info.state != "approved" {
        bail!("Proposal was not approved; inspect its status before retrying");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_input_does_not_echo_values() {
        let error = parse_batch(br#"[{"value":"PRIVATE", broken}]"#).unwrap_err();
        assert!(!error.to_string().contains("PRIVATE"));
    }
    #[test]
    fn multi_value_json_is_redacted_in_debug() {
        let batch=parse_batch(br#"[{"namespace":"personal","name":"A","value":"PRIVATE_A"},{"namespace":"personal","name":"B","value":"PRIVATE_B"}]"#).unwrap();
        assert_eq!(batch.len(), 2);
        assert!(!format!("{batch:?}").contains("PRIVATE"));
    }
}

#[cfg(test)]
mod cli_tests {
    use clap::Parser;
    #[test]
    fn propose_cli_accepts_file_batches_and_metadata_status() {
        for args in [
            vec![
                "agent-portal",
                "secret",
                "propose",
                "--batch-file",
                "input.json",
                "--wait",
            ],
            vec![
                "agent-portal",
                "secret",
                "propose",
                "personal/NAME",
                "--value-file",
                "value",
                "--share",
                "google:user@example.test",
            ],
            vec![
                "agent-portal",
                "secret",
                "propose",
                "--status",
                "00000000-0000-4000-8000-000000000001",
            ],
        ] {
            assert!(crate::Args::try_parse_from(args).is_ok());
        }
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "propose",
            "personal/NAME",
            "raw-secret-value"
        ])
        .is_err());
    }
}
