use super::*;
use shared::api::{
    SecretAccessGroupCreateRequest, SecretAccessGroupInfo, SecretAccessGroupItem,
    MAX_SECRET_ACCESS_GROUP_ITEMS,
};
fn payload(args: &RequestArgs) -> anyhow::Result<SecretAccessGroupCreateRequest> {
    if args.credentials.len() > MAX_SECRET_ACCESS_GROUP_ITEMS {
        bail!("Request at most 16 credentials");
    }
    let mut envs = std::collections::HashSet::new();
    for mapping in &args.credentials {
        if !envs.insert(mapping.name.to_ascii_uppercase()) {
            bail!("Duplicate environment variable");
        }
    }
    Ok(SecretAccessGroupCreateRequest {
        request_id: args.request_id.unwrap_or_else(Uuid::new_v4),
        expires_in_seconds: args.expires_in,
        items: args
            .credentials
            .iter()
            .map(|m| SecretAccessGroupItem {
                reference: m.reference.to_string(),
                env_name: m.name.clone(),
                version: None,
            })
            .collect(),
    })
}
fn validate(
    agent: &BoundAgent,
    request: &SecretAccessGroupCreateRequest,
    info: &SecretAccessGroupInfo,
) -> anyhow::Result<()> {
    if info.id != request.request_id || info.items.len() != request.items.len() {
        bail!("Credential set response does not match request");
    }
    for (index, (expected, item)) in request.items.iter().zip(&info.items).enumerate() {
        validate_scope(
            agent,
            &SecretAccessCreateRequest {
                request_id: Uuid::new_v5(
                    &request.request_id,
                    format!("secret-access-group:v1:{index}").as_bytes(),
                ),
                reference: expected.reference.clone(),
                env_name: expected.env_name.clone(),
                version: expected.version,
                expires_in_seconds: request.expires_in_seconds,
            },
            item,
        )?;
        if info.state == SecretAccessState::Approved && item.state != SecretAccessState::Approved {
            bail!("Credential set was only partially approved");
        }
    }
    Ok(())
}
pub(super) async fn run(args: &RequestArgs, dev: bool) -> anyhow::Result<()> {
    let request = payload(args)?;
    let agent = BoundAgent::from_env(dev)?;
    eprintln!("Credential set {}: one approval card appears in this session. Reuse --request-id {} with identical arguments after a retry.", request.request_id, request.request_id);
    let mut info: SecretAccessGroupInfo = agent
        .client
        .request(Method::POST, "/request-groups", Some(&request))
        .await
        .map_err(request_error)?;
    validate(&agent, &request, &info)?;
    let expires = info
        .items
        .iter()
        .map(|i| chrono::DateTime::parse_from_rfc3339(&i.expires_at))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min()
        .context("Empty credential set")?;
    let remaining = expires
        .with_timezone(&chrono::Utc)
        .signed_duration_since(chrono::Utc::now())
        .to_std()
        .unwrap_or_default();
    let deadline = tokio::time::Instant::now() + remaining.min(MAX_WAIT);
    let mut timed_out = false;
    while args.wait && info.state == SecretAccessState::Pending {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            timed_out = true;
            break;
        }
        tokio::time::sleep(POLL_INTERVAL.min(remaining)).await;
        let path = format!("/request-groups/{}", request.request_id);
        let fetch = agent
            .client
            .request::<SecretAccessGroupInfo, ()>(Method::GET, &path, None);
        let next = match tokio::time::timeout_at(deadline, fetch).await {
            Ok(result) => result.map_err(request_error)?,
            Err(_) => {
                timed_out = true;
                break;
            }
        };
        validate(&agent, &request, &next)?;
        for (previous, next) in info.items.iter().zip(&next.items) {
            validate_pinned_scope(previous, next)?;
        }
        info = next;
    }
    super::super::output(&info)?;
    if timed_out {
        bail!("Approval still pending; retry with the same --request-id");
    }
    if !matches!(
        info.state,
        SecretAccessState::Pending | SecretAccessState::Approved
    ) {
        bail!("Credential set was denied, expired, or invalidated");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn repeated_credentials_have_stable_retry_identity_and_reject_ambiguous_scope() {
        let id = Uuid::new_v4().to_string();
        let args = crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "request",
            "--credential",
            "TOKEN=personal/TOKEN",
            "--credential",
            "OTHER=org/OTHER",
            "--request-id",
            &id,
            "--wait",
        ])
        .unwrap();
        let Some(crate::Command::Secret {
            action: super::super::super::Action::Request(args),
        }) = args.command
        else {
            panic!("wrong action");
        };
        let request = payload(&args).unwrap();
        assert_eq!(request.request_id.to_string(), id);
        assert_eq!(request.items.len(), 2);
        assert_eq!(request.items[1].reference, "org/OTHER");
        assert!(args.wait);
        assert!(crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "request",
            "personal/TOKEN",
            "--env",
            "TOKEN",
            "--credential",
            "OTHER=org/OTHER"
        ])
        .is_err());
        let args = crate::Args::try_parse_from([
            "agent-portal",
            "secret",
            "request",
            "--credential",
            "TOKEN=personal/TOKEN",
            "--credential",
            "TOKEN=org/OTHER",
        ])
        .unwrap();
        let Some(crate::Command::Secret {
            action: super::super::super::Action::Request(args),
        }) = args.command
        else {
            panic!("wrong action");
        };
        assert!(payload(&args).is_err());
    }
}
