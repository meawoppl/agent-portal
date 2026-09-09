//! The transcript render budget, shared by both sides of the hydration wire.
//!
//! The dashboard keeps a bounded live buffer per session — a **rendering**
//! budget, since every buffered record is a live DOM subtree. The budget is
//! counted in *renderable* messages: Claude emits many bodyless cumulative
//! thinking-token markers per turn that render as one compact chip, so they
//! ride along without counting.
//!
//! This module is the single implementation of that counting rule and the
//! newest-window trim it drives. The backend uses it to size a hydration page
//! (`GET /api/sessions/{id}/messages?render_limit=N`) to exactly what the
//! client will keep, and the client applies the same trim to its live buffer —
//! one definition, so the page the server sends and the buffer the client
//! retains can never disagree (#1915).

/// Whether one stored wire frame counts against the render budget.
///
/// Claude's cumulative thinking-token system markers are free: many arrive per
/// turn and the frontend collapses them into a single chip.
pub fn counts_toward_render_limit(content: &str) -> bool {
    !matches!(
        serde_json::from_str::<crate::ClaudeOutput>(content),
        Ok(crate::ClaudeOutput::System(message)) if message.is_thinking_tokens()
    )
}

/// Trim `items` (chronological, oldest first) to the newest window containing
/// at most `max_cost` counting items. Non-counting items inside the kept
/// window survive; everything older than the window is dropped, counting or
/// not.
pub fn retain_newest_items_by_cost<T>(
    items: &mut Vec<T>,
    max_cost: usize,
    counts_toward_limit: impl Fn(&T) -> bool,
) {
    let excess = items
        .iter()
        .filter(|item| counts_toward_limit(item))
        .count()
        .saturating_sub(max_cost);
    if excess == 0 {
        return;
    }

    let mut counted = 0;
    let keep_from = items
        .iter()
        .position(|item| {
            if counts_toward_limit(item) {
                counted += 1;
            }
            counted == excess
        })
        .map_or(items.len(), |index| index + 1);
    items.drain(0..keep_from);
}

/// Size a newest-first page to exactly `render_limit` counting items: keep
/// rows from the newest backwards until the budget is spent (non-counting
/// rows in that window ride along), drop the rest, and return the kept rows
/// in chronological order. The server-side twin of
/// [`retain_newest_items_by_cost`] for pages fetched `created_at DESC`.
pub fn take_render_window_newest_first<T>(
    mut newest_first: Vec<T>,
    render_limit: usize,
    counts_toward_limit: impl Fn(&T) -> bool,
) -> Vec<T> {
    let mut counting = 0;
    let mut cut = newest_first.len();
    for (index, item) in newest_first.iter().enumerate() {
        if counts_toward_limit(item) {
            if counting == render_limit {
                cut = index;
                break;
            }
            counting += 1;
        }
    }
    // The loop cuts at the (limit+1)-th counting item, so non-counting items
    // older than the limit-th counting one stay in the window until the next
    // counting item — exactly where `retain_newest_items_by_cost`'s drain
    // lands, keeping the two sides' kept sets identical.
    newest_first.truncate(cut);
    newest_first.reverse();
    newest_first
}

#[cfg(test)]
mod tests {
    use super::*;

    const THINKING: &str = r#"{"type":"system","subtype":"thinking_tokens","session_id":"s","cumulative_thinking_tokens":42}"#;
    const TEXTISH: &str = r#"{"type":"user","content":"hello"}"#;

    #[test]
    fn thinking_token_markers_ride_free() {
        assert!(!counts_toward_render_limit(THINKING));
        assert!(counts_toward_render_limit(TEXTISH));
        assert!(counts_toward_render_limit("not json at all"));
    }

    /// The client trim and the server window must agree: trimming a
    /// chronological vector and windowing its newest-first reversal produce
    /// the same kept set.
    #[test]
    fn client_trim_and_server_window_agree() {
        let chronological: Vec<i32> = (0..20).collect();
        let counts = |n: &i32| n % 3 != 0; // every third item rides free

        let mut trimmed = chronological.clone();
        retain_newest_items_by_cost(&mut trimmed, 5, counts);

        let mut newest_first = chronological;
        newest_first.reverse();
        let windowed = take_render_window_newest_first(newest_first, 5, counts);

        assert_eq!(trimmed, windowed);
        assert_eq!(windowed.iter().filter(|n| counts(n)).count(), 5);
    }

    #[test]
    fn window_keeps_interspersed_free_items_but_not_older_ones() {
        // newest-first: [counting, free, counting, free, counting, ...]
        let newest_first = vec![(1, true), (2, false), (3, true), (4, false), (5, true)];
        let kept = take_render_window_newest_first(newest_first, 2, |(_, c)| *c);
        // Budget of 2 counting items → the free rows between AND immediately
        // older than the kept counting rows ride along (the window extends to
        // the next counting item, matching the client drain); the older
        // counting row (5) falls out. Chronological order out.
        assert_eq!(kept, vec![(4, false), (3, true), (2, false), (1, true)]);
    }

    #[test]
    fn underfilled_window_keeps_everything() {
        let newest_first = vec![1, 2, 3];
        let kept = take_render_window_newest_first(newest_first, 10, |_| true);
        assert_eq!(kept, vec![3, 2, 1]);
    }
}
