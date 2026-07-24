//! History browser (`/history`): stats strip, filter controls, session table.

use std::collections::BTreeMap;

use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_router::prelude::*;

use shared::api::{HistorySessionSummary, HistorySessionsResponse};

use super::fetch::{fetch_json, Load};
use super::filters::{filter_and_sort, SessionFilter};
use crate::Route;

#[function_component(HistoryBrowserPage)]
pub fn history_browser_page() -> Html {
    let response = use_state(|| None as Load<HistorySessionsResponse>);
    let filter = use_state(SessionFilter::default);

    {
        let response = response.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                response.set(Some(
                    fetch_json::<HistorySessionsResponse>("/api/history/sessions").await,
                ));
            });
            || ()
        });
    }

    let body = match &*response {
        None => html! { <div class="history-loading">{ "Loading history…" }</div> },
        Some(Err(e)) => html! {
            <div class="history-error">{ format!("Could not load history: {e}") }</div>
        },
        Some(Ok(resp)) => {
            let rows = filter_and_sort(&resp.sessions, &filter);
            html! {
                <>
                    { stats_strip(&rows, resp.is_admin) }
                    { filter_controls(&resp.sessions, resp.is_admin, &filter) }
                    { session_table(&rows, resp.is_admin) }
                </>
            }
        }
    };

    html! {
        <div class="history-root">
            <nav class="history-nav">
                <Link<Route> to={Route::Dashboard}>{ "← Dashboard" }</Link<Route>>
            </nav>
            <header class="history-header">
                <h1>{ "Session History" }</h1>
            </header>
            { body }
        </div>
    }
}

/// Totals over the *filtered* rows, so the tiles always agree with the table.
/// Admins additionally get a per-user cost breakdown.
fn stats_strip(rows: &[HistorySessionSummary], is_admin: bool) -> Html {
    if rows.is_empty() {
        return Html::default();
    }
    let total_cost: f64 = rows.iter().map(|s| s.total_cost_usd).sum();
    let total_messages: i64 = rows.iter().map(|s| s.message_count).sum();

    let user_tiles = if is_admin {
        let mut by_user: BTreeMap<String, (String, i64, f64)> = BTreeMap::new();
        for s in rows {
            let entry = by_user
                .entry(s.user_id.clone())
                .or_insert_with(|| (owner_label(s), 0, 0.0));
            entry.1 += 1;
            entry.2 += s.total_cost_usd;
        }
        let mut users: Vec<_> = by_user.into_values().collect();
        users.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        users
            .into_iter()
            .map(|(label, sessions, cost)| {
                html! {
                    <div class="rollup-tile rollup-user">
                        <span class="rollup-value">{ format!("${cost:.2}") }</span>
                        <span class="rollup-label">{ format!("{label} ({sessions} sess)") }</span>
                    </div>
                }
            })
            .collect::<Html>()
    } else {
        Html::default()
    };

    html! {
        <div class="history-rollup">
            <div class="rollup-tile">
                <span class="rollup-value">{ rows.len() }</span>
                <span class="rollup-label">{ "sessions" }</span>
            </div>
            <div class="rollup-tile">
                <span class="rollup-value">{ total_messages }</span>
                <span class="rollup-label">{ "messages" }</span>
            </div>
            <div class="rollup-tile">
                <span class="rollup-value">{ format!("${total_cost:.2}") }</span>
                <span class="rollup-label">{ "total spend" }</span>
            </div>
            { user_tiles }
        </div>
    }
}

fn owner_label(s: &HistorySessionSummary) -> String {
    s.owner_name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            if s.owner_email.is_empty() {
                s.user_id.clone()
            } else {
                s.owner_email.clone()
            }
        })
}

fn filter_controls(
    all_sessions: &[HistorySessionSummary],
    is_admin: bool,
    filter: &UseStateHandle<SessionFilter>,
) -> Html {
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

    // Admin-only user dropdown, derived from the unfiltered visible rows so
    // selecting a user doesn't shrink the option list.
    let user_filter = if is_admin {
        let mut owners: BTreeMap<String, String> = BTreeMap::new();
        for s in all_sessions {
            owners
                .entry(s.user_id.clone())
                .or_insert_with(|| owner_label(s));
        }
        let options = owners
            .into_iter()
            .map(|(id, label)| html! { <option value={id}>{ label }</option> })
            .collect::<Html>();
        html! {
            <label>
                { "User" }
                <select onchange={on_user}>
                    <option value="">{ "All users" }</option>
                    { options }
                </select>
            </label>
        }
    } else {
        Html::default()
    };

    html! {
        <div class="history-filters">
            { user_filter }
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
            <label class="history-filter-search">
                { "Name" }
                <input type="text" placeholder="substring…" oninput={on_query} />
            </label>
        </div>
    }
}

fn session_table(rows: &[HistorySessionSummary], is_admin: bool) -> Html {
    if rows.is_empty() {
        return html! {
            <div class="history-empty">{ "No archived sessions match these filters." }</div>
        };
    }
    html! {
        <table class="history-table">
            <thead>
                <tr>
                    <th>{ "Name" }</th>
                    <th>{ "Agent" }</th>
                    { if is_admin { html! { <th>{ "User" }</th> } } else { Html::default() } }
                    <th>{ "Host" }</th>
                    <th>{ "Created" }</th>
                    <th>{ "Last activity" }</th>
                    <th class="num">{ "Msgs" }</th>
                    <th class="num">{ "Cost" }</th>
                    <th>{ "Models" }</th>
                </tr>
            </thead>
            <tbody>
                { for rows.iter().map(|s| html! { <SessionRow session={s.clone()} {is_admin} /> }) }
            </tbody>
        </table>
    }
}

#[derive(Properties, PartialEq)]
struct SessionRowProps {
    session: HistorySessionSummary,
    is_admin: bool,
}

#[function_component(SessionRow)]
fn session_row(props: &SessionRowProps) -> Html {
    let s = &props.session;
    let route = Route::HistorySession {
        user: s.user_id.clone(),
        session: s.session_id.clone(),
    };
    let navigator = use_navigator();
    // Whole-row click navigates, but stays out of the way of real anchor
    // behavior: a modified click (new tab) or a click the inner `Link` already
    // handled (`default_prevented`) is left alone.
    let onclick = {
        let route = route.clone();
        Callback::from(move |e: MouseEvent| {
            if e.default_prevented() || e.ctrl_key() || e.meta_key() || e.shift_key() {
                return;
            }
            if let Some(navigator) = &navigator {
                navigator.push(&route);
            }
        })
    };
    let name = if s.session_name.is_empty() {
        s.session_id.clone()
    } else {
        s.session_name.clone()
    };
    html! {
        <tr class="history-row" {onclick}>
            <td class="cell-name">
                <Link<Route> to={route}>{ name }</Link<Route>>
            </td>
            <td>{ &s.agent_type }</td>
            {
                if props.is_admin {
                    html! { <td>{ owner_label(s) }</td> }
                } else {
                    Html::default()
                }
            }
            <td>{ &s.hostname }</td>
            <td class="cell-date">{ short_date(&s.created_at) }</td>
            <td class="cell-date">{ short_date(&s.last_activity) }</td>
            <td class="num">{ s.message_count }</td>
            <td class="num">{ format!("${:.2}", s.total_cost_usd) }</td>
            <td class="cell-models">{ s.models.join(", ") }</td>
        </tr>
    }
}

/// Trim an ISO timestamp to `YYYY-MM-DD HH:MM` for compact table display.
fn short_date(iso: &str) -> String {
    let trimmed = iso.replace('T', " ");
    match trimmed.char_indices().nth(16) {
        Some((idx, _)) => trimmed[..idx].to_string(),
        None => trimmed,
    }
}
