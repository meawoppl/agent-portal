//! History browser (`/history`): stats strip, filter controls, session table.

use gloo::timers::callback::Timeout;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_router::prelude::*;

use shared::api::{HistorySessionSummary, HistorySessionsResponse, DEFAULT_HISTORY_PAGE_SIZE};

use super::fetch::{fetch_json, Load};
use super::filters::SessionFilter;
use crate::Route;

/// Rows requested per page. Must not exceed `MAX_HISTORY_PAGE_SIZE`, which the
/// backend clamps to.
const PAGE_SIZE: usize = DEFAULT_HISTORY_PAGE_SIZE;

/// Delay before a filter/page change actually hits the network. Typing in the
/// name box mutates the filter on every keystroke, and each request re-filters
/// the whole archive server-side; without this, a ten-character search is ten
/// requests. Short enough to feel immediate on a page-turn click.
const FETCH_DEBOUNCE_MS: u32 = 250;

/// Embedding hooks. Both default to `None`, which keeps the standalone
/// `/history` route behaving exactly as before; the dashboard overlay supplies
/// them so opening history never unmounts the dashboard.
#[derive(Properties, PartialEq, Default)]
pub struct HistoryBrowserProps {
    #[prop_or_default]
    pub on_close: Option<Callback<()>>,
    /// Intercepts a row click as `(user_id, session_id)` instead of navigating.
    #[prop_or_default]
    pub on_open_session: Option<Callback<(String, String)>>,
}

#[function_component(HistoryBrowserPage)]
pub fn history_browser_page(props: &HistoryBrowserProps) -> Html {
    let response = use_state(|| None as Load<HistorySessionsResponse>);
    let filter = use_state(SessionFilter::default);
    let page = use_state(|| 0usize);

    // One effect, keyed on both inputs, is the whole data path. Filter changes
    // reset the page inside the control callbacks rather than in a second
    // effect — an effect would observe the new filter with the old page, fetch,
    // then reset the page and fetch again, doubling every request.
    {
        let response = response.clone();
        use_effect_with(((*filter).clone(), *page), move |(filter, page)| {
            let query = filter.to_query(page * PAGE_SIZE, PAGE_SIZE);
            let timeout = Timeout::new(FETCH_DEBOUNCE_MS, move || {
                spawn_local(async move {
                    let url = format!("/api/history/sessions?{query}");
                    // Deliberately not cleared to `None` first: a refetch keeps
                    // the current page on screen instead of flashing the loading
                    // state on every keystroke.
                    response.set(Some(fetch_json::<HistorySessionsResponse>(&url).await));
                });
            });
            // Dropping the timeout cancels it, so a superseded keystroke never
            // reaches the network.
            move || drop(timeout)
        });
    }

    // Filter edits reset to page 0 together with the filter itself. Without it,
    // narrowing while on a late page requests an offset past the end of the new
    // result set and the table comes back empty — reading as "no matches" for a
    // filter that in fact matched.
    let on_filter = {
        let filter = filter.clone();
        let page = page.clone();
        Callback::from(move |next: SessionFilter| {
            filter.set(next);
            page.set(0);
        })
    };

    let body = match &*response {
        None => html! { <div class="history-loading">{ "Loading history…" }</div> },
        Some(Err(e)) => html! {
            <div class="history-error">{ format!("Could not load history: {e}") }</div>
        },
        Some(Ok(resp)) => {
            let window = PageWindow::resolve(resp.total as usize, *page, PAGE_SIZE);
            html! {
                <>
                    // Totals and the owner list describe the whole filtered set,
                    // computed server-side — deriving them from `resp.sessions`
                    // would silently describe this page instead.
                    { stats_strip(resp, &filter) }
                    { filter_controls(resp, &filter, &on_filter) }
                    { session_table(&resp.sessions, resp.is_admin, &props.on_open_session) }
                    { pagination_controls(&window, resp.sessions.len(), &page) }
                </>
            }
        }
    };

    html! {
        <div class="history-root">
            <nav class="history-nav">
                if let Some(on_close) = props.on_close.clone() {
                    <button class="link-button" onclick={Callback::from(move |_| on_close.emit(()))}>
                        { "← Dashboard" }
                    </button>
                } else {
                    <Link<Route> to={Route::Dashboard}>{ "← Dashboard" }</Link<Route>>
                }
            </nav>
            <header class="history-header">
                <h1>{ "Session History" }</h1>
            </header>
            { body }
        </div>
    }
}

/// Totals across the whole filtered set (not this page), as computed by the
/// backend. Admins additionally get a per-user cost breakdown.
fn stats_strip(resp: &HistorySessionsResponse, filter: &UseStateHandle<SessionFilter>) -> Html {
    if resp.total == 0 {
        return Html::default();
    }

    // `owners` covers every filter *except* `user`, so when a user is selected
    // narrow it here — that keeps the tiles agreeing with the table while the
    // dropdown above still lists everyone.
    let selected = filter.user_id.as_deref().filter(|s| !s.trim().is_empty());
    let user_tiles = if resp.is_admin {
        resp.owners
            .iter()
            .filter(|o| selected.is_none_or(|want| o.user_id == want))
            .map(|o| {
                html! {
                    <div class="rollup-tile rollup-user">
                        <span class="rollup-value">{ format!("${:.2}", o.total_cost_usd) }</span>
                        <span class="rollup-label">
                            { format!("{} ({} sess)", o.label, o.session_count) }
                        </span>
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
                <span class="rollup-value">{ resp.totals.session_count }</span>
                <span class="rollup-label">{ "sessions" }</span>
            </div>
            <div class="rollup-tile">
                <span class="rollup-value">{ resp.totals.message_count }</span>
                <span class="rollup-label">{ "messages" }</span>
            </div>
            <div class="rollup-tile">
                <span class="rollup-value">
                    { format!("${:.2}", resp.totals.total_cost_usd) }
                </span>
                <span class="rollup-label">{ "total spend" }</span>
            </div>
            { user_tiles }
        </div>
    }
}

/// `on_change` carries the whole next filter rather than a per-field callback so
/// the parent can reset the page in the same update — see `on_filter`.
fn filter_controls(
    resp: &HistorySessionsResponse,
    filter: &UseStateHandle<SessionFilter>,
    on_change: &Callback<SessionFilter>,
) -> Html {
    /// Build a handler that edits one field of the current filter and emits it.
    macro_rules! field_handler {
        ($event:ty, $target:ty, $field:ident) => {{
            let filter = filter.clone();
            let on_change = on_change.clone();
            Callback::from(move |e: $event| {
                let value = e.target_unchecked_into::<$target>().value();
                let mut next = (*filter).clone();
                next.$field = (!value.is_empty()).then_some(value);
                on_change.emit(next);
            })
        }};
    }

    let on_user = field_handler!(Event, HtmlSelectElement, user_id);
    let on_agent = field_handler!(Event, HtmlSelectElement, agent_type);
    let on_from = field_handler!(InputEvent, HtmlInputElement, from);
    let on_to = field_handler!(InputEvent, HtmlInputElement, to);
    // Uncontrolled (no `value` binding) so the node is never recreated on the
    // parent re-render each keystroke triggers — focus and caret stay put.
    let on_query = field_handler!(InputEvent, HtmlInputElement, query);

    // Admin-only user dropdown. `owners` is computed server-side over every
    // filter except `user`, so selecting a user doesn't shrink the option list.
    let user_filter = if resp.is_admin {
        let options = resp
            .owners
            .iter()
            .map(|o| {
                html! { <option value={o.user_id.clone()}>{ o.label.clone() }</option> }
            })
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

/// A resolved page: which page is actually being shown and the row range it
/// covers, always a valid slice of `total` rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PageWindow {
    page: usize,
    page_count: usize,
    start: usize,
    end: usize,
}

impl PageWindow {
    /// Clamp `requested` against the current row count.
    ///
    /// Clamping is not belt-and-braces over the reset effect — it is required.
    /// The effect that returns to page 0 on a filter change runs *after* the
    /// render that observes the new filter, so for one frame a stale page index
    /// is live against a shorter row set. Without clamping that frame panics on
    /// an out-of-range slice.
    fn resolve(total: usize, requested: usize, page_size: usize) -> Self {
        debug_assert!(page_size > 0, "page size must be non-zero");
        let page_count = total.div_ceil(page_size).max(1);
        let page = requested.min(page_count - 1);
        let start = (page * page_size).min(total);
        let end = (start + page_size).min(total);
        Self {
            page,
            page_count,
            start,
            end,
        }
    }

    fn shown(&self) -> usize {
        self.end - self.start
    }
}

/// Prev/next controls plus a "showing X–Y of N" summary.
///
/// This paginates the *rendered table* only: `/api/history/sessions` returns
/// every visible session in a single response and the whole set is filtered and
/// totalled client-side. That keeps the stats strip and the admin user dropdown
/// honest — both need the full result set — at the cost of the response staying
/// O(all sessions). Making the fetch itself paged means moving filtering to the
/// server (the endpoint already accepts `user`/`agent`/`q`/`from`/`to`) *and*
/// returning aggregates alongside the page, or the totals would silently start
/// describing one page instead of the whole filter.
///
/// Hidden entirely for a single page, so the common case gains no chrome.
fn pagination_controls(window: &PageWindow, total: usize, page: &UseStateHandle<usize>) -> Html {
    if window.page_count <= 1 {
        return Html::default();
    }
    let current = window.page;
    let on_prev = {
        let page = page.clone();
        Callback::from(move |_: MouseEvent| page.set(current.saturating_sub(1)))
    };
    let on_next = {
        let page = page.clone();
        let last = window.page_count - 1;
        Callback::from(move |_: MouseEvent| page.set((current + 1).min(last)))
    };
    html! {
        <nav class="history-pagination" aria-label="History pages">
            <button
                class="history-page-button"
                onclick={on_prev}
                disabled={current == 0}
                aria-label="Previous page"
            >
                { "← Prev" }
            </button>
            <span class="history-page-status">
                { format!(
                    "{}–{} of {total}  ·  page {} of {}",
                    window.start + 1,
                    window.start + window.shown(),
                    current + 1,
                    window.page_count,
                ) }
            </span>
            <button
                class="history-page-button"
                onclick={on_next}
                disabled={current + 1 >= window.page_count}
                aria-label="Next page"
            >
                { "Next →" }
            </button>
        </nav>
    }
}

fn session_table(
    rows: &[HistorySessionSummary],
    is_admin: bool,
    on_open_session: &Option<Callback<(String, String)>>,
) -> Html {
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
                { for rows.iter().map(|s| html! { <SessionRow session={s.clone()} {is_admin} on_open_session={on_open_session.clone()} /> }) }
            </tbody>
        </table>
    }
}

#[derive(Properties, PartialEq)]
struct SessionRowProps {
    session: HistorySessionSummary,
    is_admin: bool,
    #[prop_or_default]
    on_open_session: Option<Callback<(String, String)>>,
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
        let embed = props.on_open_session.clone();
        let ids = (s.user_id.clone(), s.session_id.clone());
        Callback::from(move |e: MouseEvent| {
            if e.default_prevented() || e.ctrl_key() || e.meta_key() || e.shift_key() {
                return;
            }
            // Embedded: open in place. Standalone: navigate as before. A
            // modified click still falls through to the anchor either way, so
            // cmd-click keeps opening a real tab.
            if let Some(embed) = &embed {
                e.prevent_default();
                embed.emit(ids.clone());
            } else if let Some(navigator) = &navigator {
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

/// Display name → email → raw id, for the admin User column. The rollup tiles
/// use the label the backend resolved; this is the same rule applied to a row.
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

/// Trim an ISO timestamp to `YYYY-MM-DD HH:MM` for compact table display.
fn short_date(iso: &str) -> String {
    let trimmed = iso.replace('T', " ");
    match trimmed.char_indices().nth(16) {
        Some((idx, _)) => trimmed[..idx].to_string(),
        None => trimmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_page_of_an_exact_multiple() {
        let w = PageWindow::resolve(100, 0, 50);
        assert_eq!((w.page, w.page_count, w.start, w.end), (0, 2, 0, 50));
    }

    #[test]
    fn last_page_is_short_when_rows_do_not_divide_evenly() {
        let w = PageWindow::resolve(120, 2, 50);
        assert_eq!((w.page, w.page_count, w.start, w.end), (2, 3, 100, 120));
        assert_eq!(w.shown(), 20);
    }

    #[test]
    fn a_stale_page_index_clamps_instead_of_slicing_out_of_range() {
        // The frame after a filter narrows 500 rows to 3, before the reset
        // effect runs: page 9 is still live and must not produce 450..460.
        let w = PageWindow::resolve(3, 9, 50);
        assert_eq!((w.page, w.page_count, w.start, w.end), (0, 1, 0, 3));
    }

    #[test]
    fn no_rows_still_yields_one_empty_page() {
        // page_count is floored at 1 so `page_count - 1` never underflows.
        let w = PageWindow::resolve(0, 0, 50);
        assert_eq!((w.page, w.page_count, w.start, w.end), (0, 1, 0, 0));
        assert_eq!(w.shown(), 0);
    }

    #[test]
    fn a_single_short_page_reports_one_page_so_controls_stay_hidden() {
        let w = PageWindow::resolve(7, 0, 50);
        assert_eq!(w.page_count, 1);
        assert_eq!(w.shown(), 7);
    }

    #[test]
    fn every_window_is_a_valid_slice_across_a_range_of_sizes() {
        for total in 0..200usize {
            for requested in [0usize, 1, 3, 17, 999] {
                let w = PageWindow::resolve(total, requested, 50);
                assert!(w.start <= w.end, "start past end at total={total}");
                assert!(w.end <= total, "end past total at total={total}");
                assert!(w.page < w.page_count, "page out of range at total={total}");
            }
        }
    }
}
