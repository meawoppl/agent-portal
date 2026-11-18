//! Live, unpersisted status shown after the transcript.

use super::helpers::{format_tool_elapsed, ActiveToolProgress, PermissionCheck};
use yew::prelude::*;

#[derive(Clone, Default, PartialEq)]
pub struct LiveStatusSnapshot {
    pub(crate) active_tools: Vec<ActiveToolProgress>,
    pub(crate) permission_checks: Vec<PermissionCheck>,
    pub(crate) ephemeral_status: Option<String>,
}

#[derive(Properties, PartialEq)]
pub struct LiveStatusProps {
    pub on_register: Callback<Callback<LiveStatusSnapshot>>,
    pub on_rendered: Callback<()>,
}

#[derive(Default)]
pub struct LiveStatus {
    snapshot: LiveStatusSnapshot,
}

impl Component for LiveStatus {
    type Message = LiveStatusSnapshot;
    type Properties = LiveStatusProps;

    fn create(ctx: &Context<Self>) -> Self {
        ctx.props()
            .on_register
            .emit(ctx.link().callback(|snapshot| snapshot));
        Self::default()
    }

    fn update(&mut self, _ctx: &Context<Self>, snapshot: Self::Message) -> bool {
        if self.snapshot == snapshot {
            return false;
        }
        self.snapshot = snapshot;
        true
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if !first_render {
            ctx.props().on_rendered.emit(());
        }
    }

    fn view(&self, _ctx: &Context<Self>) -> Html {
        html! {
            <>
                { self.render_active_tools() }
                { for self.snapshot.permission_checks.iter().map(|check| html! {
                    <div class="ephemeral-status-strip" role="status" title={check.tool_use_id.clone()}>
                        <span class="ephemeral-status-spinner" />
                        <span class="ephemeral-status-text">{ "Checking tool permission..." }</span>
                    </div>
                }) }
                { self.render_ephemeral_status() }
            </>
        }
    }
}

impl LiveStatus {
    fn render_active_tools(&self) -> Html {
        if self.snapshot.active_tools.is_empty() {
            return html! {};
        }
        html! {
            <div class="active-tool-strip">
                { for self.snapshot.active_tools.iter().map(|tool| {
                    html! {
                        <div class="active-tool-pill" key={tool.key.clone()}>
                            <span class="active-tool-spinner" />
                            <span class="active-tool-name">{ tool.tool_name.clone() }</span>
                            // Which Task sub-agent is running (#1474) — a long
                            // `Task` otherwise reads as an anonymous stall.
                            if let Some(subagent) = tool.subagent_type.as_deref() {
                                <span class="active-tool-subagent">{ subagent }</span>
                            }
                            <span class="active-tool-status">
                                { format!("running — {}", format_tool_elapsed(tool.elapsed_seconds)) }
                            </span>
                            // Only while the sub-agent is retrying: says the
                            // turn is flaky-but-progressing, not hung.
                            if let Some(retry) = tool.subagent_retry.as_ref() {
                                <span class="active-tool-retry">
                                    { format!(
                                        "retrying {}/{} — {}",
                                        retry.attempt, retry.max_retries, retry.error_category
                                    ) }
                                </span>
                            }
                        </div>
                    }
                }) }
            </div>
        }
    }

    fn render_ephemeral_status(&self) -> Html {
        let Some(status) = self.snapshot.ephemeral_status.as_deref() else {
            return html! {};
        };
        html! {
            <div class="ephemeral-status-strip">
                <span class="ephemeral-status-spinner" />
                <span class="ephemeral-status-text">{ status }</span>
            </div>
        }
    }
}
