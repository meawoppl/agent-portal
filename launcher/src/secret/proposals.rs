//! Values enter through stdin/files, never argv or status output.
use super::{bindings::BoundAgent, client::ServiceError, input::Reference};
use anyhow::{bail, Context};
use clap::Args;
use reqwest::Method;
use shared::api::*;
use std::{io::Read, path::PathBuf};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Secret to create or update, e.g. personal/AWS_ACCESS_KEY_ID.
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
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let items = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        // Never format the serde error or raw path: both may contain secret
        // values or attacker-controlled keys. Emit only known field names.
        let mut item = None;
        let mut field = None;
        for segment in error.path() {
            match segment {
                serde_path_to_error::Segment::Seq { index } if item.is_none() => item = Some(index + 1),
                serde_path_to_error::Segment::Map { key } => field = match key.as_str() {
                    "share" => Some("share must be an array of recipient strings, for example [\"google:user@example.test\"]"),
                    "namespace" => Some("namespace must be \"personal\" or \"organization\""),
                    "name" => Some("name must be a string"),
                    "description" => Some("description must be a string"),
                    "value" => Some("value must be a string"),
                    _ => None,
                },
                _ => {},
            }
        }
        if error.inner().is_syntax() || error.inner().is_eof() {
            anyhow::anyhow!("Invalid batch JSON syntax; input values are omitted")
        } else if let (Some(item), Some(field)) = (item, field) {
            anyhow::anyhow!("Invalid batch item {item}: {field}")
        } else if let Some(item) = item {
            anyhow::anyhow!("Invalid batch item {item}; expected a secret object with namespace, name and value")
        } else {
            anyhow::anyhow!("Invalid batch JSON; expected an array of secret objects")
        }
    })?;
    deserializer
        .end()
        .map_err(|_| anyhow::anyhow!("Invalid batch JSON syntax; input values are omitted"))?;
    Ok(items)
}
fn payload(args: &SetArgs) -> anyhow::Result<SecretDepositCreate> {
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
            .context("Provide a named secret reference")?;
        let (namespace, name) = match reference {
            Reference::Personal(name) => (SecretNamespace::Personal, name.clone()),
            Reference::Organization(name) => (SecretNamespace::Organization, name.clone()),
            Reference::Id(_) => {
                bail!("Use a personal/NAME or org/NAME reference with set")
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
pub(super) fn proposal_error(error: anyhow::Error, submitting: bool) -> anyhow::Error {
    if error
        .downcast_ref::<ServiceError>()
        .is_some_and(|e| e.status == 404)
    {
        if submitting {
            anyhow::anyhow!("Secret set submission rejected before approval: target secret or recipient unavailable. For email sharing, ask the recipient to sign in through their provider, or use their registered Portal user UUID. No new approval proposal was created.")
        } else {
            anyhow::anyhow!("Proposal not found for this session. A submission rejected before approval has no saved status; check the original submission error and request ID.")
        }
    } else {
        error
    }
}
pub(super) async fn run(args: &SetArgs, dev: bool) -> anyhow::Result<()> {
    let agent = BoundAgent::from_env(dev)?;
    let (id, mut info) = if let Some(id) = args.status {
        let info: SecretDepositInfo = agent
            .client
            .request::<_, ()>(Method::GET, &format!("/deposits/{id}"), None)
            .await
            .map_err(|e| proposal_error(e, false))?;
        (id, info)
    } else {
        let request = payload(args)?;
        let id = request.request_id;
        // Print the recovery ID before sending; never include the input payload.
        eprintln!("Secret proposal {id}; if the submission result is uncertain, check: agent-portal secret set --status {id}");
        let response = agent
            .client
            .request(Method::POST, "/deposits", Some(&request))
            .await;
        for mut item in request.items {
            zeroize::Zeroize::zeroize(&mut item.value.0);
        }
        (id, response.map_err(|e| proposal_error(e, true))?)
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
    fn batch_errors_name_only_safe_fields_and_item_positions() {
        for share in [r#""PRIVATE_RECIPIENT""#, r#"[42]"#, r#"null"#] {
            let input = format!(
                r#"[{{"namespace":"personal","name":"PRIVATE_NAME","value":"PRIVATE_VALUE","share":{share}}}]"#
            );
            let error = parse_batch(input.as_bytes()).unwrap_err();
            let shown = format!("{error:#?}");
            assert!(shown.contains("item 1: share must be an array"), "{shown}");
            assert!(!shown.contains("PRIVATE"));
        }
        for (input, expected) in [
            (r#"{}"#, "expected an array"),
            (r#"["PRIVATE"]"#, "item 1; expected a secret object"),
            (
                r#"[{"namespace":"PRIVATE","name":"A","value":"PRIVATE"}]"#,
                "namespace must be",
            ),
            (
                r#"[{"namespace":"personal","name":"A","value":42}]"#,
                "value must be a string",
            ),
            (r#"[] PRIVATE"#, "Invalid batch JSON syntax"),
        ] {
            let shown = format!("{:#?}", parse_batch(input.as_bytes()).unwrap_err());
            assert!(shown.contains(expected), "{shown}");
            assert!(!shown.contains("PRIVATE"));
        }
        let valid = br#"[{"namespace":"personal","name":"A","value":"PRIVATE","share":["google:user@example.test","00000000-0000-4000-8000-000000000001"]}]"#;
        assert_eq!(parse_batch(valid).unwrap()[0].share.len(), 2);
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
    fn set_cli_accepts_file_batches_and_metadata_status() {
        for args in [
            vec![
                "agent-portal",
                "secret",
                "set",
                "--batch-file",
                "input.json",
                "--wait",
            ],
            vec![
                "agent-portal",
                "secret",
                "set",
                "personal/NAME",
                "--value-file",
                "value",
                "--share",
                "google:user@example.test",
            ],
            vec![
                "agent-portal",
                "secret",
                "set",
                "--status",
                "00000000-0000-4000-8000-000000000001",
            ],
        ] {
            assert!(crate::Args::try_parse_from(args).is_ok());
        }
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "set",
            "personal/NAME",
            "raw-secret-value"
        ])
        .is_err());
    }
}
