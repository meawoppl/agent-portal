//! Literal template substitution using session-bound approvals.
//! Resolve everything first; publish one private file atomically. Never log content.
use super::{bindings::BoundAgent, input::Reference, sensitive::SecretString};
use anyhow::{bail, Context};
use clap::Args;
use reqwest::Method;
use shared::api::{
    SecretAccessGroupInfo, SecretAccessRequestInfo, SecretAccessState, SecretNamespace,
    MAX_SECRET_ACCESS_GROUP_ITEMS,
};
use std::{
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
};
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_TEMPLATE: usize = 1024 * 1024;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_MARKERS: usize = 4096;

#[derive(Debug, Args)]
pub struct InjectArgs {
    /// UTF-8 template containing {{ secret://org/NAME }} placeholders.
    #[arg(short = 'i', long = "in-file", alias = "in")]
    input: PathBuf,
    /// Destination file; stdout is deliberately not supported.
    #[arg(short = 'o', long = "out-file", alias = "out")]
    output: PathBuf,
    /// Replace an existing regular destination file atomically.
    #[arg(long)]
    force: bool,
    /// Approved credential-group request ID, using only the bound agent identity.
    #[arg(long)]
    pub approval: Uuid,
}
struct Marker {
    span: Range<usize>,
    reference: usize,
}
struct Template {
    source: SecretString,
    references: Vec<Reference>,
    markers: Vec<Marker>,
}
impl Template {
    fn parse(source: SecretString) -> anyhow::Result<Self> {
        if source.len() > MAX_TEMPLATE {
            bail!("Template exceeds 1 MiB limit");
        }
        let mut markers = Vec::new();
        let mut references = Vec::new();
        let mut cursor = 0;
        while let Some(offset) = source[cursor..].find("{{") {
            let start = cursor + offset;
            let content_start = start + 2;
            let rest = &source[content_start..];
            if !rest.trim_start().starts_with("secret://") {
                cursor = content_start;
                continue;
            }
            let end = content_start
                + rest
                    .find("}}")
                    .context("Unclosed secret placeholder in template")?;
            let uri = source[content_start..end].trim();
            let reference: Reference = uri.parse().map_err(|_| anyhow::anyhow!("Invalid secret placeholder; use secret://personal/NAME, secret://org/NAME or secret://id/UUID"))?;
            if let Reference::Personal(name) | Reference::Organization(name) = &reference {
                if name.len() > 128
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_- .".contains(&b))
                {
                    bail!("Invalid secret name in template placeholder");
                }
            }
            let index = if let Some(index) = references.iter().position(|r| r == &reference) {
                index
            } else {
                references.push(reference);
                references.len() - 1
            };
            if references.len() > MAX_SECRET_ACCESS_GROUP_ITEMS || markers.len() >= MAX_MARKERS {
                bail!("Template exceeds 16 unique secrets or 4096 substitutions");
            }
            markers.push(Marker {
                span: start..end + 2,
                reference: index,
            });
            cursor = end + 2;
        }
        Ok(Self {
            source,
            references,
            markers,
        })
    }
    fn read(path: &Path) -> anyhow::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|_| anyhow::anyhow!("Could not open template file"))?;
        if !file.metadata()?.is_file() {
            bail!("Template must be a regular file");
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take((MAX_TEMPLATE + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("Could not read template file"))?;
        if bytes.len() > MAX_TEMPLATE {
            bail!("Template exceeds 1 MiB limit");
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow::anyhow!("Template must be UTF-8 text"))?;
        Self::parse(SecretString::new(text.to_owned()))
    }
    fn render(&self, values: &[SecretString]) -> anyhow::Result<SecretString> {
        if values.len() != self.references.len() {
            bail!("Unresolved template references");
        }
        let mut length = self.source.len();
        for marker in &self.markers {
            length = length
                .checked_sub(marker.span.len())
                .and_then(|n| n.checked_add(values[marker.reference].len()))
                .context("Rendered template is too large")?;
            if length > MAX_OUTPUT {
                bail!("Rendered template exceeds 8 MiB limit");
            }
        }
        let mut output = Zeroizing::new(String::with_capacity(length));
        let mut cursor = 0;
        for marker in &self.markers {
            output.push_str(&self.source[cursor..marker.span.start]);
            output.push_str(values[marker.reference].as_str());
            cursor = marker.span.end;
        }
        output.push_str(&self.source[cursor..]);
        Ok(SecretString::new(std::mem::take(&mut *output)))
    }
}
fn prepare(args: &InjectArgs) -> anyhow::Result<Template> {
    if args.output == Path::new("-") {
        bail!("Choose an output file; injection never writes values to stdout");
    }
    if let Ok(metadata) = std::fs::symlink_metadata(&args.output) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("Destination must be a regular file, not a symlink or special file");
        }
        if !args.force {
            bail!("Destination already exists; use --force to replace it");
        }
        if std::fs::canonicalize(&args.input)? == std::fs::canonicalize(&args.output)? {
            bail!("Input template and output must be different files");
        }
    }
    Template::read(&args.input)
}
fn publish(args: &InjectArgs, value: SecretString) -> anyhow::Result<()> {
    let parent = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::Builder::new()
        .prefix(".portal-secret-")
        .tempfile_in(parent)
        .map_err(|_| anyhow::anyhow!("Could not create private output file"))?;
    // tempfile creates mode 0600 on Unix; Windows uses the destination directory ACL.
    file.write_all(value.as_bytes())
        .map_err(|_| anyhow::anyhow!("Could not write injected file"))?;
    file.as_file()
        .sync_all()
        .map_err(|_| anyhow::anyhow!("Could not sync injected file"))?;
    let result = if args.force {
        file.persist(&args.output)
    } else {
        file.persist_noclobber(&args.output)
    };
    result.map_err(|_| {
        anyhow::anyhow!("Could not publish injected file; destination was not replaced")
    })?;
    eprintln!("Secret template injected successfully.");
    Ok(())
}
fn matches(reference: &Reference, item: &SecretAccessRequestInfo) -> bool {
    match reference {
        Reference::Id(id) => *id == item.secret_id,
        Reference::Personal(name) => {
            item.namespace == SecretNamespace::Personal && *name == item.name
        }
        Reference::Organization(name) => {
            item.namespace == SecretNamespace::Organization && *name == item.name
        }
    }
}
fn approved_items<'a>(
    template: &Template,
    group: &'a SecretAccessGroupInfo,
    id: Uuid,
    session: Uuid,
) -> anyhow::Result<Vec<&'a SecretAccessRequestInfo>> {
    if group.id != id
        || group.state != SecretAccessState::Approved
        || group.items.is_empty()
        || group.items.len() > MAX_SECRET_ACCESS_GROUP_ITEMS
        || group.items.iter().any(|i| {
            i.session_id != session
                || i.state != SecretAccessState::Approved
                || i.version < 1
                || i.binding_id.is_none()
        })
    {
        bail!("Credential group is not approved for this session");
    }
    template
        .references
        .iter()
        .map(|reference| {
            let mut found = group.items.iter().filter(|i| matches(reference, i));
            let item = found
                .next()
                .context("Template references a secret outside the approved credential group")?;
            if found.next().is_some() {
                bail!("Template reference is ambiguous in this credential group");
            }
            Ok(item)
        })
        .collect()
}
pub(super) async fn bound(args: &InjectArgs, dev: bool) -> anyhow::Result<()> {
    let approval = args.approval;
    let agent = BoundAgent::from_env(dev)?;
    bound_with_agent(&agent, args, approval).await
}
async fn bound_with_agent(
    agent: &BoundAgent,
    args: &InjectArgs,
    approval: Uuid,
) -> anyhow::Result<()> {
    let template = prepare(args)?;
    let group: SecretAccessGroupInfo = agent
        .client
        .request::<_, ()>(Method::GET, &format!("/request-groups/{approval}"), None)
        .await?;
    let items = approved_items(&template, &group, approval, agent.session_id)?;
    let mut values = Vec::new();
    for item in items {
        let (env, value) = agent
            .redeem_version(
                item.binding_id.context("Missing approved binding")?,
                Some(item.version),
            )
            .await?;
        if env != item.env_name {
            bail!("Binding does not match the approved credential scope");
        }
        values.push(value);
    }
    publish(args, template.render(&values)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    fn template(value: &str) -> Template {
        Template::parse(SecretString::new(value.into())).unwrap()
    }
    fn args(dir: &Path) -> InjectArgs {
        InjectArgs {
            input: dir.join("config.tpl"),
            output: dir.join("config.env"),
            force: false,
            approval: Uuid::new_v4(),
        }
    }
    #[test]
    fn substitutes_literals_once_and_preserves_other_template_syntax() {
        let input = template("π {{ other }}\r\nA={{ secret://org/KEY }}\nB={{secret://org/KEY}}\n");
        assert_eq!(input.references.len(), 1);
        let value = SecretString::new("$HOME\n{{secret://personal/NOT_READ}}".into());
        let rendered = input.render(&[value]).unwrap();
        assert_eq!(rendered.as_str(),"π {{ other }}\r\nA=$HOME\n{{secret://personal/NOT_READ}}\nB=$HOME\n{{secret://personal/NOT_READ}}\n");
        assert!(input.render(&[]).is_err());
        assert_eq!(
            template("plain text").render(&[]).unwrap().as_str(),
            "plain text"
        );
    }
    #[test]
    fn malformed_or_oversized_inputs_fail_without_echoing_content() {
        for source in [
            "{{secret://org/HIDDEN",
            "{{ secret://shared/HIDDEN }}",
            "{{secret://id/HIDDEN}}",
            "{{secret://org/HIDDEN?version=1}}",
        ] {
            let error = Template::parse(SecretString::new(source.into()))
                .err()
                .unwrap();
            assert!(!format!("{error:?}").contains("HIDDEN"));
        }
        assert!(Template::parse(SecretString::new("x".repeat(MAX_TEMPLATE + 1))).is_err());
        let many = (0..17)
            .map(|i| format!("{{{{secret://org/KEY{i}}}}}"))
            .collect::<String>();
        assert!(Template::parse(SecretString::new(many)).is_err());
        let many = "{{secret://org/KEY}}".repeat(129);
        assert!(template(&many)
            .render(&[SecretString::new("x".repeat(65536))])
            .is_err());
    }
    #[test]
    fn file_publication_is_private_no_clobber_and_requires_distinct_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = args(dir.path());
        std::fs::write(&args.input, "{{secret://org/KEY}}").unwrap();
        let parsed = prepare(&args).unwrap();
        publish(
            &args,
            parsed
                .render(&[SecretString::new("first\n".into())])
                .unwrap(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&args.output).unwrap(), "first\n");
        assert!(prepare(&args).is_err());
        assert!(publish(&args, SecretString::new("must-not-overwrite".into())).is_err());
        assert_eq!(std::fs::read_to_string(&args.output).unwrap(), "first\n");
        args.force = true;
        publish(&args, SecretString::new("second".into())).unwrap();
        assert_eq!(std::fs::read_to_string(&args.output).unwrap(), "second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&args.output)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert_eq!(
            std::fs::read_to_string(&args.input).unwrap(),
            "{{secret://org/KEY}}"
        );
        args.output = args.input.clone();
        assert!(prepare(&args).is_err());
        args.output = PathBuf::from("-");
        assert!(prepare(&args).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_destination_without_touching_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = args(dir.path());
        std::fs::write(&args.input, "template").unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, &args.output).unwrap();
        args.force = true;
        assert!(prepare(&args).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
    }
    #[test]
    fn cli_requires_approval_and_file_paths_and_accepts_uri_references() {
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "inject",
            "--approval",
            "00000000-0000-0000-0000-000000000001",
            "--in-file",
            "input",
            "--out-file",
            "output"
        ])
        .is_ok());
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "inject",
            "--approval",
            "00000000-0000-0000-0000-000000000001",
            "--in-file",
            "input"
        ])
        .is_err());
        assert_eq!(
            "secret://org/TOKEN".parse::<Reference>().unwrap(),
            Reference::Organization("TOKEN".into())
        );
        assert_eq!(
            "secret://personal/TOKEN"
                .parse::<Reference>()
                .unwrap()
                .to_string(),
            "personal/TOKEN"
        );
        let id = Uuid::new_v4();
        assert_eq!(
            format!("secret://id/{id}").parse::<Reference>().unwrap(),
            Reference::Id(id)
        );
        assert!("secret://id/123".parse::<Reference>().is_err());
    }
    fn item() -> SecretAccessRequestInfo {
        SecretAccessRequestInfo {
            id: Uuid::new_v4(),
            secret_id: Uuid::new_v4(),
            owner_user_id: Uuid::new_v4(),
            namespace: SecretNamespace::Organization,
            name: "TOKEN".into(),
            version: 3,
            session_id: Uuid::new_v4(),
            launcher_id: Uuid::new_v4(),
            env_name: "TOKEN".into(),
            expires_in_seconds: 900,
            state: SecretAccessState::Approved,
            created_at: String::new(),
            expires_at: String::new(),
            binding_id: Some(Uuid::new_v4()),
        }
    }
    #[test]
    fn approval_scope_must_cover_every_reference_and_the_bound_session() {
        let item = item();
        let session = item.session_id;
        let mut group = SecretAccessGroupInfo {
            id: Uuid::new_v4(),
            state: SecretAccessState::Approved,
            items: vec![item],
        };
        let input = template("{{secret://org/TOKEN}}");
        assert!(approved_items(&input, &group, group.id, session).is_ok());
        assert!(approved_items(
            &template(&format!("{{{{secret://id/{}}}}}", group.items[0].secret_id)),
            &group,
            group.id,
            session
        )
        .is_ok());
        assert!(approved_items(
            &template("{{secret://personal/TOKEN}}"),
            &group,
            group.id,
            session
        )
        .is_err());
        assert!(approved_items(
            &template("{{secret://org/MISSING}}"),
            &group,
            group.id,
            session
        )
        .is_err());
        assert!(approved_items(&input, &group, group.id, Uuid::new_v4()).is_err());
        group.items.push(group.items[0].clone());
        assert!(approved_items(&input, &group, group.id, session).is_err());
        group.items.pop();
        group.items[0].state = SecretAccessState::Expired;
        assert!(approved_items(&input, &group, group.id, session).is_err());
    }
    #[tokio::test]
    async fn bound_injection_redeems_only_matching_approved_version_and_deduplicates() {
        use super::super::requests::tests::mock_server;
        for wrong_version in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let args = args(dir.path());
            std::fs::write(
                &args.input,
                "one={{secret://org/TOKEN}}\ntwo={{secret://org/TOKEN}}\n",
            )
            .unwrap();
            let item = item();
            let session = item.session_id;
            let group = SecretAccessGroupInfo {
                id: Uuid::new_v4(),
                state: SecretAccessState::Approved,
                items: vec![item],
            };
            let (url, server) = mock_server(vec![
                serde_json::to_value(&group).unwrap(),
                serde_json::json!({"ticket":"private-ticket","expires_in":30}),
                serde_json::json!({"env_name":"TOKEN","version":if wrong_version {2} else {3},"value":"canary"}),
            ]);
            let agent = BoundAgent::from_values(
                Some(url),
                Some("session-token".into()),
                Some(session.to_string()),
                true,
            )
            .unwrap();
            let result = bound_with_agent(&agent, &args, group.id).await;
            if wrong_version {
                assert!(result.is_err());
                assert!(!args.output.exists());
            } else {
                result.unwrap();
                assert_eq!(
                    std::fs::read_to_string(&args.output).unwrap(),
                    "one=canary\ntwo=canary\n"
                );
            }
            assert_eq!(server.join().unwrap().len(), 3);
        }
    }
}
