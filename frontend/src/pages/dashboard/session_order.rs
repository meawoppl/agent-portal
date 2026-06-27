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

/// Resolve the focused session's index in the *currently displayed* order.
///
/// Focus is stored by `session_id` (the source of truth), so a reorder or a
/// refreshed poll never changes which session is focused. This maps that id
/// back to a display index for the rail / keyboard nav.
///
/// Falls back to the first non-hidden session (then `0`) when the focused id
/// is absent — e.g. nothing focused yet, or the focused session disappeared
/// (deleted / left). That fallback is the intended "focus moved because the
/// session is gone" behavior from the issue's acceptance criteria.
pub(super) fn resolve_focus_index(
    sessions: &[SessionInfo],
    focused_id: Option<Uuid>,
    hidden: &HashSet<Uuid>,
) -> usize {
    if let Some(id) = focused_id {
        if let Some(idx) = sessions.iter().position(|s| s.id == id) {
            return idx;
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
    use shared::{AgentType, SessionInfo, SessionStatus};

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
            my_role: "owner".to_string(),
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
        assert_eq!(resolve_focus_index(&order1, Some(b), &hidden), 1);

        // A later poll surfaces the same sessions in a different order; focus
        // by id resolves to b's NEW index, not the stale 1.
        let order2 = vec![
            session(c, "/c", "h", None),
            session(b, "/b", "h", None),
            session(a, "/a", "h", None),
        ];
        assert_eq!(resolve_focus_index(&order2, Some(b), &hidden), 1);

        let order3 = vec![
            session(b, "/b", "h", None),
            session(a, "/a", "h", None),
            session(c, "/c", "h", None),
        ];
        assert_eq!(resolve_focus_index(&order3, Some(b), &hidden), 0);
    }

    /// A disappeared focus id falls back to the first non-hidden session.
    #[test]
    fn missing_focus_falls_back_to_first_non_hidden() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let gone = Uuid::from_u128(99);
        let mut hidden = HashSet::new();
        hidden.insert(a);

        let sessions = vec![session(a, "/a", "h", None), session(b, "/b", "h", None)];
        // a is hidden → first non-hidden is b at index 1.
        assert_eq!(resolve_focus_index(&sessions, Some(gone), &hidden), 1);
        // None focus behaves the same.
        assert_eq!(resolve_focus_index(&sessions, None, &hidden), 1);
    }
}
