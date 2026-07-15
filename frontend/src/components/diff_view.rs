//! On-demand working-directory diff viewer (#1329), rendered inline as a
//! session-view tab (not a modal).
//!
//! Fetches `GET /api/sessions/{id}/diff` (a `git diff HEAD` run on the backend
//! host) and renders it as one framed `DiffCard` per changed file, reusing the
//! shared unified-diff parser/renderer from [`crate::components::diff`]. The
//! panel fetches once on mount and offers a manual refresh.

use shared::api::SessionDiffResponse;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::components::diff::{split_git_diff, DiffCard, DiffSource, GitDiffFile};
use crate::utils::{self, On401};

#[derive(Properties, PartialEq)]
pub struct SessionDiffPanelProps {
    pub session_id: Uuid,
    /// Directory shown in the header while the diff loads (from the session
    /// record). The response echoes back the authoritative path.
    pub working_directory: String,
}

pub enum SessionDiffPanelMsg {
    Refresh,
    Loaded(SessionDiffResponse),
    Failed(String),
}

pub struct SessionDiffPanel {
    diff: Option<SessionDiffResponse>,
    error: Option<String>,
    loading: bool,
}

impl SessionDiffPanel {
    fn fetch(&mut self, ctx: &Context<Self>) {
        self.loading = true;
        self.error = None;
        let session_id = ctx.props().session_id;
        let link = ctx.link().clone();
        spawn_local(async move {
            match utils::fetch_json::<SessionDiffResponse>(
                &format!("/api/sessions/{}/diff", session_id),
                On401::Ignore,
            )
            .await
            {
                Ok(data) => link.send_message(SessionDiffPanelMsg::Loaded(data)),
                Err(e) => {
                    log::error!("Failed to load diff: {}", e);
                    link.send_message(SessionDiffPanelMsg::Failed(
                        "Failed to load diff".to_string(),
                    ));
                }
            }
        });
    }
}

impl Component for SessionDiffPanel {
    type Message = SessionDiffPanelMsg;
    type Properties = SessionDiffPanelProps;

    fn create(ctx: &Context<Self>) -> Self {
        let mut panel = Self {
            diff: None,
            error: None,
            loading: false,
        };
        panel.fetch(ctx);
        panel
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            SessionDiffPanelMsg::Refresh => {
                self.fetch(ctx);
            }
            SessionDiffPanelMsg::Loaded(data) => {
                self.diff = Some(data);
                self.error = None;
                self.loading = false;
            }
            SessionDiffPanelMsg::Failed(err) => {
                self.error = Some(err);
                self.loading = false;
            }
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let on_refresh = ctx.link().callback(|_| SessionDiffPanelMsg::Refresh);
        let path = self
            .diff
            .as_ref()
            .map(|d| d.working_directory.clone())
            .unwrap_or_else(|| ctx.props().working_directory.clone());

        html! {
            <div class="session-diff-panel">
                <div class="session-diff-toolbar">
                    <span class="session-diff-path" title={path.clone()}>{ path }</span>
                    <button
                        class="session-diff-refresh"
                        onclick={on_refresh}
                        disabled={self.loading}
                        title="Re-run git diff"
                    >
                        { if self.loading { "Refreshing…" } else { "Refresh" } }
                    </button>
                </div>
                <div class="session-diff-body">
                    { self.view_body() }
                </div>
            </div>
        }
    }
}

impl SessionDiffPanel {
    fn view_body(&self) -> Html {
        if let Some(error) = &self.error {
            return html! { <div class="session-diff-message error">{ error }</div> };
        }
        let Some(diff) = &self.diff else {
            return html! { <div class="session-diff-message">{ "Loading diff…" }</div> };
        };

        if !diff.is_git_repo {
            return html! {
                <div class="session-diff-message">
                    { "No git repository is reachable at this working directory on the \
                       server host, so a diff can't be shown." }
                </div>
            };
        }

        let files = split_git_diff(&diff.diff);
        if files.is_empty() {
            return html! {
                <div class="session-diff-message">
                    { "No uncommitted changes in the working directory." }
                </div>
            };
        }

        html! {
            <>
                { for files.into_iter().map(render_file) }
            </>
        }
    }
}

/// Render one changed file as a `DiffCard`. Files without textual hunks
/// (binary, or a pure rename/mode change) show just the header row.
fn render_file(file: GitDiffFile) -> Html {
    let path = AttrValue::from(file.path);
    let kind = AttrValue::from(file.kind);
    if file.hunks.is_empty() {
        html! {
            <div class="diff-card">
                <div class="diff-card-header">
                    <span class="tool-icon">{ "\u{1f4dd}" }</span>
                    <span class={classes!("diff-card-kind", kind.to_string())}>{ kind }</span>
                    <span class="diff-card-path">{ path }</span>
                    <span class="diff-card-cumulative">{ "no textual changes" }</span>
                </div>
            </div>
        }
    } else {
        let source = DiffSource::Unified { text: file.hunks };
        html! { <DiffCard {source} file_path={path} kind={kind} /> }
    }
}
