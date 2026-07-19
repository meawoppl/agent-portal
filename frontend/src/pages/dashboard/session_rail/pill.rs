use crate::utils::extract_folder;
use shared::SessionInfo;
use uuid::Uuid;
use web_sys::MouseEvent;
use yew::prelude::*;

use super::sorted_prs;
use super::sparkline::{render_activity_sparkline, ActivityRef};

#[derive(Clone, Debug, PartialEq, Eq)]
enum VcsView {
    PullRequests(Vec<(i64, String)>),
    Branch(String),
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PillViewModel {
    pill_classes: Vec<&'static str>,
    watermark_class: &'static str,
    connection_class: &'static str,
    connection_symbol: &'static str,
    number_annotation: Option<String>,
    session_name: String,
    repo_label: String,
    repo_title: String,
    vcs: VcsView,
    agent_badge: Option<(&'static str, &'static str)>,
    hidden_badge: bool,
    role_badge_class: Option<String>,
}

#[derive(Properties, PartialEq)]
pub(super) struct SessionPillProps {
    pub index: usize,
    pub display_number: Option<usize>,
    pub session: SessionInfo,
    pub is_focused: bool,
    pub is_awaiting: bool,
    pub is_hidden: bool,
    pub is_connected: bool,
    pub nav_mode: bool,
    pub server_version: String,
    pub activity_timestamps: ActivityRef,
    pub is_broadcast_sender: bool,
    pub is_broadcast_receiver: bool,
    pub render_time: f64,
    pub on_select: Callback<usize>,
    pub on_toggle_menu: Callback<(Uuid, MouseEvent)>,
}

#[function_component(SessionPill)]
pub(super) fn session_pill(props: &SessionPillProps) -> Html {
    let session = &props.session;
    let view = PillViewModel::new(props);

    let on_click = {
        let on_select = props.on_select.clone();
        let index = props.index;
        Callback::from(move |_| on_select.emit(index))
    };

    let on_toggle_menu = {
        let on_toggle_menu = props.on_toggle_menu.clone();
        let session_id = session.id;
        Callback::from(move |e: MouseEvent| on_toggle_menu.emit((session_id, e)))
    };

    let sparkline =
        render_activity_sparkline(&props.activity_timestamps, session.id, props.render_time);

    html! {
        <div
            class={classes!(view.pill_classes)}
            onclick={on_click}
            key={session.id.to_string()}
            data-index={props.index.to_string()}
        >
            <span class={view.watermark_class} aria-hidden="true" />
            {
                if let Some(num) = &view.number_annotation {
                    html! { <span class="pill-number">{ num }</span> }
                } else {
                    html! {}
                }
            }
            <span class={view.connection_class}>
                { view.connection_symbol }
            </span>
            <span class="pill-name" title={view.repo_title.clone()}>
                <span class="pill-session-name" title={session.session_name.clone()}>
                    { view.session_name }
                </span>
                <span class="pill-repo" title={view.repo_title.clone()}>
                    { view.repo_label }
                </span>
                { render_vcs(&view.vcs) }
            </span>
            // Codex text badge removed: the agent-type watermark behind the
            // pill (anthropic-mark.svg / openai-mark.png) carries this signal.
            {
                if let Some((class, label)) = view.agent_badge {
                    html! { <span class={classes!("pill-agent-badge", class)}>{ label }</span> }
                } else {
                    html! {}
                }
            }
            {
                if view.hidden_badge {
                    html! { <span class="pill-hidden-badge">{ "ᴴ" }</span> }
                } else {
                    html! {}
                }
            }
            {
                if let Some(role_class) = view.role_badge_class {
                    html! { <span class={role_class}>{ session.my_role.as_str() }</span> }
                } else {
                    html! {}
                }
            }
            <button type="button" class="pill-menu-toggle" onclick={on_toggle_menu}>
                { "▼" }
            </button>
            { sparkline }
        </div>
    }
}

impl PillViewModel {
    fn new(props: &SessionPillProps) -> Self {
        let session = &props.session;
        let is_status_disconnected = session.status.as_str() != "active";

        let mut pill_classes = vec!["session-pill"];
        if props.is_focused {
            pill_classes.push("focused");
        }
        if props.is_awaiting {
            pill_classes.push("awaiting");
        }
        if props.is_hidden {
            pill_classes.push("hidden");
        }
        if props.nav_mode {
            pill_classes.push("nav-mode");
        }
        if props.is_broadcast_sender {
            pill_classes.push("broadcast-sender");
        }
        if props.is_broadcast_receiver {
            pill_classes.push("broadcast-receiver");
        }
        if is_status_disconnected {
            pill_classes.push("status-disconnected");
        }

        let number_annotation = if props.nav_mode {
            props
                .display_number
                .filter(|&n| n < 9)
                .map(|n| format!("{}", n + 1))
        } else {
            None
        };

        let (repo_label, repo_title) = repo_context(session);

        let vcs = {
            let prs = sorted_prs(&session.open_prs);
            if !prs.is_empty() {
                VcsView::PullRequests(
                    prs.iter()
                        .map(|pr| (pr.number, pr.branch.clone()))
                        .collect(),
                )
            } else if let Some(branch) = &session.git_branch {
                VcsView::Branch(branch.clone())
            } else {
                VcsView::None
            }
        };

        let agent_badge = if session.scheduled_task_id.is_some() {
            Some(("cron", "Cron"))
        } else if session.paused {
            Some(("paused", "Paused"))
        } else {
            None
        };

        let role_badge_class = (session.my_role != shared::SessionRole::Owner)
            .then(|| format!("pill-role-badge role-{}", session.my_role.as_str()));

        Self {
            pill_classes,
            watermark_class: match session.agent_type {
                shared::AgentType::Claude => "pill-watermark claude",
                shared::AgentType::Codex => "pill-watermark codex",
            },
            connection_class: if props.is_connected {
                "pill-status connected"
            } else {
                "pill-status disconnected"
            },
            connection_symbol: if props.is_connected { "●" } else { "○" },
            number_annotation,
            session_name: session.session_name.clone(),
            repo_label,
            repo_title,
            vcs,
            agent_badge,
            hidden_badge: props.is_hidden,
            role_badge_class,
        }
    }
}

fn repo_context(session: &SessionInfo) -> (String, String) {
    if let Some(repo_url) = session.repo_url.as_deref() {
        let label = repo_label_from_url(repo_url);
        if !label.is_empty() {
            return (label, repo_url.to_string());
        }
    }

    let fallback = extract_folder(&session.working_directory);
    (fallback.to_string(), session.working_directory.clone())
}

fn render_vcs(vcs: &VcsView) -> Html {
    match vcs {
        VcsView::PullRequests(prs) => html! {
            <span class="pill-branch pill-prs">
                { for prs.iter().map(|(number, branch)| html! {
                    <span class="pill-pr-num" title={branch.clone()}>
                        { format!("#{number}") }
                    </span>
                }) }
            </span>
        },
        VcsView::Branch(branch) => {
            html! { <span class="pill-branch" title={branch.clone()}>{ branch }</span> }
        }
        VcsView::None => html! { <span class="pill-branch pill-no-vcs">{ "No VCS" }</span> },
    }
}

fn repo_label_from_url(repo_url: &str) -> String {
    let trimmed = repo_url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/');

    let path = if let Some((_, path)) = trimmed.split_once("://") {
        path.split_once('/').map(|(_, path)| path).unwrap_or(path)
    } else if let Some((_, path)) = trimmed.split_once(':') {
        path
    } else {
        trimmed
    };

    let parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [.., owner, repo] => format!("{owner}/{repo}"),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_label_from_url_uses_owner_and_repo() {
        assert_eq!(
            repo_label_from_url("https://github.com/meawoppl/agent-portal.git"),
            "meawoppl/agent-portal"
        );
        assert_eq!(
            repo_label_from_url("git@github.com:meawoppl/agent-portal.git"),
            "meawoppl/agent-portal"
        );
    }

    #[test]
    fn repo_label_from_url_strips_query_and_fragment() {
        assert_eq!(
            repo_label_from_url("https://github.com/meawoppl/agent-portal.git?tab=readme#main"),
            "meawoppl/agent-portal"
        );
    }

    #[test]
    fn render_vcs_prefers_pull_requests_over_branch() {
        let prs = VcsView::PullRequests(vec![(12, "feature/a".to_string())]);
        let branch = VcsView::Branch("main".to_string());
        let none = VcsView::None;

        assert_ne!(prs, branch);
        assert_ne!(branch, none);
    }
}
