//! Minimal modal ("vim-like") editing for the message-input textarea.
//!
//! This is an OPT-IN, deliberately small subset of vim — enough to be genuinely
//! usable for composing a message without pulling in a heavy JS editor (which
//! would violate the WASM constraints of the frontend). Only two modes exist:
//!
//! - **NORMAL** — motions (`h j k l`, `w b`, `0 $`) and edits (`x`, `dd`, `dw`,
//!   `i a o O`).
//! - **INSERT** — typing inserts normally; `Esc` returns to NORMAL.
//!
//! The engine operates on the textarea's value as a `Vec<char>` with the cursor
//! tracked as a **char index**. The DOM's `selectionStart`/`setSelectionRange`
//! speak UTF-16 code units, so we convert at the boundary. Astral-plane
//! characters (emoji) count as a single "cell" here, which is close enough for
//! a chat box; full grapheme handling is out of scope for v1.
//!
//! The textarea is uncontrolled (the parent `InputBar` never passes `value`
//! through Yew), so this module reads and writes the DOM element directly, the
//! same way `InputBar::set_input_text` does.

use web_sys::{Element, HtmlTextAreaElement, KeyboardEvent};

/// The two supported editing modes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VimMode {
    Normal,
    Insert,
}

impl VimMode {
    /// Short label shown in the mode indicator.
    pub fn label(self) -> &'static str {
        match self {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
        }
    }

    /// CSS classes for the indicator (base + per-mode modifier).
    pub fn css_class(self) -> &'static str {
        match self {
            VimMode::Normal => "vim-mode-indicator normal",
            VimMode::Insert => "vim-mode-indicator insert",
        }
    }
}

/// Per-input modal state.
///
/// Starts in INSERT: enabling vim mode should leave the textarea behaving like
/// an ordinary chat box (typing works, `Enter` sends). The user opts into
/// NORMAL with `Esc`. This keeps the feature unobtrusive for anyone who toggled
/// it on to try it out.
pub struct VimState {
    pub mode: VimMode,
    /// A pending operator awaiting its motion. Currently only `d` (for `dd` and
    /// `dw`).
    pending_op: Option<char>,
}

impl Default for VimState {
    fn default() -> Self {
        Self {
            mode: VimMode::Insert,
            pending_op: None,
        }
    }
}

/// Outcome of feeding a keydown to the modal engine.
pub enum VimHandled {
    /// The key was consumed by vim (default action already prevented).
    /// `rerender` is true when the mode changed or the text was edited, so the
    /// caller should re-render to refresh the indicator and re-sync its tracked
    /// text mirror.
    Consumed { rerender: bool },
    /// Not a vim key in the current mode — the caller should run its normal
    /// keydown logic (Enter-to-send, Arrow history, plain typing, …).
    Passthrough,
}

/// Feed a keydown event to the modal engine. Mutates `state`, applies any edit
/// directly to `textarea`, and calls `event.prevent_default()` for consumed
/// keys.
pub fn handle_key(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
) -> VimHandled {
    // Never intercept modifier chords — leave copy/paste, browser shortcuts,
    // and the Ctrl+M voice toggle to their existing handlers.
    if event.ctrl_key() || event.meta_key() || event.alt_key() {
        return VimHandled::Passthrough;
    }

    let key = event.key();

    match state.mode {
        VimMode::Insert => {
            if key == "Escape" {
                enter_normal(state, textarea);
                event.prevent_default();
                VimHandled::Consumed { rerender: true }
            } else {
                // Enter (send / newline), Arrows (history / cursor), and plain
                // typing all fall through to the existing handler.
                VimHandled::Passthrough
            }
        }
        VimMode::Normal => handle_normal(state, textarea, event, &key),
    }
}

fn handle_normal(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
    key: &str,
) -> VimHandled {
    // Even in NORMAL mode, keep Enter sending and let the Arrow keys drive
    // command history / native caret movement (matches the non-vim contract).
    if matches!(
        key,
        "Enter" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight"
    ) {
        state.pending_op = None;
        return VimHandled::Passthrough;
    }

    let text: Vec<char> = textarea.value().chars().collect();
    let cursor = read_cursor(textarea, &text);

    // Resolve a pending operator (only `d` so far: `dd`, `dw`).
    if let Some(op) = state.pending_op.take() {
        if op == 'd' {
            match key {
                "d" => {
                    let (new_text, new_cursor) = delete_line(&text, cursor);
                    apply_edit(textarea, &new_text, new_cursor);
                    event.prevent_default();
                    return VimHandled::Consumed { rerender: true };
                }
                "w" => {
                    let (new_text, new_cursor) = delete_word(&text, cursor);
                    apply_edit(textarea, &new_text, new_cursor);
                    event.prevent_default();
                    return VimHandled::Consumed { rerender: true };
                }
                _ => {
                    // Unknown motion after `d` — cancel and swallow the key.
                    event.prevent_default();
                    return VimHandled::Consumed { rerender: false };
                }
            }
        }
    }

    match key {
        "h" => motion(textarea, &text, event, cursor.saturating_sub(1)),
        "l" => motion(textarea, &text, event, (cursor + 1).min(text.len())),
        "j" => motion(textarea, &text, event, line_down(&text, cursor)),
        "k" => motion(textarea, &text, event, line_up(&text, cursor)),
        "w" => motion(textarea, &text, event, next_word(&text, cursor)),
        "b" => motion(textarea, &text, event, prev_word(&text, cursor)),
        "0" => motion(textarea, &text, event, line_start(&text, cursor)),
        "$" => motion(textarea, &text, event, line_end(&text, cursor)),
        "i" => {
            state.mode = VimMode::Insert;
            move_cursor(textarea, &text, cursor);
            event.prevent_default();
            VimHandled::Consumed { rerender: true }
        }
        "a" => {
            state.mode = VimMode::Insert;
            move_cursor(textarea, &text, (cursor + 1).min(text.len()));
            event.prevent_default();
            VimHandled::Consumed { rerender: true }
        }
        "o" => {
            let (new_text, new_cursor) = open_line_below(&text, cursor);
            apply_edit(textarea, &new_text, new_cursor);
            state.mode = VimMode::Insert;
            event.prevent_default();
            VimHandled::Consumed { rerender: true }
        }
        "O" => {
            let (new_text, new_cursor) = open_line_above(&text, cursor);
            apply_edit(textarea, &new_text, new_cursor);
            state.mode = VimMode::Insert;
            event.prevent_default();
            VimHandled::Consumed { rerender: true }
        }
        "x" => {
            let (new_text, new_cursor) = delete_char(&text, cursor);
            apply_edit(textarea, &new_text, new_cursor);
            event.prevent_default();
            VimHandled::Consumed { rerender: true }
        }
        "d" => {
            state.pending_op = Some('d');
            event.prevent_default();
            VimHandled::Consumed { rerender: false }
        }
        "Escape" => {
            // Already NORMAL — clear any half-typed operator.
            event.prevent_default();
            VimHandled::Consumed { rerender: false }
        }
        _ => {
            // Swallow any other single printable character so stray letters
            // never leak into the textarea while in NORMAL mode. Let genuine
            // control keys (Tab, function keys, …) through.
            if key.chars().count() == 1 {
                event.prevent_default();
                VimHandled::Consumed { rerender: false }
            } else {
                VimHandled::Passthrough
            }
        }
    }
}

/// Apply a pure cursor motion: prevent default and move the caret.
fn motion(
    textarea: &HtmlTextAreaElement,
    text: &[char],
    event: &KeyboardEvent,
    target: usize,
) -> VimHandled {
    move_cursor(textarea, text, target);
    event.prevent_default();
    VimHandled::Consumed { rerender: false }
}

/// Switch to NORMAL mode, nudging the caret one cell left (vim moves off the
/// just-typed character), clamped to the start of the current line.
fn enter_normal(state: &mut VimState, textarea: &HtmlTextAreaElement) {
    state.mode = VimMode::Normal;
    state.pending_op = None;
    let text: Vec<char> = textarea.value().chars().collect();
    let cursor = read_cursor(textarea, &text);
    let ls = line_start(&text, cursor);
    let new = if cursor > ls { cursor - 1 } else { cursor };
    move_cursor(textarea, &text, new);
}

// --- DOM boundary helpers ---------------------------------------------------

fn read_cursor(textarea: &HtmlTextAreaElement, text: &[char]) -> usize {
    let off = textarea.selection_start().ok().flatten().unwrap_or(0);
    utf16_to_char_idx(text, off)
}

fn move_cursor(textarea: &HtmlTextAreaElement, text: &[char], idx: usize) {
    let off = char_idx_to_utf16(text, idx);
    let _ = textarea.set_selection_range(off, off);
}

fn apply_edit(textarea: &HtmlTextAreaElement, text: &[char], cursor: usize) {
    let s: String = text.iter().collect();
    textarea.set_value(&s);
    autosize(textarea, &s);
    let off = char_idx_to_utf16(text, cursor);
    let _ = textarea.set_selection_range(off, off);
}

/// Resize the textarea to fit its content (mirrors `InputBar::set_input_text`).
fn autosize(textarea: &HtmlTextAreaElement, text: &str) {
    let el: &Element = textarea.as_ref();
    if text.is_empty() {
        el.remove_attribute("style").ok();
    } else {
        el.set_attribute("style", "height: 0; overflow-y: hidden")
            .ok();
        el.set_attribute("style", &format!("height: {}px", textarea.scroll_height()))
            .ok();
    }
}

fn char_idx_to_utf16(text: &[char], idx: usize) -> u32 {
    text[..idx.min(text.len())]
        .iter()
        .map(|c| c.len_utf16() as u32)
        .sum()
}

fn utf16_to_char_idx(text: &[char], off: u32) -> usize {
    let mut acc = 0u32;
    for (i, c) in text.iter().enumerate() {
        if acc >= off {
            return i;
        }
        acc += c.len_utf16() as u32;
    }
    text.len()
}

// --- Pure motion / edit primitives (unit-tested below) ----------------------

/// Index of the first character of the line containing `cursor`.
fn line_start(text: &[char], cursor: usize) -> usize {
    let mut i = cursor.min(text.len());
    while i > 0 && text[i - 1] != '\n' {
        i -= 1;
    }
    i
}

/// Index just past the last character of the line containing `cursor` (i.e. the
/// position of the terminating `\n`, or the end of the text).
fn line_end(text: &[char], cursor: usize) -> usize {
    let mut i = cursor.min(text.len());
    while i < text.len() && text[i] != '\n' {
        i += 1;
    }
    i
}

fn line_down(text: &[char], cursor: usize) -> usize {
    let ls = line_start(text, cursor);
    let col = cursor - ls;
    let le = line_end(text, cursor);
    if le >= text.len() {
        return cursor; // already on the last line
    }
    let next_ls = le + 1;
    let next_le = line_end(text, next_ls);
    (next_ls + col).min(next_le)
}

fn line_up(text: &[char], cursor: usize) -> usize {
    let ls = line_start(text, cursor);
    if ls == 0 {
        return cursor; // already on the first line
    }
    let col = cursor - ls;
    let prev_le = ls - 1; // the '\n' ending the previous line
    let prev_ls = line_start(text, prev_le);
    (prev_ls + col).min(prev_le)
}

/// Start of the next word. Words are maximal runs of non-whitespace.
fn next_word(text: &[char], cursor: usize) -> usize {
    let n = text.len();
    let mut i = cursor;
    if i >= n {
        return n;
    }
    while i < n && !text[i].is_whitespace() {
        i += 1;
    }
    while i < n && text[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Start of the current or previous word.
fn prev_word(text: &[char], cursor: usize) -> usize {
    let mut i = cursor;
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && text[i].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !text[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

fn delete_char(text: &[char], cursor: usize) -> (Vec<char>, usize) {
    let mut v = text.to_vec();
    if cursor < v.len() {
        v.remove(cursor);
    }
    let c = cursor.min(v.len());
    (v, c)
}

fn delete_word(text: &[char], cursor: usize) -> (Vec<char>, usize) {
    let end = next_word(text, cursor);
    let mut v = text.to_vec();
    let start = cursor.min(v.len());
    let end = end.min(v.len());
    v.drain(start..end);
    let cursor = start.min(v.len());
    (v, cursor)
}

fn delete_line(text: &[char], cursor: usize) -> (Vec<char>, usize) {
    let ls = line_start(text, cursor);
    let le = line_end(text, cursor);
    let mut v = text.to_vec();
    if le < v.len() {
        // Include the trailing newline so the line fully disappears.
        v.drain(ls..=le);
    } else if ls > 0 {
        // Last line: also drop the newline that precedes it.
        v.drain(ls - 1..le);
    } else {
        // Only line: clear it.
        v.drain(ls..le);
    }
    let c = line_start(&v, ls.min(v.len()));
    (v, c)
}

fn open_line_below(text: &[char], cursor: usize) -> (Vec<char>, usize) {
    let le = line_end(text, cursor);
    let mut v = text.to_vec();
    v.insert(le, '\n');
    (v, le + 1)
}

fn open_line_above(text: &[char], cursor: usize) -> (Vec<char>, usize) {
    let ls = line_start(text, cursor);
    let mut v = text.to_vec();
    v.insert(ls, '\n');
    (v, ls)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn line_boundaries() {
        let t = chars("ab\ncd");
        assert_eq!(line_start(&t, 4), 3);
        assert_eq!(line_end(&t, 4), 5);
        assert_eq!(line_start(&t, 1), 0);
        assert_eq!(line_end(&t, 1), 2);
    }

    #[test]
    fn horizontal_motions() {
        let t = chars("hello");
        assert_eq!(line_start(&t, 3), 0);
        assert_eq!(line_end(&t, 3), 5);
    }

    #[test]
    fn vertical_motions_preserve_column() {
        let t = chars("abcd\nef\nghij");
        // From col 3 on line 0 → line 1 ("ef") has only 2 chars, clamp to its
        // end (index 7, the caret just past 'f').
        assert_eq!(line_down(&t, 3), 7);
        // From col 1 on line 2 ("ghij", starts at 8) → same column on line 1.
        assert_eq!(line_up(&t, 9), 6);
        // Up from the first line is a no-op.
        assert_eq!(line_up(&t, 2), 2);
    }

    #[test]
    fn word_motions() {
        let t = chars("foo bar  baz");
        assert_eq!(next_word(&t, 0), 4); // start of "bar"
        assert_eq!(next_word(&t, 4), 9); // start of "baz"
        assert_eq!(prev_word(&t, 9), 4); // back to "bar"
        assert_eq!(prev_word(&t, 4), 0); // back to "foo"
    }

    #[test]
    fn x_deletes_char_under_cursor() {
        let (v, c) = delete_char(&chars("abc"), 1);
        assert_eq!(v.iter().collect::<String>(), "ac");
        assert_eq!(c, 1);
        // At end of buffer, nothing to delete.
        let (v, c) = delete_char(&chars("ab"), 2);
        assert_eq!(v.iter().collect::<String>(), "ab");
        assert_eq!(c, 2);
    }

    #[test]
    fn dw_deletes_to_next_word() {
        let (v, c) = delete_word(&chars("foo bar"), 0);
        assert_eq!(v.iter().collect::<String>(), "bar");
        assert_eq!(c, 0);
    }

    #[test]
    fn dd_deletes_middle_line_with_trailing_newline() {
        let (v, c) = delete_line(&chars("a\nb\nc"), 2);
        assert_eq!(v.iter().collect::<String>(), "a\nc");
        assert_eq!(c, 2);
    }

    #[test]
    fn dd_deletes_last_line_with_preceding_newline() {
        let (v, _c) = delete_line(&chars("a\nb"), 2);
        assert_eq!(v.iter().collect::<String>(), "a");
    }

    #[test]
    fn dd_clears_only_line() {
        let (v, c) = delete_line(&chars("abc"), 1);
        assert_eq!(v.iter().collect::<String>(), "");
        assert_eq!(c, 0);
    }

    #[test]
    fn o_and_O_insert_newlines() {
        let (v, c) = open_line_below(&chars("ab\ncd"), 1);
        assert_eq!(v.iter().collect::<String>(), "ab\n\ncd");
        assert_eq!(c, 3);
        let (v, c) = open_line_above(&chars("ab\ncd"), 4);
        assert_eq!(v.iter().collect::<String>(), "ab\n\ncd");
        assert_eq!(c, 3);
    }

    #[test]
    fn utf16_roundtrip_with_astral() {
        // "a😀b": the emoji is 2 UTF-16 units.
        let t = chars("a😀b");
        assert_eq!(char_idx_to_utf16(&t, 0), 0);
        assert_eq!(char_idx_to_utf16(&t, 1), 1);
        assert_eq!(char_idx_to_utf16(&t, 2), 3);
        assert_eq!(char_idx_to_utf16(&t, 3), 4);
        assert_eq!(utf16_to_char_idx(&t, 3), 2);
        assert_eq!(utf16_to_char_idx(&t, 1), 1);
    }
}
