// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Admin ▸ Visual PRs: list open pull requests, kick off background
//! generation of a before/after summary SVG (the `.claude/skills/visual-pr`
//! house style), and click into a ready preview to approve (squash-merge)
//! the PR. Testing feature — previews live in backend memory only.

use gloo::timers::callback::Interval;
use gloo_net::http::Request;
use shared::api::{
    VisualPrApproveResponse, VisualPrItem, VisualPrListResponse, VisualPrPreviewState,
};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::utils::{self, On401};

/// Poll cadence while the tab is open: generation runs for minutes, so a
/// 5-second refresh keeps status chips honest without hammering `gh`.
const POLL_MS: u32 = 5_000;

#[function_component(AdminVisualPrsTab)]
pub fn admin_visual_prs_tab() -> Html {
    let data = use_state(|| None::<VisualPrListResponse>);
    let expanded = use_state(|| None::<i64>);
    let confirm_merge = use_state(|| false);
    let notice = use_state(|| None::<String>);

    let reload = {
        let data = data.clone();
        move || {
            let data = data.clone();
            spawn_local(async move {
                if let Ok(d) = utils::fetch_json::<VisualPrListResponse>(
                    "/api/admin/visual-prs",
                    On401::Ignore,
                )
                .await
                {
                    data.set(Some(d));
                }
            });
        }
    };

    {
        let reload = reload.clone();
        use_effect_with((), move |_| {
            reload();
            let interval = Interval::new(POLL_MS, reload);
            move || drop(interval)
        });
    }

    let on_generate = {
        let reload = reload.clone();
        let notice = notice.clone();
        Callback::from(move |number: i64| {
            let reload = reload.clone();
            let notice = notice.clone();
            spawn_local(async move {
                let url = utils::api_url(&format!("/api/admin/visual-prs/{number}/generate"));
                match Request::post(&url).send().await {
                    Ok(resp) if resp.ok() => notice.set(None),
                    Ok(resp) => notice.set(Some(format!(
                        "Generate for #{number} failed: HTTP {}",
                        resp.status()
                    ))),
                    Err(e) => notice.set(Some(format!("Generate for #{number} failed: {e}"))),
                }
                reload();
            });
        })
    };

    let on_approve = {
        let reload = reload.clone();
        let notice = notice.clone();
        let expanded = expanded.clone();
        let confirm_merge = confirm_merge.clone();
        Callback::from(move |number: i64| {
            let reload = reload.clone();
            let notice = notice.clone();
            let expanded = expanded.clone();
            let confirm_merge = confirm_merge.clone();
            spawn_local(async move {
                let url = utils::api_url(&format!("/api/admin/visual-prs/{number}/approve"));
                match Request::post(&url).send().await {
                    Ok(resp) if resp.ok() => {
                        let msg = resp
                            .json::<VisualPrApproveResponse>()
                            .await
                            .map(|r| r.message)
                            .unwrap_or_else(|_| format!("PR #{number} approved"));
                        notice.set(Some(msg));
                        expanded.set(None);
                    }
                    Ok(resp) => {
                        let body = resp.text().await.unwrap_or_default();
                        notice.set(Some(format!("Approve #{number} failed: {body}")));
                    }
                    Err(e) => notice.set(Some(format!("Approve #{number} failed: {e}"))),
                }
                confirm_merge.set(false);
                reload();
            });
        })
    };

    let Some(list) = (*data).clone() else {
        return html! { <p class="empty-state">{ "Loading pull requests…" }</p> };
    };

    if !list.enabled {
        return html! {
            <section class="visual-prs">
                <h3>{ "Visual PR review" }</h3>
                <p class="empty-state">
                    { list.disabled_reason.unwrap_or_else(|| "Feature disabled.".to_string()) }
                </p>
            </section>
        };
    }

    let expanded_pr: Option<VisualPrItem> =
        expanded.and_then(|n| list.prs.iter().find(|p| p.number == n).cloned());

    html! {
        <section class="visual-prs">
            <h3>{ "Visual PR review" }</h3>
            <p class="section-note">
                { "Generate renders a before/after summary of the PR's actual diff in the background \
                   (a headless claude run — takes a minute or three). Click a ready preview to review \
                   and approve." }
            </p>
            if let Some(msg) = (*notice).clone() {
                <p class="visual-pr-notice">{ msg }</p>
            }
            if list.prs.is_empty() {
                <p class="empty-state">{ "No open pull requests." }</p>
            } else {
                <table class="admin-table">
                    <thead>
                        <tr>
                            <th>{ "PR" }</th>
                            <th>{ "Title" }</th>
                            <th>{ "Branch" }</th>
                            <th>{ "Preview" }</th>
                            <th class="actions"></th>
                        </tr>
                    </thead>
                    <tbody>
                        { for list.prs.iter().map(|pr| render_row(pr, &expanded, &on_generate)) }
                    </tbody>
                </table>
            }
            if let Some(pr) = expanded_pr {
                { render_preview_modal(&pr, &expanded, &confirm_merge, &on_generate, &on_approve) }
            }
        </section>
    }
}

fn render_row(
    pr: &VisualPrItem,
    expanded: &UseStateHandle<Option<i64>>,
    on_generate: &Callback<i64>,
) -> Html {
    let number = pr.number;
    let status = match pr.preview {
        VisualPrPreviewState::None => html! { <span class="visual-pr-chip none">{ "—" }</span> },
        VisualPrPreviewState::Generating => {
            html! { <span class="visual-pr-chip generating">{ "Generating…" }</span> }
        }
        VisualPrPreviewState::Ready => {
            html! { <span class="visual-pr-chip ready">{ "Ready" }</span> }
        }
        VisualPrPreviewState::Failed => html! {
            <span class="visual-pr-chip failed" title={pr.preview_error.clone().unwrap_or_default()}>
                { "Failed" }
            </span>
        },
    };

    let action = match pr.preview {
        VisualPrPreviewState::Generating => html! {},
        VisualPrPreviewState::Ready => {
            let expanded = expanded.clone();
            html! {
                <button class="header-button" onclick={Callback::from(move |_| expanded.set(Some(number)))}>
                    { "View" }
                </button>
            }
        }
        VisualPrPreviewState::None | VisualPrPreviewState::Failed => {
            let on_generate = on_generate.clone();
            let label = if pr.preview == VisualPrPreviewState::Failed {
                "Retry"
            } else {
                "Generate"
            };
            html! {
                <button class="header-button" onclick={Callback::from(move |_| on_generate.emit(number))}>
                    { label }
                </button>
            }
        }
    };

    html! {
        <tr key={pr.number.to_string()}>
            <td class="numeric">
                <a href={pr.url.clone()} target="_blank" rel="noopener noreferrer">
                    { format!("#{}", pr.number) }
                </a>
            </td>
            <td>
                { &pr.title }
                if pr.draft {
                    <span class="visual-pr-chip none">{ "draft" }</span>
                }
            </td>
            <td class="timestamp">{ &pr.head_ref }</td>
            <td>{ status }</td>
            <td class="actions">{ action }</td>
        </tr>
    }
}

fn render_preview_modal(
    pr: &VisualPrItem,
    expanded: &UseStateHandle<Option<i64>>,
    confirm_merge: &UseStateHandle<bool>,
    on_generate: &Callback<i64>,
    on_approve: &Callback<i64>,
) -> Html {
    let number = pr.number;
    let svg_url = utils::api_url(&format!("/api/admin/visual-prs/{number}/preview.svg"));

    let on_close = {
        let expanded = expanded.clone();
        let confirm_merge = confirm_merge.clone();
        Callback::from(move |_: MouseEvent| {
            expanded.set(None);
            confirm_merge.set(false);
        })
    };
    let on_regenerate = {
        let on_generate = on_generate.clone();
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| {
            on_generate.emit(number);
            expanded.set(None);
        })
    };
    let approve_button = if **confirm_merge {
        let on_approve = on_approve.clone();
        html! {
            <button class="modal-confirm" onclick={Callback::from(move |_: MouseEvent| on_approve.emit(number))}>
                { "Confirm squash-merge" }
            </button>
        }
    } else {
        let confirm_merge = confirm_merge.clone();
        html! {
            <button class="modal-confirm" onclick={Callback::from(move |_: MouseEvent| confirm_merge.set(true))}>
                { "Approve & merge" }
            </button>
        }
    };

    html! {
        <div class="modal-overlay" onclick={on_close.clone()}>
            <div
                class="modal-content visual-pr-modal"
                onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}
            >
                <h3>
                    { format!("PR #{} · {}", pr.number, pr.title) }
                </h3>
                <img class="visual-pr-preview" src={svg_url} alt={format!("Visual summary of PR #{}", pr.number)} />
                <div class="modal-buttons">
                    <button class="modal-cancel" onclick={on_close}>{ "Close" }</button>
                    <button class="modal-cancel" onclick={on_regenerate}>{ "Regenerate" }</button>
                    { approve_button }
                </div>
            </div>
        </div>
    }
}
