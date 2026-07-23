//! Transcript view (`#/session/{user}/{session}`): manifest header card plus
//! the archived messages rendered with the real portal renderers.

use std::str::FromStr;

use frontend::viewer_api::{group_messages, MessageGroupRenderer, RenderedMessage};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api::{self, FetchError, Manifest, MessageLine};
use crate::media_rewrite::rewrite_media_urls;

type Load<T> = Option<Result<T, FetchError>>;

#[derive(Properties, PartialEq)]
pub struct TranscriptProps {
    pub user: String,
    pub session: String,
}

#[function_component(TranscriptView)]
pub fn transcript_view(props: &TranscriptProps) -> Html {
    let manifest = use_state(|| None as Load<Manifest>);
    let messages = use_state(|| None as Load<Vec<MessageLine>>);

    {
        let manifest = manifest.clone();
        let messages = messages.clone();
        let user = props.user.clone();
        let session = props.session.clone();
        use_effect_with((user.clone(), session.clone()), move |(user, session)| {
            let path = format!("/api/sessions/{user}/{session}/manifest");
            {
                let manifest = manifest.clone();
                spawn_local(async move {
                    manifest.set(Some(api::fetch_json::<Manifest>(&path).await));
                });
            }
            {
                let (user, session) = (user.clone(), session.clone());
                spawn_local(async move {
                    messages.set(Some(api::fetch_messages(&user, &session).await));
                });
            }
            || ()
        });
    }

    // Agent type governs grouping; fall back to Claude if the manifest is not
    // (yet) loaded or carries an unknown value.
    let agent_type = match &*manifest {
        Some(Ok(m)) => shared::AgentType::from_str(&m.agent_type).unwrap_or_default(),
        _ => shared::AgentType::default(),
    };
    let session_id = Uuid::from_str(&props.session).unwrap_or(Uuid::nil());

    html! {
        <div class="viewer-root viewer-transcript">
            <nav class="viewer-nav"><a href="#/">{ "← All sessions" }</a></nav>
            { header_card(&manifest) }
            { transcript_body(&messages, &props.user, &props.session, agent_type, session_id) }
        </div>
    }
}

fn header_card(manifest: &Load<Manifest>) -> Html {
    match manifest {
        None => html! { <div class="viewer-loading">{ "Loading manifest…" }</div> },
        Some(Err(e)) => html! {
            <div class="viewer-error">{ format!("Could not load manifest: {e}") }</div>
        },
        Some(Ok(m)) => {
            let name = if m.session_name.is_empty() {
                m.session_id.clone()
            } else {
                m.session_name.clone()
            };
            html! {
                <div class="viewer-manifest-card">
                    <h2>{ name }</h2>
                    <div class="manifest-grid">
                        { field("Agent", &m.agent_type) }
                        { field("Status", &m.status) }
                        { field("User", &owner(m)) }
                        { field("Host", &m.hostname) }
                        { field("Directory", &m.working_directory) }
                        { opt_field("Branch", &m.git_branch) }
                        { opt_field("Repo", &m.repo_url) }
                        { opt_field("PR", &m.pr_url) }
                        { field("Created", &m.created_at) }
                        { field("Last activity", &m.last_activity) }
                        { field("Archived", &m.archived_at) }
                        { field("Tokens", &m.tokens.total().to_string()) }
                        { field("Cost", &format!("${:.4}", m.total_cost_usd)) }
                    </div>
                    <div class="manifest-provenance">
                        { provenance("launcher", &m.launcher_version) }
                        { provenance("client", &m.client_version) }
                        { provenance("archiver", &m.archived_by_version) }
                    </div>
                </div>
            }
        }
    }
}

fn owner(m: &Manifest) -> String {
    match &m.owner_name {
        Some(n) if !n.is_empty() => format!("{n} <{}>", m.owner_email),
        _ => m.owner_email.clone(),
    }
}

fn field(label: &str, value: &str) -> Html {
    if value.is_empty() {
        return Html::default();
    }
    html! {
        <div class="manifest-field">
            <span class="manifest-key">{ label }</span>
            <span class="manifest-val">{ value }</span>
        </div>
    }
}

fn opt_field(label: &str, value: &Option<String>) -> Html {
    match value {
        Some(v) if !v.is_empty() => field(label, v),
        _ => Html::default(),
    }
}

fn provenance(label: &str, version: &Option<String>) -> Html {
    let v = version.clone().filter(|s| !s.is_empty());
    html! {
        <span class="provenance-chip">
            { format!("{label}: {}", v.unwrap_or_else(|| "—".to_string())) }
        </span>
    }
}

fn transcript_body(
    messages: &Load<Vec<MessageLine>>,
    user: &str,
    session: &str,
    agent_type: shared::AgentType,
    session_id: Uuid,
) -> Html {
    match messages {
        None => html! { <div class="viewer-loading">{ "Loading transcript…" }</div> },
        Some(Err(e)) => html! {
            <div class="viewer-error">{ format!("Could not load transcript: {e}") }</div>
        },
        Some(Ok(lines)) if lines.is_empty() => html! {
            <div class="viewer-empty">{ "This session has no archived messages." }</div>
        },
        Some(Ok(lines)) => {
            let rendered: Vec<RenderedMessage> = lines
                .iter()
                .map(|line| to_rendered(line, user, session))
                .collect();
            let groups = group_messages(&rendered, agent_type, None);
            html! {
                <div class="messages-container">
                    { for groups.into_iter().map(|group| html! {
                        <MessageGroupRenderer {group} {session_id} {agent_type} />
                    }) }
                </div>
            }
        }
    }
}

/// Build a `RenderedMessage` from one archived line: serialize the stored
/// content back to its wire JSON string, rewrite archived media URLs to the
/// viewer's session-scoped endpoint, and attach the row timestamp so the
/// renderers show real times.
fn to_rendered(line: &MessageLine, user: &str, session: &str) -> RenderedMessage {
    let raw = serde_json::to_string(&line.content).unwrap_or_else(|_| "{}".to_string());
    let content = rewrite_media_urls(&raw, user, session);
    let meta = shared::PortalMeta {
        created_at: (!line.created_at.is_empty()).then(|| line.created_at.clone()),
        ..Default::default()
    };
    RenderedMessage::new(content, Some(meta))
}
