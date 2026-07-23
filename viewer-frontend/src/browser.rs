//! Session browser (`#/`): rollup strip, filter controls, session table.

use std::collections::HashMap;

use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

use crate::api::{self, FetchError, RollupRow, SessionSummary, UserSummary};
use crate::filters::{filter_and_sort, SessionFilter};

/// `None` = in flight; `Some(Ok/Err)` = settled.
type Load<T> = Option<Result<T, FetchError>>;

#[function_component(SessionBrowser)]
pub fn session_browser() -> Html {
    let users = use_state(|| None as Load<Vec<UserSummary>>);
    let sessions = use_state(|| None as Load<Vec<SessionSummary>>);
    let rollup = use_state(|| None as Load<Vec<RollupRow>>);
    let filter = use_state(SessionFilter::default);

    {
        let users = users.clone();
        let sessions = sessions.clone();
        let rollup = rollup.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                users.set(Some(
                    api::fetch_json::<Vec<UserSummary>>("/api/users").await,
                ));
            });
            spawn_local(async move {
                sessions.set(Some(
                    api::fetch_json::<Vec<SessionSummary>>("/api/sessions").await,
                ));
            });
            spawn_local(async move {
                rollup.set(Some(
                    api::fetch_json::<Vec<RollupRow>>("/api/rollup?group_by=user").await,
                ));
            });
            || ()
        });
    }

    let user_labels: HashMap<String, String> = match &*users {
        Some(Ok(list)) => list
            .iter()
            .map(|u| (u.user_id.clone(), u.label()))
            .collect(),
        _ => HashMap::new(),
    };

    html! {
        <div class="viewer-root">
            <header class="viewer-header">
                <h1>{ "Agent Portal — Session Archive" }</h1>
            </header>
            { rollup_strip(&rollup) }
            { filter_controls(&users, &filter) }
            { session_table(&sessions, &filter, &user_labels) }
        </div>
    }
}

fn rollup_strip(rollup: &Load<Vec<RollupRow>>) -> Html {
    match rollup {
        None => html! { <div class="viewer-rollup viewer-loading">{ "Loading totals…" }</div> },
        Some(Err(e)) => html! {
            <div class="viewer-rollup viewer-error">{ format!("Totals unavailable: {e}") }</div>
        },
        Some(Ok(rows)) if rows.is_empty() => Html::default(),
        Some(Ok(rows)) => {
            let total_sessions: i64 = rows.iter().map(|r| r.session_count).sum();
            let total_cost: f64 = rows.iter().map(|r| r.total_cost_usd).sum();
            html! {
                <div class="viewer-rollup">
                    <div class="rollup-tile">
                        <span class="rollup-value">{ total_sessions }</span>
                        <span class="rollup-label">{ "sessions" }</span>
                    </div>
                    <div class="rollup-tile">
                        <span class="rollup-value">{ format!("${total_cost:.2}") }</span>
                        <span class="rollup-label">{ "total spend" }</span>
                    </div>
                    { for rows.iter().map(|r| html! {
                        <div class="rollup-tile rollup-user">
                            <span class="rollup-value">{ format!("${:.2}", r.total_cost_usd) }</span>
                            <span class="rollup-label">
                                { format!("{} ({} sess)", r.display_label(), r.session_count) }
                            </span>
                        </div>
                    }) }
                </div>
            }
        }
    }
}

fn filter_controls(users: &Load<Vec<UserSummary>>, filter: &UseStateHandle<SessionFilter>) -> Html {
    let on_user = {
        let filter = filter.clone();
        Callback::from(move |e: Event| {
            let value = e.target_unchecked_into::<HtmlSelectElement>().value();
            let mut next = (*filter).clone();
            next.user_id = (!value.is_empty()).then_some(value);
            filter.set(next);
        })
    };
    let on_agent = {
        let filter = filter.clone();
        Callback::from(move |e: Event| {
            let value = e.target_unchecked_into::<HtmlSelectElement>().value();
            let mut next = (*filter).clone();
            next.agent_type = (!value.is_empty()).then_some(value);
            filter.set(next);
        })
    };
    let on_from = {
        let filter = filter.clone();
        Callback::from(move |e: InputEvent| {
            let value = e.target_unchecked_into::<HtmlInputElement>().value();
            let mut next = (*filter).clone();
            next.from = (!value.is_empty()).then_some(value);
            filter.set(next);
        })
    };
    let on_to = {
        let filter = filter.clone();
        Callback::from(move |e: InputEvent| {
            let value = e.target_unchecked_into::<HtmlInputElement>().value();
            let mut next = (*filter).clone();
            next.to = (!value.is_empty()).then_some(value);
            filter.set(next);
        })
    };
    // Uncontrolled (no `value` binding) so the node is never recreated on the
    // parent re-render each keystroke triggers — focus and caret stay put.
    let on_query = {
        let filter = filter.clone();
        Callback::from(move |e: InputEvent| {
            let value = e.target_unchecked_into::<HtmlInputElement>().value();
            let mut next = (*filter).clone();
            next.query = (!value.is_empty()).then_some(value);
            filter.set(next);
        })
    };

    let user_options = match users {
        Some(Ok(list)) => list
            .iter()
            .map(|u| html! { <option value={u.user_id.clone()}>{ u.label() }</option> })
            .collect::<Html>(),
        _ => Html::default(),
    };

    html! {
        <div class="viewer-filters">
            <label>
                { "User" }
                <select onchange={on_user}>
                    <option value="">{ "All users" }</option>
                    { user_options }
                </select>
            </label>
            <label>
                { "Agent" }
                <select onchange={on_agent}>
                    <option value="">{ "All agents" }</option>
                    <option value="claude">{ "Claude" }</option>
                    <option value="codex">{ "Codex" }</option>
                </select>
            </label>
            <label>
                { "From" }
                <input type="date" oninput={on_from} />
            </label>
            <label>
                { "To" }
                <input type="date" oninput={on_to} />
            </label>
            <label class="viewer-filter-search">
                { "Name" }
                <input type="text" placeholder="substring…" oninput={on_query} />
            </label>
        </div>
    }
}

fn session_table(
    sessions: &Load<Vec<SessionSummary>>,
    filter: &SessionFilter,
    user_labels: &HashMap<String, String>,
) -> Html {
    match sessions {
        None => html! { <div class="viewer-loading">{ "Loading sessions…" }</div> },
        Some(Err(e)) => html! {
            <div class="viewer-error">{ format!("Could not load sessions: {e}") }</div>
        },
        Some(Ok(list)) => {
            let rows = filter_and_sort(list, filter);
            if rows.is_empty() {
                return html! {
                    <div class="viewer-empty">{ "No sessions match these filters." }</div>
                };
            }
            html! {
                <table class="viewer-table">
                    <thead>
                        <tr>
                            <th>{ "Name" }</th>
                            <th>{ "Agent" }</th>
                            <th>{ "User" }</th>
                            <th>{ "Host" }</th>
                            <th>{ "Created" }</th>
                            <th>{ "Last activity" }</th>
                            <th class="num">{ "Msgs" }</th>
                            <th class="num">{ "Cost" }</th>
                            <th>{ "Models" }</th>
                        </tr>
                    </thead>
                    <tbody>
                        { for rows.iter().map(|s| session_row(s, user_labels)) }
                    </tbody>
                </table>
            }
        }
    }
}

fn session_row(s: &SessionSummary, user_labels: &HashMap<String, String>) -> Html {
    let href = format!("#/session/{}/{}", s.user_id, s.session_id);
    let user_label = user_labels
        .get(&s.user_id)
        .cloned()
        .unwrap_or_else(|| s.user_id.clone());
    let name = if s.session_name.is_empty() {
        s.session_id.clone()
    } else {
        s.session_name.clone()
    };
    let models = s.models.join(", ");
    html! {
        <tr class="viewer-row" onclick={navigate(&href)}>
            <td class="cell-name"><a href={href.clone()}>{ name }</a></td>
            <td>{ &s.agent_type }</td>
            <td>{ user_label }</td>
            <td>{ &s.hostname }</td>
            <td class="cell-date">{ short_date(&s.created_at) }</td>
            <td class="cell-date">{ short_date(&s.last_activity) }</td>
            <td class="num">{ s.message_count }</td>
            <td class="num">{ format!("${:.2}", s.total_cost_usd) }</td>
            <td class="cell-models">{ models }</td>
        </tr>
    }
}

/// Navigate on row click. Anchor already handles keyboard/middle-click; this
/// makes the whole row clickable.
fn navigate(href: &str) -> Callback<MouseEvent> {
    let href = href.to_string();
    Callback::from(move |_| {
        if let Some(win) = web_sys::window() {
            let _ = win.location().set_hash(href.trim_start_matches('#'));
        }
    })
}

/// Trim an ISO timestamp to `YYYY-MM-DD HH:MM` for compact table display.
fn short_date(iso: &str) -> String {
    let trimmed = iso.replace('T', " ");
    match trimmed.char_indices().nth(16) {
        Some((idx, _)) => trimmed[..idx].to_string(),
        None => trimmed,
    }
}
