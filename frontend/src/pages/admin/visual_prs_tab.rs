// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Admin ▸ Visual PRs: pick a launcher host (one with an authenticated `gh`),
//! a model, and a repo; list the repo's open PRs; kick off background
//! generation of a before/after summary SVG on that host (shallow clone →
//! headless claude → cleanup); click a ready preview to review and approve
//! (squash-merge) the PR. Finished SVGs are stored durably in the portal DB.

use gloo::storage::{LocalStorage, Storage};
use gloo::timers::callback::Interval;
use gloo_net::http::Request;
use shared::api::{
    ProbeAgentsResponse, VisualPrApproveRequest, VisualPrApproveResponse, VisualPrGenerateRequest,
    VisualPrItem, VisualPrListResponse, VisualPrPreviewState,
};
use shared::{GhStatus, LauncherInfo, LAUNCHER_CAPABILITY_VISUAL_PR};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

use crate::utils::{self, On401};

/// Poll cadence while the tab is open: generation runs for minutes, so a
/// 5-second refresh keeps status chips honest without hammering `gh`.
const POLL_MS: u32 = 5_000;

/// localStorage keys for the picker state, so the tab reopens configured.
const LS_HOST: &str = "visual-pr-host";
const LS_MODEL: &str = "visual-pr-model";
const LS_REPO: &str = "visual-pr-repo";

/// Model choices passed to the headless claude run's `--model`. CLI aliases,
/// not API ids — empty string means the CLI default.
const MODELS: &[(&str, &str)] = &[
    ("", "Default model"),
    ("sonnet", "Sonnet"),
    ("opus", "Opus"),
    ("haiku", "Haiku"),
];

#[function_component(AdminVisualPrsTab)]
pub fn admin_visual_prs_tab() -> Html {
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let gh_probes = use_state(HashMap::<Uuid, GhStatus>::new);
    let host = use_state(|| LocalStorage::get::<String>(LS_HOST).unwrap_or_default());
    let model = use_state(|| LocalStorage::get::<String>(LS_MODEL).unwrap_or_default());
    let repo = use_state(|| LocalStorage::get::<String>(LS_REPO).unwrap_or_default());
    let list = use_state(|| None::<VisualPrListResponse>);
    let list_error = use_state(|| None::<String>);
    let expanded = use_state(|| None::<i64>);
    let confirm_merge = use_state(|| false);
    let notice = use_state(|| None::<String>);

    // Launchers + a gh probe per connected one, once on mount. The picker
    // marks hosts whose gh is missing/unauthenticated (or whose launcher
    // predates the visual-PR capability) as unusable.
    {
        let launchers = launchers.clone();
        let gh_probes = gh_probes.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                let Ok(list) =
                    utils::fetch_json::<Vec<LauncherInfo>>("/api/launchers", On401::Ignore).await
                else {
                    return;
                };
                for l in list.iter().filter(|l| l.connected) {
                    let gh_probes = gh_probes.clone();
                    let id = l.launcher_id;
                    spawn_local(async move {
                        let path = format!("/api/launchers/{id}/probe-agents");
                        if let Ok(resp) =
                            utils::fetch_json::<ProbeAgentsResponse>(&path, On401::Ignore).await
                        {
                            if let Some(gh) = resp.gh {
                                let mut next = (*gh_probes).clone();
                                next.insert(id, gh);
                                gh_probes.set(next);
                            }
                        }
                    });
                }
                launchers.set(list);
            });
            || ()
        });
    }

    let reload = {
        let list = list.clone();
        let list_error = list_error.clone();
        let host = host.clone();
        let repo = repo.clone();
        move || {
            let (Ok(launcher_id), repo) = (host.parse::<Uuid>(), (*repo).clone()) else {
                return;
            };
            if !repo.contains('/') {
                return;
            }
            let list = list.clone();
            let list_error = list_error.clone();
            spawn_local(async move {
                let path = format!("/api/admin/visual-prs?launcher_id={launcher_id}&repo={repo}");
                match utils::fetch_json::<VisualPrListResponse>(&path, On401::Ignore).await {
                    Ok(d) => {
                        list.set(Some(d));
                        list_error.set(None);
                    }
                    Err(e) => list_error.set(Some(e.to_string())),
                }
            });
        }
    };

    // Refetch when the host/repo selection changes, then keep polling.
    {
        let reload = reload.clone();
        use_effect_with(((*host).clone(), (*repo).clone()), move |_| {
            reload();
            let interval = Interval::new(POLL_MS, reload);
            move || drop(interval)
        });
    }

    let on_host = {
        let host = host.clone();
        Callback::from(move |e: Event| {
            let v = e.target_unchecked_into::<HtmlSelectElement>().value();
            let _ = LocalStorage::set(LS_HOST, &v);
            host.set(v);
        })
    };
    let on_model = {
        let model = model.clone();
        Callback::from(move |e: Event| {
            let v = e.target_unchecked_into::<HtmlSelectElement>().value();
            let _ = LocalStorage::set(LS_MODEL, &v);
            model.set(v);
        })
    };
    let on_repo = {
        let repo = repo.clone();
        Callback::from(move |e: Event| {
            let v = e
                .target_unchecked_into::<HtmlInputElement>()
                .value()
                .trim()
                .to_string();
            let _ = LocalStorage::set(LS_REPO, &v);
            repo.set(v);
        })
    };

    let on_generate = {
        let reload = reload.clone();
        let notice = notice.clone();
        let host = host.clone();
        let repo = repo.clone();
        let model = model.clone();
        Callback::from(move |number: i64| {
            let Ok(launcher_id) = host.parse::<Uuid>() else {
                return;
            };
            let body = VisualPrGenerateRequest {
                launcher_id,
                repo: (*repo).clone(),
                model: Some((*model).clone()).filter(|m| !m.is_empty()),
            };
            let reload = reload.clone();
            let notice = notice.clone();
            spawn_local(async move {
                let url = utils::api_url(&format!("/api/admin/visual-prs/{number}/generate"));
                match Request::post(&url).json(&body).unwrap().send().await {
                    Ok(resp) if resp.ok() => notice.set(None),
                    Ok(resp) => {
                        let text = resp.text().await.unwrap_or_default();
                        notice.set(Some(format!("Generate for #{number} failed: {text}")));
                    }
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
        let host = host.clone();
        let repo = repo.clone();
        Callback::from(move |number: i64| {
            let Ok(launcher_id) = host.parse::<Uuid>() else {
                return;
            };
            let body = VisualPrApproveRequest {
                launcher_id,
                repo: (*repo).clone(),
            };
            let reload = reload.clone();
            let notice = notice.clone();
            let expanded = expanded.clone();
            let confirm_merge = confirm_merge.clone();
            spawn_local(async move {
                let url = utils::api_url(&format!("/api/admin/visual-prs/{number}/approve"));
                match Request::post(&url).json(&body).unwrap().send().await {
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

    let expanded_pr: Option<VisualPrItem> = expanded.and_then(|n| {
        list.as_ref()
            .and_then(|l| l.prs.iter().find(|p| p.number == n).cloned())
    });

    let configured = host.parse::<Uuid>().is_ok() && repo.contains('/');

    html! {
        <section class="visual-prs">
            <h3>{ "Visual PR review" }</h3>
            <p class="section-note">
                { "Pick a computer with an authenticated GitHub CLI — generation shallow-clones \
                   the repo into a temp dir on that machine, renders a before/after summary of \
                   the PR's actual diff, stores the image here, and cleans the clone up." }
            </p>
            { picker_row(&launchers, &gh_probes, &host, &model, &repo, &on_host, &on_model, &on_repo) }
            if let Some(msg) = (*notice).clone() {
                <p class="visual-pr-notice">{ msg }</p>
            }
            if let Some(err) = (*list_error).clone() {
                <p class="visual-pr-notice">{ err }</p>
            }
            if !configured {
                <p class="empty-state">{ "Choose a host and enter a repo (owner/name) to list its open PRs." }</p>
            } else if let Some(list) = &*list {
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
            } else {
                <p class="empty-state">{ "Loading pull requests…" }</p>
            }
            if let Some(pr) = expanded_pr {
                { render_preview_modal(&pr, &repo, &expanded, &confirm_merge, &on_generate, &on_approve) }
            }
        </section>
    }
}

/// Host / model / repo pickers. A host is offered only when its launcher
/// advertises the visual-PR capability; gh state annotates the label so an
/// unauthenticated machine explains itself.
#[allow(clippy::too_many_arguments)]
fn picker_row(
    launchers: &[LauncherInfo],
    gh_probes: &HashMap<Uuid, GhStatus>,
    host: &str,
    model: &str,
    repo: &str,
    on_host: &Callback<Event>,
    on_model: &Callback<Event>,
    on_repo: &Callback<Event>,
) -> Html {
    let host_options = launchers
        .iter()
        .filter(|l| l.connected)
        .map(|l| {
            let capable = l
                .capabilities
                .iter()
                .any(|c| c == LAUNCHER_CAPABILITY_VISUAL_PR);
            let (usable, gh_label) = match gh_probes.get(&l.launcher_id) {
                _ if !capable => (false, " (launcher too old)"),
                Some(gh) if gh.authenticated => (true, " (gh ✓)"),
                Some(gh) if gh.installed => (false, " (gh not signed in)"),
                Some(_) => (false, " (gh not installed)"),
                None => (false, " (probing gh…)"),
            };
            let id = l.launcher_id.to_string();
            html! {
                <option value={id.clone()} disabled={!usable} selected={id == host}>
                    { format!("{}{}", l.hostname, gh_label) }
                </option>
            }
        })
        .collect::<Html>();

    html! {
        <div class="visual-pr-pickers">
            <label>
                { "Host" }
                <select onchange={on_host}>
                    <option value="" selected={host.is_empty()}>{ "Choose a computer" }</option>
                    { host_options }
                </select>
            </label>
            <label>
                { "Model" }
                <select onchange={on_model}>
                    { for MODELS.iter().map(|(value, label)| html! {
                        <option value={*value} selected={*value == model}>{ *label }</option>
                    }) }
                </select>
            </label>
            <label>
                { "Repo" }
                <input
                    type="text"
                    placeholder="owner/name"
                    value={repo.to_string()}
                    onchange={on_repo}
                />
            </label>
        </div>
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
            <td class="num">
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
    repo: &str,
    expanded: &UseStateHandle<Option<i64>>,
    confirm_merge: &UseStateHandle<bool>,
    on_generate: &Callback<i64>,
    on_approve: &Callback<i64>,
) -> Html {
    let number = pr.number;
    let svg_url = utils::api_url(&format!(
        "/api/admin/visual-prs/{number}/preview.svg?repo={repo}"
    ));

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
