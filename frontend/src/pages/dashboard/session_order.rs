//! Deterministic session display ordering + focus resolution (issue #1094).
//!
//! Why this exists: the dashboard polls `/api/sessions` every few seconds, and
//! the backend orders rows by `last_activity DESC` — an activity-sensitive
//! order that reshuffles between polls. The rail previously sorted only on
//! folder name then hostname, so multiple agents in the *same* repo/worktree
//! family (identical folder + host) compared **equal**, and Rust's `sort_by`
//! gives no order guarantee for equal keys. Combined with focus being tracked
//! by array *index*, a reshuffled poll could slide the focused index onto a
//! different session — the "focus bounce" users saw with several same-repo
//! agents running.
//!
//! Fix: a **total** comparator whose final tie-breaker is the unique session
//! `id`, so the displayed order is a pure function of the *set* of sessions
//! (independent of the source/poll order), plus focus tracked by `id` and
//! resolved to an index only at render time via [`resolve_focus_index`].

use std::cmp::Ordering;
use std::collections::HashSet;

use shared::SessionInfo;
use uuid::Uuid;

/// Total, deterministic display-order key for a session.
///
/// Lexicographic tuple, coarsest discriminant first:
/// 1. top-level folder name (lowercased) — groups sessions by repo
/// 2. agent type — orders the agents working that repo (e.g. claude, codex)
/// 3. `id` — unique final tie-breaker, so no two sessions ever compare equal
///
/// Deliberately **does not** key on `git_branch` (or working_directory /
/// hostname / timestamps). Branch is derived by best-effort detection that can
/// flap between polls (and is wrong under worktrees — see #1067); keying on it
/// made pills reorder when the branch reading changed. Folder + agent + id is
/// the stable identity that doesn't move when branch detection wobbles.
///
/// Because the key ends in the unique `id`, the order is *total*: the same set
/// of sessions always sorts identically no matter what order the poll returned
/// them in, and never depends on branch state.
fn display_sort_key(s: &SessionInfo) -> (String, String, Uuid) {
    (
        crate::utils::extract_folder(&s.working_directory).to_lowercase(),
        s.agent_type.as_str().to_string(),
        s.id,
    )
}

/// Total ordering comparator for the session rail. See [`display_sort_key`].
pub(super) fn session_display_cmp(a: &SessionInfo, b: &SessionInfo) -> Ordering {
    display_sort_key(a).cmp(&display_sort_key(b))
}

/// Label shown for the "no hostname" bucket in the grouped rail.
pub(super) const UNKNOWN_HOST_LABEL: &str = "unknown host";

/// Group-membership label for a session's host section. Whitespace-only /
/// empty hostnames collapse into a single [`UNKNOWN_HOST_LABEL`] bucket;
/// otherwise the trimmed hostname (original case preserved for display).
pub(super) fn host_group_label(s: &SessionInfo) -> String {
    let host = s.hostname.trim();
    if host.is_empty() {
        UNKNOWN_HOST_LABEL.to_string()
    } else {
        host.to_string()
    }
}

/// Sort key that places a session into its host *section*. Named hosts sort
/// alphabetically (case-insensitively); the empty/unknown-host bucket sorts
/// last via the leading `is_unknown` flag. This is intentionally coarser than
/// [`display_sort_key`] — it only decides which section a session lands in, not
/// its order *within* the section.
fn host_group_key(s: &SessionInfo) -> (bool, String) {
    let host = s.hostname.trim();
    (host.is_empty(), host.to_lowercase())
}

/// Grouped (by-host) display comparator used when the "group rail by host"
/// preference is on. Keys the host *section* first, then falls back to the
/// standard total order [`session_display_cmp`] within each section. This means:
///
/// - host sections are alphabetical, with the empty/unknown-host bucket last;
/// - sessions **within** a section keep the exact relative order they'd have
///   ungrouped (so activity-driven polls don't reshuffle within a group);
/// - the order stays *total* (the inner key ends in the unique `id`), so — like
///   [`session_display_cmp`] — the displayed sequence is a pure function of the
///   session set. That totality is what lets nav-mode numbering, `j`/`k`
///   traversal, and [`resolve_focus_index`] all consume the same vector and
///   agree with the visible top-to-bottom order.
pub(super) fn session_display_cmp_grouped(a: &SessionInfo, b: &SessionInfo) -> Ordering {
    host_group_key(a)
        .cmp(&host_group_key(b))
        .then_with(|| session_display_cmp(a, b))
}

/// Resolve the focused session's index in the *currently displayed* order.
///
/// Focus is stored by `session_id` (the source of truth), so a reorder or a
/// refreshed poll never changes which session is focused. This maps that id
/// back to a display index for the rail / keyboard nav.
///
/// `previous_index` is the index this function returned on the prior render. It
/// is used only for the *transient-miss* case below.
///
/// Resolution order:
/// 1. Focused id present in the list → its current index (the normal path).
/// 2. Focused id set but **absent** from this snapshot → hold `previous_index`
///    (clamped) rather than snapping to the first session. This is the #1368
///    fix: when focus follows a just-launched session (`FocusNewlyLaunched`),
///    a racing/stale `/api/sessions` poll — one issued *before* the session
///    existed but landing *after* the WS-driven refresh that added it — can
///    momentarily deliver a list without that session. Falling back to the
///    first entry there is exactly the "creating a session steals focus to the
///    first session" bug. Holding the previous position is safe because the
///    display order is a *total* function of the session set
///    (`session_display_cmp`): while the set is unchanged the same index maps
///    to the same session, and once the focused session reappears we resolve it
///    by id again.
/// 3. Nothing focused yet (`focused_id` is `None`, e.g. initial load) → the
///    first non-hidden session (then `0`).
pub(super) fn resolve_focus_index(
    sessions: &[SessionInfo],
    focused_id: Option<Uuid>,
    hidden: &HashSet<Uuid>,
    previous_index: usize,
) -> usize {
    if let Some(id) = focused_id {
        if let Some(idx) = sessions.iter().position(|s| s.id == id) {
            return idx;
        }
        // Focused id set but not in this snapshot: transient gap, not a real
        // removal — hold the last resolved position instead of jumping to the
        // first session (#1368).
        if !sessions.is_empty() {
            return previous_index.min(sessions.len() - 1);
        }
    }
    sessions
        .iter()
        .position(|s| !hidden.contains(&s.id))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{AgentType, SessionInfo, SessionRole, SessionStatus};

    fn session(id: Uuid, dir: &str, host: &str, branch: Option<&str>) -> SessionInfo {
        SessionInfo {
            id,
            user_id: Uuid::nil(),
            session_name: String::new(),
            session_key: String::new(),
            working_directory: dir.to_string(),
            status: SessionStatus::Active,
            last_activity: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            git_branch: branch.map(|b| b.to_string()),
            my_role: SessionRole::Owner,
            hostname: host.to_string(),
            launcher_id: None,
            launcher_version: None,
            pr_url: None,
            repo_url: None,
            open_prs: Vec::new(),
            agent_type: AgentType::Claude,
            client_version: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: Vec::new(),
            last_model: None,
        }
    }

    /// The unique-id tie-breaker makes ordering independent of input order:
    /// same folder + same host + different ids must sort identically no matter
    /// how the poll happened to return them.
    #[test]
    fn same_folder_same_host_orders_stably_regardless_of_input_order() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let mk = || {
            vec![
                session(a, "/home/me/repo", "host", None),
                session(b, "/home/me/repo", "host", None),
                session(c, "/home/me/repo", "host", None),
            ]
        };

        let mut forward = mk();
        forward.sort_by(session_display_cmp);

        let mut reversed = mk();
        reversed.reverse();
        reversed.sort_by(session_display_cmp);

        let ids = |v: &[SessionInfo]| v.iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids(&forward), ids(&reversed));
        // And it's the id order, since every other key is equal.
        assert_eq!(ids(&forward), vec![a, b, c]);
    }

    /// Two worktrees of the same repo (same leaf folder, different full path +
    /// branch) get a stable, distinct ordering.
    #[test]
    fn two_worktrees_same_repo_order_stably() {
        let a = Uuid::from_u128(10);
        let b = Uuid::from_u128(20);
        let mk = || {
            vec![
                session(b, "/home/me/repo-wt2", "host", Some("feature")),
                session(a, "/home/me/repo-wt1", "host", Some("main")),
            ]
        };
        let mut first = mk();
        first.sort_by(session_display_cmp);
        let mut second = mk();
        second.reverse();
        second.sort_by(session_display_cmp);

        let ids = |v: &[SessionInfo]| v.iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(ids(&first), ids(&second));
        // wt1 sorts before wt2 on the top-level folder name.
        assert_eq!(ids(&first), vec![a, b]);
    }

    /// Two different agents in the same repo order by agent type, and that
    /// order is immune to `git_branch` flapping — the case Matt flagged.
    /// Branch detection wobbles (and is wrong under worktrees, #1067), so it
    /// must not participate in ordering.
    #[test]
    fn two_agents_same_repo_order_by_agent_ignoring_branch() {
        let claude = Uuid::from_u128(100);
        let codex = Uuid::from_u128(200);
        let mk = || {
            let mut c = session(claude, "/home/me/app", "h", Some("main"));
            c.agent_type = AgentType::Claude;
            let mut x = session(codex, "/home/me/app", "h", Some("feature"));
            x.agent_type = AgentType::Codex;
            vec![x, c] // codex-first input order on purpose
        };

        let mut v = mk();
        v.sort_by(session_display_cmp);
        // "claude" < "codex" → claude first, regardless of input order.
        assert_eq!(
            v.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![claude, codex]
        );

        // Flip/clear both branches — order must NOT change (branch isn't keyed).
        let mut v2 = mk();
        v2[0].git_branch = Some("totally-different".to_string());
        v2[1].git_branch = None;
        v2.sort_by(session_display_cmp);
        assert_eq!(
            v2.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![claude, codex]
        );
    }

    /// Focus stays attached to its session id even after the surrounding list
    /// is reordered (the core anti-bounce guarantee).
    #[test]
    fn focus_follows_id_across_reorder() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let hidden = HashSet::new();

        let order1 = vec![
            session(a, "/a", "h", None),
            session(b, "/b", "h", None),
            session(c, "/c", "h", None),
        ];
        assert_eq!(resolve_focus_index(&order1, Some(b), &hidden, 0), 1);

        // A later poll surfaces the same sessions in a different order; focus
        // by id resolves to b's NEW index, not the stale 1.
        let order2 = vec![
            session(c, "/c", "h", None),
            session(b, "/b", "h", None),
            session(a, "/a", "h", None),
        ];
        assert_eq!(resolve_focus_index(&order2, Some(b), &hidden, 1), 1);

        let order3 = vec![
            session(b, "/b", "h", None),
            session(a, "/a", "h", None),
            session(c, "/c", "h", None),
        ];
        assert_eq!(resolve_focus_index(&order3, Some(b), &hidden, 1), 0);
    }

    /// Nothing focused yet (`None`) falls back to the first non-hidden session.
    #[test]
    fn no_focus_falls_back_to_first_non_hidden() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut hidden = HashSet::new();
        hidden.insert(a);

        let sessions = vec![session(a, "/a", "h", None), session(b, "/b", "h", None)];
        // a is hidden → first non-hidden is b at index 1.
        assert_eq!(resolve_focus_index(&sessions, None, &hidden, 0), 1);
    }

    /// A focused id that is only *transiently* absent (e.g. a just-launched
    /// session dropped by a racing/stale poll — #1368) holds the previous
    /// position instead of snapping to the first session, and re-resolves by id
    /// the moment the session reappears.
    #[test]
    fn transiently_missing_focus_holds_previous_index() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let launched = Uuid::from_u128(3);
        let hidden = HashSet::new();

        // Focus follows the just-launched session (index 2 of the full list).
        let full = vec![
            session(a, "/a", "h", None),
            session(b, "/b", "h", None),
            session(launched, "/c", "h", None),
        ];
        assert_eq!(resolve_focus_index(&full, Some(launched), &hidden, 0), 2);

        // A stale poll response momentarily lacks the new session. Holding the
        // previous index (clamped to bounds) avoids the "jump to the first
        // session" bug — it does NOT fall back to index 0.
        let stale = vec![session(a, "/a", "h", None), session(b, "/b", "h", None)];
        assert_eq!(resolve_focus_index(&stale, Some(launched), &hidden, 2), 1);

        // Next refresh brings the session back; focus resolves by id again.
        assert_eq!(resolve_focus_index(&full, Some(launched), &hidden, 1), 2);
    }

    /// Grouped ordering sorts sessions into alphabetical host sections, and the
    /// section a session lands in is independent of the poll's input order.
    #[test]
    fn grouped_orders_sections_alphabetically_by_host() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let mk = || {
            vec![
                session(a, "/repo", "zeta", None),
                session(b, "/repo", "alpha", None),
                session(c, "/repo", "mid", None),
            ]
        };

        let mut forward = mk();
        forward.sort_by(session_display_cmp_grouped);
        let mut reversed = mk();
        reversed.reverse();
        reversed.sort_by(session_display_cmp_grouped);

        let hosts = |v: &[SessionInfo]| v.iter().map(|s| s.hostname.clone()).collect::<Vec<_>>();
        // alpha < mid < zeta, regardless of input order.
        assert_eq!(hosts(&forward), vec!["alpha", "mid", "zeta"]);
        assert_eq!(hosts(&forward), hosts(&reversed));
    }

    /// Within a single host section, grouped order matches the ungrouped total
    /// order (folder, then agent, then id) — grouping only reorders *across*
    /// hosts, never within one.
    #[test]
    fn grouped_keeps_within_host_order_stable() {
        let a = Uuid::from_u128(10);
        let b = Uuid::from_u128(20);
        let c = Uuid::from_u128(30);
        let mk = || {
            vec![
                session(c, "/home/me/charlie", "host", None),
                session(a, "/home/me/alpha", "host", None),
                session(b, "/home/me/bravo", "host", None),
            ]
        };

        let mut grouped = mk();
        grouped.sort_by(session_display_cmp_grouped);
        let mut ungrouped = mk();
        ungrouped.sort_by(session_display_cmp);

        let ids = |v: &[SessionInfo]| v.iter().map(|s| s.id).collect::<Vec<_>>();
        // Single host → grouped is identical to ungrouped, i.e. folder order.
        assert_eq!(ids(&grouped), ids(&ungrouped));
        assert_eq!(ids(&grouped), vec![a, b, c]);
    }

    /// Sessions with an empty (or whitespace-only) hostname collapse into one
    /// `unknown host` bucket that sorts *after* every named host.
    #[test]
    fn grouped_buckets_unknown_host_last() {
        let named = Uuid::from_u128(1);
        let blank = Uuid::from_u128(2);
        let spaces = Uuid::from_u128(3);
        let mut v = [
            session(blank, "/repo", "", None),
            session(named, "/repo", "web-01", None),
            session(spaces, "/repo", "   ", None),
        ];
        v.sort_by(session_display_cmp_grouped);

        // Named host first; the two empty/whitespace hosts share the trailing
        // unknown bucket (ordered among themselves by the total inner key).
        assert_eq!(v[0].id, named);
        assert_eq!(host_group_label(&v[0]), "web-01");
        assert_eq!(host_group_label(&v[1]), UNKNOWN_HOST_LABEL);
        assert_eq!(host_group_label(&v[2]), UNKNOWN_HOST_LABEL);
    }

    /// Focus resolution runs against the grouped order exactly as it does for the
    /// ungrouped order: focus stays pinned to its session id even though grouping
    /// moves that session to a different display index. This is the guarantee
    /// that nav numbers / `j`/`k` line up with the visible grouped rail.
    #[test]
    fn focus_resolves_against_grouped_order() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let hidden = HashSet::new();

        // Distinct hosts so grouping reorders relative to insertion order.
        let mut sessions = vec![
            session(a, "/repo", "zeta", None),
            session(b, "/repo", "alpha", None),
            session(c, "/repo", "mid", None),
        ];
        sessions.sort_by(session_display_cmp_grouped);
        // Grouped order is alpha(b), mid(c), zeta(a).
        assert_eq!(
            sessions.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![b, c, a]
        );

        // Focused session `a` (host zeta) resolves to its grouped index 2, not
        // its pre-sort index 0.
        assert_eq!(resolve_focus_index(&sessions, Some(a), &hidden, 0), 2);
        assert_eq!(resolve_focus_index(&sessions, Some(b), &hidden, 0), 0);
        assert_eq!(resolve_focus_index(&sessions, Some(c), &hidden, 0), 1);
    }

    /// The transient-miss hold must never index past the end of the list.
    #[test]
    fn transiently_missing_focus_clamps_to_bounds() {
        let a = Uuid::from_u128(1);
        let gone = Uuid::from_u128(99);
        let hidden = HashSet::new();

        let sessions = vec![session(a, "/a", "h", None)];
        // previous_index 5 is stale/out of range → clamped to the last index.
        assert_eq!(resolve_focus_index(&sessions, Some(gone), &hidden, 5), 0);

        // Empty list can't hold anything → 0.
        assert_eq!(resolve_focus_index(&[], Some(gone), &hidden, 3), 0);
    }
}
