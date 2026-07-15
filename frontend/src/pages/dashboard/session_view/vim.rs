//! Modal ("vim-like") editing for the message-input textarea.
//!
//! This is an OPT-IN vim subset — enough to compose a message without pulling
//! in a heavy JS editor (which would violate the WASM constraints of the
//! frontend). Two modes exist:
//!
//! - **NORMAL** — motions (`h j k l`, `w b e`, `0 $`, `gg G`), edits (`x`,
//!   `r` replace, `~` toggle-case, `dd`/`dw`/`D`/`C`/`cc`/`S`, `i a o O A I`),
//!   operators (`d`/`c`/`y`) over motions and text objects (`iw`/`aw`,
//!   `i(`/`a(`, `i"`…), paste (`p`/`P`), undo (`u`), and page scrolling of the
//!   conversation (`Ctrl-d/u/f/b`, `gg`, `G`).
//! - **INSERT** — typing inserts normally; `Esc` returns to NORMAL.
//!
//! The engine operates on the textarea's value as a `Vec<char>` with the cursor
//! tracked as a **char index**. The DOM's `selectionStart`/`setSelectionRange`
//! speak UTF-16 code units, so we convert at the boundary. Astral-plane
//! characters (emoji) count as a single "cell" here, which is close enough for
//! a chat box; full grapheme handling is out of scope.
//!
//! **Block cursor.** In NORMAL mode the caret is rendered as the classic vim
//! block by keeping a one-character DOM selection (`sel = idx..idx+1`) and
//! having the parent add a `vim-normal` CSS class to the textarea, so
//! `.message-input.vim-normal::selection` paints a solid block. At end-of-line
//! / empty input the selection collapses (no char to cover). INSERT mode
//! removes the class and collapses to a thin caret. The class is Yew-managed
//! (the parent reads `state.mode` when rendering) so a re-render can't wipe it;
//! this module only ever sets the *selection*, never the class.
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

/// An operator awaiting a motion or text object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Delete,
    Change,
    Yank,
}

/// The multi-key sequence state machine. Only one of these is ever pending
/// between keystrokes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    /// Fresh — the next key starts a new command.
    None,
    /// An operator (`d`/`c`/`y`) is waiting for its motion, a doubled operator
    /// char (linewise), or a text-object introducer (`i`/`a`).
    Operator(Op),
    /// A text object is being built: operator + `i`(inner)/`a`(around) seen,
    /// awaiting the object selector (`w`, a delimiter, or a quote).
    TextObject { op: Op, around: bool },
    /// `r` seen — the next key is the replacement character.
    Replace,
    /// `g` seen — awaiting a second `g` (for `gg`).
    GPrefix,
}

/// The yank/delete register. Charwise holds a fragment; linewise holds whole
/// line(s) without the trailing newline (paste re-adds it).
#[derive(Default, Clone)]
struct Register {
    text: String,
    linewise: bool,
}

/// Which conversation-scroll action a key maps to.
#[derive(Clone, Copy)]
enum Scroll {
    HalfDown,
    HalfUp,
    FullDown,
    FullUp,
    Top,
    Bottom,
}

const UNDO_LIMIT: usize = 100;

/// Per-input modal state.
///
/// Starts in INSERT: enabling vim mode should leave the textarea behaving like
/// an ordinary chat box (typing works, `Enter` sends). The user opts into
/// NORMAL with `Esc`. This keeps the feature unobtrusive for anyone who toggled
/// it on to try it out.
pub struct VimState {
    pub mode: VimMode,
    pending: Pending,
    /// Numeric count prefix accumulated in NORMAL mode (`3j`, `2dd`).
    count: Option<usize>,
    /// Internal paste register, populated by x/d/c/y.
    register: Register,
    /// Bounded undo stack of `(text, cursor)` snapshots pushed before each
    /// mutating op.
    undo: Vec<(String, usize)>,
}

impl Default for VimState {
    fn default() -> Self {
        Self {
            mode: VimMode::Insert,
            pending: Pending::None,
            count: None,
            register: Register::default(),
            undo: Vec::new(),
        }
    }
}

impl VimState {
    /// Reset the multi-key sequence machine (pending op + count).
    fn clear_pending(&mut self) {
        self.pending = Pending::None;
        self.count = None;
    }

    /// Whether a multi-key command is mid-flight (operator/text-object/`r`/`g`
    /// or a count prefix). Used so `Esc` cancels a half-typed command before it
    /// falls through to "leave the input for dashboard Nav mode".
    fn has_pending(&self) -> bool {
        !matches!(self.pending, Pending::None) || self.count.is_some()
    }

    /// Consume the pending count, defaulting to 1.
    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }

    /// Push a pre-mutation snapshot onto the bounded undo stack.
    fn push_undo(&mut self, text: &str, cursor: usize) {
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push((text.to_string(), cursor));
    }

    /// Pop the most recent snapshot, if any.
    fn pop_undo(&mut self) -> Option<(String, usize)> {
        self.undo.pop()
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
    /// NORMAL `Esc` with no pending command: vim reset itself to INSERT (block
    /// cursor collapsed) and blurred the textarea. The caller re-renders (to
    /// drop the `vim-normal` class) but must NOT `stop_propagation`, so the
    /// `Esc` bubbles to `use_keyboard_nav` and hands off to Nav mode.
    ExitToNav,
}

/// Feed a keydown event to the modal engine. Mutates `state`, applies any edit
/// directly to `textarea`, and calls `event.prevent_default()` for consumed
/// keys.
pub fn handle_key(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
) -> VimHandled {
    // Never intercept meta/alt chords — leave OS/browser shortcuts alone.
    if event.meta_key() || event.alt_key() {
        return VimHandled::Passthrough;
    }

    let key = event.key();

    match state.mode {
        VimMode::Insert => {
            // In INSERT, leave all Ctrl chords (copy/paste/voice) untouched.
            if event.ctrl_key() {
                return VimHandled::Passthrough;
            }
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
    // Ctrl chords in NORMAL: only the page-scroll set is ours; everything else
    // (Ctrl+C/V/A, refresh, …) passes through.
    if event.ctrl_key() {
        let scroll = match key {
            "d" => Some(Scroll::HalfDown),
            "u" => Some(Scroll::HalfUp),
            "f" => Some(Scroll::FullDown),
            "b" => Some(Scroll::FullUp),
            _ => None,
        };
        return match scroll {
            Some(s) => {
                scroll_messages(textarea, s);
                event.prevent_default();
                VimHandled::Consumed { rerender: false }
            }
            None => VimHandled::Passthrough,
        };
    }

    // Even in NORMAL mode, keep Enter sending and let the Arrow keys drive
    // command history / native caret movement (matches the non-vim contract).
    if matches!(
        key,
        "Enter" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight"
    ) {
        state.clear_pending();
        return VimHandled::Passthrough;
    }

    let text: Vec<char> = textarea.value().chars().collect();
    let cursor = read_cursor(textarea, &text);

    match state.pending {
        Pending::Replace => return resolve_replace(state, textarea, event, key, &text, cursor),
        Pending::GPrefix => return resolve_g(state, textarea, event, key),
        Pending::TextObject { op, around } => {
            return resolve_text_object(state, textarea, event, key, &text, cursor, (op, around));
        }
        Pending::Operator(op) => {
            return resolve_operator(state, textarea, event, key, &text, cursor, op);
        }
        Pending::None => {}
    }

    // --- Fresh key: numeric count prefix -----------------------------------
    if let Some(d) = key.chars().next().filter(|c| c.is_ascii_digit()) {
        let d = d as usize - '0' as usize;
        // A leading `0` is the line-start motion, not a count digit.
        if d != 0 || state.count.is_some() {
            state.count = Some(state.count.unwrap_or(0).saturating_mul(10) + d);
            event.prevent_default();
            return VimHandled::Consumed { rerender: false };
        }
    }

    // --- Plain motions (count-aware) ---------------------------------------
    if let Some(mut target) = simple_motion_target(key, &text, cursor) {
        let n = state.take_count();
        // Re-apply the motion `n` times for counts like `3w`.
        for _ in 1..n {
            target = simple_motion_target(key, &text, target).unwrap_or(target);
        }
        return motion(textarea, &text, event, target);
    }

    match key {
        // Enter INSERT ------------------------------------------------------
        "i" => enter_insert_at(state, textarea, event, &text, cursor),
        "a" => enter_insert_at(state, textarea, event, &text, (cursor + 1).min(text.len())),
        "A" => {
            let le = line_end(&text, cursor);
            enter_insert_at(state, textarea, event, &text, le)
        }
        "I" => {
            let fnb = first_non_blank(&text, cursor);
            enter_insert_at(state, textarea, event, &text, fnb)
        }
        "o" => {
            let cur: String = text.iter().collect();
            state.push_undo(&cur, cursor);
            let (new_text, new_cursor) = open_line_below(&text, cursor);
            set_text(textarea, &new_text);
            state.mode = VimMode::Insert;
            move_cursor(textarea, &new_text, new_cursor);
            state.clear_pending();
            event.prevent_default();
            VimHandled::Consumed { rerender: true }
        }
        "O" => {
            let cur: String = text.iter().collect();
            state.push_undo(&cur, cursor);
            let (new_text, new_cursor) = open_line_above(&text, cursor);
            set_text(textarea, &new_text);
            state.mode = VimMode::Insert;
            move_cursor(textarea, &new_text, new_cursor);
            state.clear_pending();
            event.prevent_default();
            VimHandled::Consumed { rerender: true }
        }

        // Deletes / changes -------------------------------------------------
        "x" => {
            let n = state.take_count();
            let end = (cursor + n).min(text.len());
            edit_range(state, textarea, event, &text, Op::Delete, cursor, end)
        }
        "D" => {
            let le = line_end(&text, cursor);
            edit_range(state, textarea, event, &text, Op::Delete, cursor, le)
        }
        "C" => {
            let le = line_end(&text, cursor);
            edit_range(state, textarea, event, &text, Op::Change, cursor, le)
        }
        "S" => linewise_op(state, textarea, event, &text, cursor, Op::Change, 1),

        // Operators (await motion / text object) ----------------------------
        "d" => set_operator(state, event, Op::Delete),
        "c" => set_operator(state, event, Op::Change),
        "y" => set_operator(state, event, Op::Yank),

        // Replace / toggle-case --------------------------------------------
        "r" => {
            state.pending = Pending::Replace;
            event.prevent_default();
            VimHandled::Consumed { rerender: false }
        }
        "~" => {
            let n = state.take_count();
            let cur: String = text.iter().collect();
            let (mut v, mut c) = (text.clone(), cursor);
            let mut changed = false;
            for _ in 0..n {
                let (nv, nc) = toggle_case(&v, c);
                if nv != v {
                    changed = true;
                }
                v = nv;
                c = nc;
            }
            if changed {
                state.push_undo(&cur, cursor);
                set_text(textarea, &v);
            }
            place_block(textarea, &v, c);
            state.clear_pending();
            event.prevent_default();
            VimHandled::Consumed { rerender: changed }
        }

        // Paste -------------------------------------------------------------
        "p" => paste_cmd(state, textarea, event, &text, cursor, true),
        "P" => paste_cmd(state, textarea, event, &text, cursor, false),

        // Undo --------------------------------------------------------------
        "u" => {
            if let Some((prev, prev_cursor)) = state.pop_undo() {
                let v: Vec<char> = prev.chars().collect();
                set_text(textarea, &v);
                place_block(textarea, &v, prev_cursor.min(v.len()));
                state.clear_pending();
                event.prevent_default();
                VimHandled::Consumed { rerender: true }
            } else {
                state.clear_pending();
                event.prevent_default();
                VimHandled::Consumed { rerender: false }
            }
        }

        // Scroll top/bottom -------------------------------------------------
        "g" => {
            state.pending = Pending::GPrefix;
            event.prevent_default();
            VimHandled::Consumed { rerender: false }
        }
        "G" => {
            scroll_messages(textarea, Scroll::Bottom);
            state.clear_pending();
            event.prevent_default();
            VimHandled::Consumed { rerender: false }
        }

        "Escape" => {
            if state.has_pending() {
                // A command is half-typed (`d`, `2`, `di`, `r`, `g`…) — `Esc`
                // cancels it and stays in NORMAL, like real vim.
                state.clear_pending();
                place_block(textarea, &text, cursor);
                event.prevent_default();
                VimHandled::Consumed { rerender: false }
            } else {
                // Clean NORMAL: a second `Esc` drops out of the input into the
                // dashboard's keyboard-nav (Nav mode). Reset to INSERT first —
                // this collapses the block cursor and means returning to the box
                // (via Nav mode's `i`, which refocuses it) lands ready to type.
                // Then blur so nav keys reach the nav hook instead of vim, and
                // return ExitToNav so the caller re-renders but lets the Esc
                // bubble to the nav hook. First Esc (INSERT→NORMAL) is still
                // consumed, so it takes two presses to leave.
                state.mode = VimMode::Insert;
                state.clear_pending();
                move_cursor(textarea, &text, cursor);
                let html_el: &web_sys::HtmlElement = textarea.as_ref();
                let _ = html_el.blur();
                VimHandled::ExitToNav
            }
        }
        _ => {
            // Swallow any other single printable character so stray letters
            // never leak into the textarea while in NORMAL mode. Let genuine
            // control keys (Tab, function keys, …) through.
            state.clear_pending();
            if key.chars().count() == 1 {
                event.prevent_default();
                VimHandled::Consumed { rerender: false }
            } else {
                VimHandled::Passthrough
            }
        }
    }
}

// --- Sequence resolvers -----------------------------------------------------

fn set_operator(state: &mut VimState, event: &KeyboardEvent, op: Op) -> VimHandled {
    state.pending = Pending::Operator(op);
    event.prevent_default();
    VimHandled::Consumed { rerender: false }
}

fn resolve_operator(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
    key: &str,
    text: &[char],
    cursor: usize,
    op: Op,
) -> VimHandled {
    // `i`/`a` introduce a text object.
    match key {
        "i" => {
            state.pending = Pending::TextObject { op, around: false };
            event.prevent_default();
            return VimHandled::Consumed { rerender: false };
        }
        "a" => {
            state.pending = Pending::TextObject { op, around: true };
            event.prevent_default();
            return VimHandled::Consumed { rerender: false };
        }
        _ => {}
    }

    // Doubled operator (`dd`/`cc`/`yy`) → linewise.
    let doubled = matches!(
        (op, key),
        (Op::Delete, "d") | (Op::Change, "c") | (Op::Yank, "y")
    );
    if doubled {
        let n = state.take_count();
        return linewise_op(state, textarea, event, text, cursor, op, n);
    }

    // Operator over a motion (`dw`, `d$`, `de`, `y0`, …).
    if let Some((start, end)) = operator_motion_range(key, text, cursor) {
        state.count = None;
        return edit_range(state, textarea, event, text, op, start, end);
    }

    // Unknown motion — cancel and swallow.
    state.clear_pending();
    event.prevent_default();
    VimHandled::Consumed { rerender: false }
}

fn resolve_text_object(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
    key: &str,
    text: &[char],
    cursor: usize,
    spec: (Op, bool),
) -> VimHandled {
    let (op, around) = spec;
    let kind = text_object_kind(key);
    let range = kind.and_then(|k| text_object_range(text, cursor, k, around));
    state.count = None;
    match range {
        Some((start, end)) => edit_range(state, textarea, event, text, op, start, end),
        None => {
            state.clear_pending();
            event.prevent_default();
            VimHandled::Consumed { rerender: false }
        }
    }
}

fn resolve_replace(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
    key: &str,
    text: &[char],
    cursor: usize,
) -> VimHandled {
    state.clear_pending();
    // The replacement char: a single printable char, or Enter → newline.
    let repl = if key == "Enter" {
        Some('\n')
    } else if key == "Escape" {
        None
    } else {
        let mut it = key.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    };
    if let Some(ch) = repl {
        if cursor < text.len() && text[cursor] != '\n' {
            let cur: String = text.iter().collect();
            state.push_undo(&cur, cursor);
            let v = replace_char(text, cursor, ch);
            set_text(textarea, &v);
            place_block(textarea, &v, cursor);
            event.prevent_default();
            return VimHandled::Consumed { rerender: true };
        }
    }
    place_block(textarea, text, cursor);
    event.prevent_default();
    VimHandled::Consumed { rerender: false }
}

fn resolve_g(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
    key: &str,
) -> VimHandled {
    state.clear_pending();
    if key == "g" {
        scroll_messages(textarea, Scroll::Top);
    }
    event.prevent_default();
    VimHandled::Consumed { rerender: false }
}

/// Enter INSERT mode with the caret collapsed at `idx`.
fn enter_insert_at(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
    text: &[char],
    idx: usize,
) -> VimHandled {
    state.mode = VimMode::Insert;
    state.clear_pending();
    move_cursor(textarea, text, idx);
    event.prevent_default();
    VimHandled::Consumed { rerender: true }
}

/// Apply an operator over a charwise `[start, end)` range. Handles register
/// capture, undo snapshot, cursor placement, and INSERT transition for
/// `Change`. Linewise operators go through `linewise_op` instead.
fn edit_range(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
    text: &[char],
    op: Op,
    start: usize,
    end: usize,
) -> VimHandled {
    state.clear_pending();
    event.prevent_default();

    let (start, end) = (start.min(text.len()), end.min(text.len()));
    let removed: String = if start < end {
        text[start..end].iter().collect()
    } else {
        String::new()
    };

    match op {
        Op::Yank => {
            state.register = Register {
                text: removed,
                linewise: false,
            };
            // Yank leaves the buffer unchanged; move the caret to the range
            // start (vim behavior for most yanks).
            place_block(textarea, text, start);
            VimHandled::Consumed { rerender: false }
        }
        Op::Delete | Op::Change => {
            let cur: String = text.iter().collect();
            state.push_undo(&cur, start.min(text.len()));
            state.register = Register {
                text: removed,
                linewise: false,
            };
            let (v, c) = remove_range(text, start, end);
            set_text(textarea, &v);
            if op == Op::Change {
                state.mode = VimMode::Insert;
                move_cursor(textarea, &v, c);
            } else {
                place_block(textarea, &v, c);
            }
            VimHandled::Consumed { rerender: true }
        }
    }
}

/// Linewise operator over `n` lines starting at the cursor's line.
fn linewise_op(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
    text: &[char],
    cursor: usize,
    op: Op,
    n: usize,
) -> VimHandled {
    state.clear_pending();
    event.prevent_default();

    let ls = line_start(text, cursor);
    // End of the nth line down (inclusive of its content, exclusive of the
    // final trailing newline).
    let mut le = line_end(text, cursor);
    for _ in 1..n {
        if le >= text.len() {
            break;
        }
        le = line_end(text, le + 1);
    }
    let content: String = text[ls..le.min(text.len())].iter().collect();

    match op {
        Op::Yank => {
            state.register = Register {
                text: content,
                linewise: true,
            };
            place_block(textarea, text, ls);
            VimHandled::Consumed { rerender: false }
        }
        Op::Delete => {
            let cur: String = text.iter().collect();
            state.push_undo(&cur, ls);
            state.register = Register {
                text: content,
                linewise: true,
            };
            let (v, c) = delete_lines(text, ls, le);
            set_text(textarea, &v);
            place_block(textarea, &v, c);
            VimHandled::Consumed { rerender: true }
        }
        Op::Change => {
            // `cc`/`S`: keep the line but blank its content, enter INSERT.
            let cur: String = text.iter().collect();
            state.push_undo(&cur, ls);
            state.register = Register {
                text: content,
                linewise: true,
            };
            let (v, c) = remove_range(text, ls, le.min(text.len()));
            set_text(textarea, &v);
            state.mode = VimMode::Insert;
            move_cursor(textarea, &v, c);
            VimHandled::Consumed { rerender: true }
        }
    }
}

fn paste_cmd(
    state: &mut VimState,
    textarea: &HtmlTextAreaElement,
    event: &KeyboardEvent,
    text: &[char],
    cursor: usize,
    after: bool,
) -> VimHandled {
    state.clear_pending();
    event.prevent_default();
    if state.register.text.is_empty() {
        place_block(textarea, text, cursor);
        return VimHandled::Consumed { rerender: false };
    }
    let cur: String = text.iter().collect();
    state.push_undo(&cur, cursor);
    let (v, c) = paste(text, cursor, &state.register, after);
    set_text(textarea, &v);
    place_block(textarea, &v, c);
    VimHandled::Consumed { rerender: true }
}

/// Apply a pure cursor motion: prevent default and move the block caret.
fn motion(
    textarea: &HtmlTextAreaElement,
    text: &[char],
    event: &KeyboardEvent,
    target: usize,
) -> VimHandled {
    place_block(textarea, text, target);
    event.prevent_default();
    VimHandled::Consumed { rerender: false }
}

/// Switch to NORMAL mode, nudging the caret one cell left (vim moves off the
/// just-typed character), clamped to the start of the current line.
fn enter_normal(state: &mut VimState, textarea: &HtmlTextAreaElement) {
    state.mode = VimMode::Normal;
    state.clear_pending();
    let text: Vec<char> = textarea.value().chars().collect();
    let cursor = read_cursor(textarea, &text);
    let ls = line_start(&text, cursor);
    let new = if cursor > ls { cursor - 1 } else { cursor };
    place_block(textarea, &text, new);
}

// --- DOM boundary helpers ---------------------------------------------------

fn read_cursor(textarea: &HtmlTextAreaElement, text: &[char]) -> usize {
    let off = textarea.selection_start().ok().flatten().unwrap_or(0);
    utf16_to_char_idx(text, off)
}

/// Collapse the caret at `idx` (INSERT-mode thin caret / mode transitions).
fn move_cursor(textarea: &HtmlTextAreaElement, text: &[char], idx: usize) {
    let off = char_idx_to_utf16(text, idx);
    let _ = textarea.set_selection_range(off, off);
}

/// Place the NORMAL-mode block caret: a one-char selection covering the cell at
/// `idx`, or a collapsed caret at end-of-line / empty input.
fn place_block(textarea: &HtmlTextAreaElement, text: &[char], idx: usize) {
    let idx = idx.min(text.len());
    let start = char_idx_to_utf16(text, idx);
    let at_eol = idx >= text.len() || text[idx] == '\n';
    let end = if at_eol {
        start
    } else {
        char_idx_to_utf16(text, idx + 1)
    };
    let _ = textarea.set_selection_range(start, end);
}

/// Write `text` to the DOM element and auto-resize it. Does not move the caret;
/// callers position it via `place_block` / `move_cursor`.
fn set_text(textarea: &HtmlTextAreaElement, text: &[char]) {
    let s: String = text.iter().collect();
    textarea.set_value(&s);
    autosize(textarea, &s);
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

/// Scroll the conversation transcript container (`.session-view-messages`) for
/// this session. Discovered by walking up from the textarea to the enclosing
/// `.session-view` and querying its scrollable messages pane — this keeps the
/// right pane scrolling when several session views are on screen. Uses
/// `set_scroll_top` (which clamps) so no smooth-scroll web-sys feature is
/// needed.
fn scroll_messages(textarea: &HtmlTextAreaElement, kind: Scroll) {
    let el: &Element = textarea.as_ref();
    let Some(view) = el.closest(".session-view").ok().flatten() else {
        return;
    };
    let Some(msgs) = view.query_selector(".session-view-messages").ok().flatten() else {
        return;
    };
    let ch = msgs.client_height();
    let sh = msgs.scroll_height();
    let st = msgs.scroll_top();
    let target = match kind {
        Scroll::HalfDown => st + ch / 2,
        Scroll::HalfUp => st - ch / 2,
        Scroll::FullDown => st + ch,
        Scroll::FullUp => st - ch,
        Scroll::Top => 0,
        Scroll::Bottom => sh,
    };
    msgs.set_scroll_top(target);
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

/// First non-blank column of the line containing `cursor`.
fn first_non_blank(text: &[char], cursor: usize) -> usize {
    let ls = line_start(text, cursor);
    let le = line_end(text, cursor);
    let mut i = ls;
    while i < le && text[i].is_whitespace() {
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

/// Index of the last character of the current or next word (vim `e`).
fn end_of_word(text: &[char], cursor: usize) -> usize {
    let n = text.len();
    if n == 0 {
        return 0;
    }
    let mut i = cursor + 1;
    while i < n && text[i].is_whitespace() {
        i += 1;
    }
    if i >= n {
        return cursor.min(n - 1);
    }
    while i + 1 < n && !text[i + 1].is_whitespace() {
        i += 1;
    }
    i
}

/// Target index for a plain motion key, or `None` if `key` isn't a motion.
fn simple_motion_target(key: &str, text: &[char], cursor: usize) -> Option<usize> {
    Some(match key {
        "h" => cursor.saturating_sub(1),
        "l" => (cursor + 1).min(text.len()),
        "j" => line_down(text, cursor),
        "k" => line_up(text, cursor),
        "w" => next_word(text, cursor),
        "b" => prev_word(text, cursor),
        "e" => end_of_word(text, cursor),
        "0" => line_start(text, cursor),
        "$" => line_end(text, cursor),
        _ => return None,
    })
}

/// Charwise `[start, end)` range spanned by an operator motion (`dw`, `d$`, …).
fn operator_motion_range(key: &str, text: &[char], cursor: usize) -> Option<(usize, usize)> {
    let n = text.len();
    Some(match key {
        "w" => (cursor, next_word(text, cursor)),
        "b" => (prev_word(text, cursor), cursor),
        "e" => (cursor, (end_of_word(text, cursor) + 1).min(n)),
        "0" => (line_start(text, cursor), cursor),
        "$" => (cursor, line_end(text, cursor)),
        "h" => (cursor.saturating_sub(1), cursor),
        "l" => (cursor, (cursor + 1).min(n)),
        _ => return None,
    })
}

/// Text-object selectors and their delimiter geometry.
#[derive(Clone, Copy)]
enum TextObjectKind {
    Word,
    /// Distinct open/close delimiter pair.
    Delim(char, char),
    /// Symmetric quote (`"`, `'`, `` ` ``).
    Quote(char),
}

/// Map a text-object selector key to its kind. `b`/`B` alias the round/curly
/// pairs (vim convention).
fn text_object_kind(key: &str) -> Option<TextObjectKind> {
    Some(match key {
        "w" => TextObjectKind::Word,
        "(" | ")" | "b" => TextObjectKind::Delim('(', ')'),
        "{" | "}" | "B" => TextObjectKind::Delim('{', '}'),
        "[" | "]" => TextObjectKind::Delim('[', ']'),
        "<" | ">" => TextObjectKind::Delim('<', '>'),
        "\"" => TextObjectKind::Quote('"'),
        "'" => TextObjectKind::Quote('\''),
        "`" => TextObjectKind::Quote('`'),
        _ => return None,
    })
}

/// Compute the charwise `[start, end)` range for a text object. `around`
/// includes the delimiters / trailing whitespace; otherwise "inside" only.
fn text_object_range(
    text: &[char],
    cursor: usize,
    kind: TextObjectKind,
    around: bool,
) -> Option<(usize, usize)> {
    match kind {
        TextObjectKind::Word => Some(word_object(text, cursor, around)),
        TextObjectKind::Delim(open, close) => {
            let (op, cp) = find_pair(text, cursor, open, close)?;
            Some(if around { (op, cp + 1) } else { (op + 1, cp) })
        }
        TextObjectKind::Quote(q) => {
            let (a, b) = find_quote(text, cursor, q)?;
            Some(if around { (a, b + 1) } else { (a + 1, b) })
        }
    }
}

/// `iw`/`aw` range. Inner = the run (whitespace or non-whitespace) under the
/// cursor. Around = plus trailing whitespace, or leading whitespace if there's
/// no trailing.
fn word_object(text: &[char], cursor: usize, around: bool) -> (usize, usize) {
    let n = text.len();
    if n == 0 {
        return (0, 0);
    }
    let cur = cursor.min(n - 1);
    let ws = text[cur].is_whitespace();
    let mut start = cur;
    while start > 0 && text[start - 1].is_whitespace() == ws {
        start -= 1;
    }
    let mut last = cur;
    while last + 1 < n && text[last + 1].is_whitespace() == ws {
        last += 1;
    }
    let mut end = last + 1;
    if around {
        let mut te = end;
        while te < n && text[te].is_whitespace() {
            te += 1;
        }
        if te > end {
            end = te;
        } else {
            while start > 0 && text[start - 1].is_whitespace() {
                start -= 1;
            }
        }
    }
    (start, end)
}

/// Find the delimiter pair enclosing (or touching) `cursor`. Returns the
/// inclusive char indices of the matching open and close delimiters, honoring
/// nesting.
fn find_pair(text: &[char], cursor: usize, open: char, close: char) -> Option<(usize, usize)> {
    let n = text.len();
    if n == 0 {
        return None;
    }
    let cur = cursor.min(n - 1);
    let open_pos = if text[cur] == open {
        cur
    } else {
        // Walk left, matching nested pairs, to find the enclosing open.
        let mut depth = 0i32;
        let mut i = cur;
        let mut found = None;
        while i > 0 {
            i -= 1;
            if text[i] == close {
                depth += 1;
            } else if text[i] == open {
                if depth == 0 {
                    found = Some(i);
                    break;
                }
                depth -= 1;
            }
        }
        found?
    };
    // Walk right for the matching close.
    let mut depth = 0i32;
    let mut j = open_pos + 1;
    while j < n {
        if text[j] == open {
            depth += 1;
        } else if text[j] == close {
            if depth == 0 {
                return Some((open_pos, j));
            }
            depth -= 1;
        }
        j += 1;
    }
    None
}

/// Find the quote pair on the cursor's line. Quotes are matched sequentially
/// left-to-right; returns the first pair whose closing quote is at/after the
/// cursor (so the cursor is inside it, or it's the next quoted span).
fn find_quote(text: &[char], cursor: usize, q: char) -> Option<(usize, usize)> {
    let ls = line_start(text, cursor);
    let le = line_end(text, cursor);
    let positions: Vec<usize> = (ls..le).filter(|&i| text[i] == q).collect();
    for pair in positions.chunks(2) {
        if let [a, b] = *pair {
            if cursor <= b {
                return Some((a, b));
            }
        }
    }
    None
}

/// Remove `[start, end)`, returning the new buffer and the resting cursor
/// (clamped to the range start).
fn remove_range(text: &[char], start: usize, end: usize) -> (Vec<char>, usize) {
    let mut v = text.to_vec();
    let start = start.min(v.len());
    let end = end.min(v.len());
    if start < end {
        v.drain(start..end);
    }
    let c = start.min(v.len());
    (v, c)
}

/// Delete whole lines `[ls, le]` where `le` is the end (exclusive of the final
/// newline) of the last line to remove. Mirrors vim `dd`/`Ndd`.
fn delete_lines(text: &[char], ls: usize, le: usize) -> (Vec<char>, usize) {
    let mut v = text.to_vec();
    let le = le.min(v.len());
    if le < v.len() {
        // Include the trailing newline so the line fully disappears.
        v.drain(ls..=le);
    } else if ls > 0 {
        // Last line(s): also drop the newline that precedes the block.
        v.drain(ls - 1..le);
    } else {
        // Whole buffer: clear it.
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

/// Replace the char under the cursor (never a newline). Cursor stays.
fn replace_char(text: &[char], cursor: usize, ch: char) -> Vec<char> {
    let mut v = text.to_vec();
    if cursor < v.len() && v[cursor] != '\n' {
        v[cursor] = ch;
    }
    v
}

/// Toggle the case of the char under the cursor and advance one cell (clamped
/// to the line). Non-letters are left unchanged but still advance.
fn toggle_case(text: &[char], cursor: usize) -> (Vec<char>, usize) {
    let mut v = text.to_vec();
    if cursor < v.len() && v[cursor] != '\n' {
        let c = v[cursor];
        let swapped = if c.is_lowercase() {
            c.to_uppercase().next().unwrap_or(c)
        } else if c.is_uppercase() {
            c.to_lowercase().next().unwrap_or(c)
        } else {
            c
        };
        v[cursor] = swapped;
        let le = line_end(&v, cursor);
        let c = (cursor + 1).min(le);
        return (v, c);
    }
    (v, cursor)
}

/// Paste the register relative to the cursor. Charwise pastes after/before the
/// cursor cell; linewise inserts whole line(s) below/above. Returns the new
/// buffer and resting cursor.
fn paste(text: &[char], cursor: usize, reg: &Register, after: bool) -> (Vec<char>, usize) {
    if reg.text.is_empty() {
        return (text.to_vec(), cursor);
    }
    let ins: Vec<char> = reg.text.chars().collect();
    let mut v = text.to_vec();
    if reg.linewise {
        if after {
            let le = line_end(&v, cursor);
            let mut seq = vec!['\n'];
            seq.extend(ins.iter().copied());
            v.splice(le..le, seq);
            // The splice always lengthens the buffer, so `le + 1` (start of the
            // pasted line) is in range.
            (v, le + 1)
        } else {
            let ls = line_start(&v, cursor);
            let mut seq = ins.clone();
            seq.push('\n');
            v.splice(ls..ls, seq);
            (v, ls)
        }
    } else {
        let at = if after {
            (cursor + 1).min(v.len())
        } else {
            cursor
        };
        v.splice(at..at, ins.iter().copied());
        let c = (at + ins.len()).saturating_sub(1).max(at);
        (v, c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn s(v: &[char]) -> String {
        v.iter().collect()
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
    fn first_non_blank_skips_leading_ws() {
        let t = chars("  hi\nx");
        assert_eq!(first_non_blank(&t, 3), 2);
        // All-blank line clamps to line end.
        let t2 = chars("   ");
        assert_eq!(first_non_blank(&t2, 1), 3);
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
        assert_eq!(line_down(&t, 3), 7);
        assert_eq!(line_up(&t, 9), 6);
        assert_eq!(line_up(&t, 2), 2);
    }

    #[test]
    fn word_motions() {
        let t = chars("foo bar  baz");
        assert_eq!(next_word(&t, 0), 4);
        assert_eq!(next_word(&t, 4), 9);
        assert_eq!(prev_word(&t, 9), 4);
        assert_eq!(prev_word(&t, 4), 0);
    }

    #[test]
    fn end_of_word_motion() {
        let t = chars("foo bar");
        assert_eq!(end_of_word(&t, 0), 2); // -> last char of "foo"
        assert_eq!(end_of_word(&t, 2), 6); // -> last char of "bar"
        assert_eq!(end_of_word(&t, 6), 6); // at buffer end, stays
    }

    /// `x` deletes the range `[cursor, cursor+1)` via `remove_range` (the same
    /// path `handle_normal` takes).
    #[test]
    fn x_deletes_char_under_cursor() {
        let (v, c) = remove_range(&chars("abc"), 1, 2);
        assert_eq!(s(&v), "ac");
        assert_eq!(c, 1);
        // At end of buffer the range is empty -> no-op.
        let (v, c) = remove_range(&chars("ab"), 2, 3);
        assert_eq!(s(&v), "ab");
        assert_eq!(c, 2);
    }

    /// `dw` = operator-motion range over `w`, then `remove_range`.
    #[test]
    fn dw_deletes_to_next_word() {
        let t = chars("foo bar");
        let (start, end) = operator_motion_range("w", &t, 0).unwrap();
        let (v, c) = remove_range(&t, start, end);
        assert_eq!(s(&v), "bar");
        assert_eq!(c, 0);
    }

    /// `dd` on a middle line drops the line and its trailing newline.
    #[test]
    fn dd_deletes_middle_line_with_trailing_newline() {
        let t = chars("a\nb\nc");
        let ls = line_start(&t, 2);
        let le = line_end(&t, 2);
        let (v, c) = delete_lines(&t, ls, le);
        assert_eq!(s(&v), "a\nc");
        assert_eq!(c, 2);
    }

    #[test]
    fn dd_deletes_last_line_with_preceding_newline() {
        let t = chars("a\nb");
        let ls = line_start(&t, 2);
        let le = line_end(&t, 2);
        let (v, _c) = delete_lines(&t, ls, le);
        assert_eq!(s(&v), "a");
    }

    #[test]
    fn dd_clears_only_line() {
        let t = chars("abc");
        let (v, c) = delete_lines(&t, line_start(&t, 1), line_end(&t, 1));
        assert_eq!(s(&v), "");
        assert_eq!(c, 0);
    }

    #[test]
    fn delete_multiple_lines() {
        let t = chars("a\nb\nc\nd");
        // Delete 2 lines starting at line 0 (ls=0), le = end of 2nd line (=3).
        let ls = line_start(&t, 0);
        let mut le = line_end(&t, 0);
        le = line_end(&t, le + 1);
        let (v, c) = delete_lines(&t, ls, le);
        assert_eq!(s(&v), "c\nd");
        assert_eq!(c, 0);
    }

    #[test]
    fn o_and_upper_o_insert_newlines() {
        let (v, c) = open_line_below(&chars("ab\ncd"), 1);
        assert_eq!(s(&v), "ab\n\ncd");
        assert_eq!(c, 3);
        let (v, c) = open_line_above(&chars("ab\ncd"), 4);
        assert_eq!(s(&v), "ab\n\ncd");
        assert_eq!(c, 3);
    }

    // --- text objects: word ---

    #[test]
    fn inner_word_selects_run_under_cursor() {
        let t = chars("foo bar baz");
        assert_eq!(word_object(&t, 5, false), (4, 7)); // "bar"
                                                       // on whitespace -> the whitespace run
        assert_eq!(word_object(&t, 3, false), (3, 4));
    }

    #[test]
    fn around_word_includes_trailing_whitespace() {
        let t = chars("foo bar baz");
        // aw on "bar" includes the trailing space -> [4,8)
        assert_eq!(word_object(&t, 5, true), (4, 8));
    }

    #[test]
    fn around_word_includes_leading_ws_when_no_trailing() {
        let t = chars("foo bar");
        // aw on "bar" (no trailing ws) grabs the leading space -> [3,7)
        assert_eq!(word_object(&t, 5, true), (3, 7));
    }

    #[test]
    fn text_object_word_via_range() {
        let t = chars("foo bar");
        assert_eq!(
            text_object_range(&t, 5, TextObjectKind::Word, false),
            Some((4, 7))
        );
    }

    // --- text objects: delimiters ---

    #[test]
    fn find_pair_parens_simple() {
        let t = chars("a(bc)d");
        assert_eq!(find_pair(&t, 3, '(', ')'), Some((1, 4)));
        // cursor on the open delimiter
        assert_eq!(find_pair(&t, 1, '(', ')'), Some((1, 4)));
        // cursor on the close delimiter
        assert_eq!(find_pair(&t, 4, '(', ')'), Some((1, 4)));
    }

    #[test]
    fn find_pair_respects_nesting() {
        let t = chars("(a(b)c)");
        // inner cursor at 'b' (index 3) -> innermost pair (2,4)
        assert_eq!(find_pair(&t, 3, '(', ')'), Some((2, 4)));
        // cursor at 'a' (index 1) -> outer pair (0,6)
        assert_eq!(find_pair(&t, 1, '(', ')'), Some((0, 6)));
    }

    #[test]
    fn inside_vs_around_delimiters() {
        let t = chars("x{ab}y");
        // i{ -> "ab" = [2,4)
        assert_eq!(
            text_object_range(&t, 3, TextObjectKind::Delim('{', '}'), false),
            Some((2, 4))
        );
        // a{ -> "{ab}" = [1,5)
        assert_eq!(
            text_object_range(&t, 3, TextObjectKind::Delim('{', '}'), true),
            Some((1, 5))
        );
    }

    #[test]
    fn brackets_and_angles() {
        let t = chars("[hi] <yo>");
        assert_eq!(find_pair(&t, 2, '[', ']'), Some((0, 3)));
        assert_eq!(find_pair(&t, 6, '<', '>'), Some((5, 8)));
    }

    #[test]
    fn delimiter_aliases_b_and_upper_b() {
        assert!(matches!(
            text_object_kind("b"),
            Some(TextObjectKind::Delim('(', ')'))
        ));
        assert!(matches!(
            text_object_kind("B"),
            Some(TextObjectKind::Delim('{', '}'))
        ));
    }

    // --- text objects: quotes ---

    #[test]
    fn find_quote_on_line() {
        let t = chars("say \"hi there\" ok");
        // cursor inside the quotes
        assert_eq!(find_quote(&t, 7, '"'), Some((4, 13)));
    }

    #[test]
    fn inside_vs_around_quotes() {
        let t = chars("a'bc'd");
        assert_eq!(
            text_object_range(&t, 2, TextObjectKind::Quote('\''), false),
            Some((2, 4))
        );
        assert_eq!(
            text_object_range(&t, 2, TextObjectKind::Quote('\''), true),
            Some((1, 5))
        );
    }

    #[test]
    fn quote_before_cursor_picks_next_pair() {
        let t = chars("x \"y\"");
        // cursor before the quotes -> next pair
        assert_eq!(find_quote(&t, 0, '"'), Some((2, 4)));
    }

    // --- operators over ranges (remove_range mirrors edit_range's math) ---

    #[test]
    fn remove_range_deletes_and_rests_cursor_at_start() {
        let (v, c) = remove_range(&chars("hello world"), 0, 6);
        assert_eq!(s(&v), "world");
        assert_eq!(c, 0);
    }

    #[test]
    fn diw_removes_inner_word() {
        let t = chars("foo bar baz");
        let (start, end) = text_object_range(&t, 5, TextObjectKind::Word, false).unwrap();
        let (v, c) = remove_range(&t, start, end);
        assert_eq!(s(&v), "foo  baz");
        assert_eq!(c, 4);
    }

    #[test]
    fn ci_paren_removes_inner() {
        let t = chars("f(arg)");
        let (start, end) =
            text_object_range(&t, 3, TextObjectKind::Delim('(', ')'), false).unwrap();
        let (v, c) = remove_range(&t, start, end);
        assert_eq!(s(&v), "f()");
        assert_eq!(c, 2);
    }

    #[test]
    fn operator_motion_ranges() {
        let t = chars("foo bar");
        assert_eq!(operator_motion_range("w", &t, 0), Some((0, 4)));
        assert_eq!(operator_motion_range("$", &t, 0), Some((0, 7)));
        assert_eq!(operator_motion_range("e", &t, 0), Some((0, 3)));
    }

    // --- replace / toggle-case ---

    #[test]
    fn replace_char_swaps_one() {
        let v = replace_char(&chars("cat"), 0, 'b');
        assert_eq!(s(&v), "bat");
        // never replaces a newline
        let v = replace_char(&chars("a\nb"), 1, 'x');
        assert_eq!(s(&v), "a\nb");
    }

    #[test]
    fn toggle_case_advances() {
        let (v, c) = toggle_case(&chars("aBc"), 0);
        assert_eq!(s(&v), "ABc");
        assert_eq!(c, 1);
        let (v, c) = toggle_case(&chars("aBc"), 1);
        assert_eq!(s(&v), "abc");
        assert_eq!(c, 2);
    }

    #[test]
    fn toggle_case_at_line_end_stays() {
        let (v, c) = toggle_case(&chars("ab"), 1);
        assert_eq!(s(&v), "aB");
        assert_eq!(c, 2); // advanced to line end
    }

    // --- paste ---

    #[test]
    fn charwise_paste_after_and_before() {
        let reg = Register {
            text: "XY".into(),
            linewise: false,
        };
        let (v, c) = paste(&chars("ab"), 0, &reg, true);
        assert_eq!(s(&v), "aXYb");
        assert_eq!(c, 2); // on last pasted char 'Y'
        let (v, c) = paste(&chars("ab"), 0, &reg, false);
        assert_eq!(s(&v), "XYab");
        assert_eq!(c, 1);
    }

    #[test]
    fn linewise_paste_below_and_above() {
        let reg = Register {
            text: "new".into(),
            linewise: true,
        };
        let (v, c) = paste(&chars("a\nb"), 0, &reg, true);
        assert_eq!(s(&v), "a\nnew\nb");
        assert_eq!(c, 2); // start of pasted line
        let (v, c) = paste(&chars("a\nb"), 0, &reg, false);
        assert_eq!(s(&v), "new\na\nb");
        assert_eq!(c, 0);
    }

    #[test]
    fn empty_register_paste_is_noop() {
        let reg = Register::default();
        let (v, c) = paste(&chars("ab"), 1, &reg, true);
        assert_eq!(s(&v), "ab");
        assert_eq!(c, 1);
    }

    // --- undo stack ---

    #[test]
    fn undo_stack_push_pop_lifo() {
        let mut st = VimState::default();
        st.push_undo("one", 0);
        st.push_undo("two", 1);
        assert_eq!(st.pop_undo(), Some(("two".to_string(), 1)));
        assert_eq!(st.pop_undo(), Some(("one".to_string(), 0)));
        assert_eq!(st.pop_undo(), None);
    }

    #[test]
    fn undo_stack_is_bounded() {
        let mut st = VimState::default();
        for i in 0..(UNDO_LIMIT + 10) {
            st.push_undo(&format!("{i}"), i);
        }
        assert_eq!(st.undo.len(), UNDO_LIMIT);
        // Oldest entries were dropped; the most recent survives on top.
        assert_eq!(
            st.pop_undo(),
            Some(((UNDO_LIMIT + 9).to_string(), UNDO_LIMIT + 9))
        );
    }

    #[test]
    fn count_accumulation_via_take() {
        let mut st = VimState::default();
        st.count = Some(2);
        assert_eq!(st.take_count(), 2);
        assert_eq!(st.take_count(), 1); // default after clear
    }

    #[test]
    fn utf16_roundtrip_with_astral() {
        let t = chars("a😀b");
        assert_eq!(char_idx_to_utf16(&t, 0), 0);
        assert_eq!(char_idx_to_utf16(&t, 1), 1);
        assert_eq!(char_idx_to_utf16(&t, 2), 3);
        assert_eq!(char_idx_to_utf16(&t, 3), 4);
        assert_eq!(utf16_to_char_idx(&t, 3), 2);
        assert_eq!(utf16_to_char_idx(&t, 1), 1);
    }
}
