//! ANSI SGR → styled HTML for terminal/tool output (#1496).
//!
//! CLIs (cargo, git, test runners, ripgrep, eza, …) emit SGR escape sequences
//! to color and style their output. Rendered raw those show as literal escape
//! bytes — noise that also loses the severity signal the color carried. This
//! module parses the common SGR set and renders it as styled `<span>`s, with
//! colors mapped to the portal's Tokyo-Night palette so they read on the dark
//! surface instead of using raw terminal RGB.
//!
//! Shared by both agents' tool-output renderers (via [`crate::components::expandable`]),
//! so Claude `tool_result` text and Codex `command_execution` output style
//! identically.
//!
//! Safety: the parser only ever emits *text* (as Yew text nodes, which Yew
//! escapes) wrapped in spans whose `style` is composed from a fixed vocabulary
//! of our own hex colors and CSS keywords — output bytes never become markup.
//!
//! Non-SGR escapes (cursor moves, clear-line, OSC title sets, …) are stripped.
//! Carriage-return progress-bar redraws are collapsed to their final state per
//! line, so `\r`-spam degrades to just the last frame rather than a wall of
//! half-drawn lines.

use super::markdown::linkify_urls;
use yew::prelude::*;

/// A color drawn from an SGR sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnsiColor {
    /// One of the 16 standard/bright slots (0–15).
    Standard(u8),
    /// An xterm-256 palette index (16–255; 0–15 fold into `Standard`).
    Indexed(u8),
    /// A 24-bit truecolor value.
    Rgb(u8, u8, u8),
}

impl AnsiColor {
    /// Hex string for the CSS `color` / `background-color` value.
    fn to_hex(self) -> String {
        match self {
            AnsiColor::Standard(i) => STANDARD_HEX[(i & 0x0f) as usize].to_string(),
            AnsiColor::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            AnsiColor::Indexed(n) => indexed_to_hex(n),
        }
    }
}

/// The 16 standard SGR colors, mapped to Tokyo-Night terminal hues so they read
/// on the `#1a1b26` surface. Index 0–7 standard, 8–15 bright.
const STANDARD_HEX: [&str; 16] = [
    "#414868", // 0 black
    "#f7768e", // 1 red
    "#9ece6a", // 2 green
    "#e0af68", // 3 yellow
    "#7aa2f7", // 4 blue
    "#bb9af7", // 5 magenta
    "#7dcfff", // 6 cyan
    "#a9b1d6", // 7 white
    "#565f89", // 8 bright black (grey)
    "#ff899d", // 9 bright red
    "#9fe044", // 10 bright green
    "#faba4a", // 11 bright yellow
    "#8db0ff", // 12 bright blue
    "#c7a9ff", // 13 bright magenta
    "#a4daff", // 14 bright cyan
    "#c0caf5", // 15 bright white
];

/// Resolve an xterm-256 index to a hex string: 0–15 the standard slots, 16–231
/// the 6×6×6 color cube, 232–255 the 24-step grayscale ramp.
fn indexed_to_hex(n: u8) -> String {
    if n < 16 {
        return STANDARD_HEX[n as usize].to_string();
    }
    if n >= 232 {
        let v = 8 + (n as u16 - 232) * 10; // 8, 18, …, 238
        let v = v as u8;
        return format!("#{v:02x}{v:02x}{v:02x}");
    }
    // 6×6×6 cube. Each axis level maps 0→0, else 55 + 40·level.
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let c = n - 16;
    let r = LEVELS[(c / 36) as usize];
    let g = LEVELS[((c / 6) % 6) as usize];
    let b = LEVELS[(c % 6) as usize];
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Accumulated SGR state. Default = no styling (renders as a bare text node).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Style {
    fg: Option<AnsiColor>,
    bg: Option<AnsiColor>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl Style {
    fn is_default(&self) -> bool {
        *self == Style::default()
    }

    /// Inline CSS for this style, or `None` when default (no span needed).
    fn css(&self) -> Option<String> {
        if self.is_default() {
            return None;
        }
        let mut s = String::new();
        if let Some(c) = self.fg {
            s.push_str(&format!("color:{};", c.to_hex()));
        }
        if let Some(c) = self.bg {
            s.push_str(&format!("background-color:{};", c.to_hex()));
        }
        if self.bold {
            s.push_str("font-weight:600;");
        }
        if self.dim {
            s.push_str("opacity:0.7;");
        }
        if self.italic {
            s.push_str("font-style:italic;");
        }
        if self.underline {
            s.push_str("text-decoration:underline;");
        }
        Some(s)
    }
}

/// A run of text sharing one style.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    text: String,
    style: Style,
}

/// Apply one SGR parameter list (the bytes between `ESC[` and `m`) to `style`.
///
/// Handles reset, bold/dim/italic/underline (and their un-set counterparts),
/// the 8 standard + 8 bright fg/bg colors, and the extended `38;5;n` /
/// `38;2;r;g;b` (and `48;…` bg) forms. Unknown codes are ignored.
fn apply_sgr(style: &mut Style, params: &str) {
    // An empty parameter string (`ESC[m`) means reset, same as `0`.
    let codes: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split(';')
            .map(|p| p.parse::<u16>().unwrap_or(0))
            .collect()
    };
    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            0 => *style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            30..=37 => style.fg = Some(AnsiColor::Standard((codes[i] - 30) as u8)),
            39 => style.fg = None,
            40..=47 => style.bg = Some(AnsiColor::Standard((codes[i] - 40) as u8)),
            49 => style.bg = None,
            90..=97 => style.fg = Some(AnsiColor::Standard((codes[i] - 90 + 8) as u8)),
            100..=107 => style.bg = Some(AnsiColor::Standard((codes[i] - 100 + 8) as u8)),
            38 | 48 => {
                let is_fg = codes[i] == 38;
                // 38;5;n  or  38;2;r;g;b — consume the sub-params.
                let color = match codes.get(i + 1) {
                    Some(5) => {
                        let c = codes.get(i + 2).map(|n| AnsiColor::Indexed(*n as u8));
                        i += 2;
                        c
                    }
                    Some(2) => {
                        let r = codes.get(i + 2).copied().unwrap_or(0) as u8;
                        let g = codes.get(i + 3).copied().unwrap_or(0) as u8;
                        let b = codes.get(i + 4).copied().unwrap_or(0) as u8;
                        i += 4;
                        Some(AnsiColor::Rgb(r, g, b))
                    }
                    // Malformed extended color — skip the introducer only.
                    _ => None,
                };
                if let Some(c) = color {
                    if is_fg {
                        style.fg = Some(c);
                    } else {
                        style.bg = Some(c);
                    }
                }
            }
            _ => {} // unknown / unsupported SGR code
        }
        i += 1;
    }
}

/// Collapse carriage-return line redraws: within each `\n`-delimited line, keep
/// only the text after the final `\r`. This turns progress-bar spam (which
/// overwrites one line via `\r`) into just its last frame, matching what a
/// terminal would show, instead of a stack of half-drawn lines.
fn collapse_carriage_returns(raw: &str) -> std::borrow::Cow<'_, str> {
    if !raw.contains('\r') {
        return std::borrow::Cow::Borrowed(raw);
    }
    let collapsed = raw
        .split('\n')
        .map(|line| line.rsplit('\r').next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    std::borrow::Cow::Owned(collapsed)
}

/// Parse `raw` into styled segments. Pure (no DOM), so it is unit-testable.
fn parse(raw: &str) -> Vec<Segment> {
    let raw = collapse_carriage_returns(raw);
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut segments: Vec<Segment> = Vec::new();
    let mut style = Style::default();
    let mut cur = String::new();
    let mut i = 0;

    // ESC (0x1b) never appears inside a UTF-8 multibyte sequence (continuation
    // and lead bytes are all ≥ 0x80), so slicing at ESC boundaries is UTF-8
    // safe. Same for the CSI/OSC terminator bytes, which are all ASCII.
    while i < len {
        if bytes[i] != 0x1b {
            let start = i;
            while i < len && bytes[i] != 0x1b {
                i += 1;
            }
            cur.push_str(&raw[start..i]);
            continue;
        }

        // Hit an escape. We flush the accumulated run into a segment ONLY when
        // the escape actually changes the style (an SGR sequence); stripped
        // sequences (cursor moves, OSC, …) just advance past the bytes and let
        // the surrounding text keep accumulating into one run — so text on
        // either side of a stripped escape coalesces instead of fragmenting.
        match bytes.get(i + 1) {
            // CSI: ESC [ params … final(0x40–0x7e). Only `m` is SGR; strip the
            // rest (cursor moves, clear-line, etc.).
            Some(b'[') => {
                let mut j = i + 2;
                while j < len && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                if j >= len {
                    i = len; // incomplete sequence at end of buffer — drop it
                } else {
                    if bytes[j] == b'm' {
                        if !cur.is_empty() {
                            segments.push(Segment {
                                text: std::mem::take(&mut cur),
                                style,
                            });
                        }
                        apply_sgr(&mut style, &raw[i + 2..j]);
                    }
                    i = j + 1;
                }
            }
            // OSC: ESC ] … terminated by BEL (0x07) or ST (ESC \). Stripped.
            Some(b']') => {
                let mut j = i + 2;
                while j < len
                    && bytes[j] != 0x07
                    && !(bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\'))
                {
                    j += 1;
                }
                i = if j >= len {
                    len
                } else if bytes[j] == 0x07 {
                    j + 1
                } else {
                    j + 2 // ST is two bytes
                };
            }
            // Lone ESC or a two-byte escape (ESC c, etc.) — skip both bytes.
            _ => i = (i + 2).min(len),
        }
    }

    if !cur.is_empty() {
        segments.push(Segment { text: cur, style });
    }
    segments
}

/// Render terminal output with ANSI SGR styling applied.
///
/// Default-styled runs render as bare (linkified) text; styled runs get a
/// `<span style="…">`. URL auto-linking is preserved within every run.
pub fn render_ansi(raw: &str) -> Html {
    let segments = parse(raw);
    html! {
        <>
            { for segments.into_iter().map(|seg| match seg.style.css() {
                Some(css) => html! { <span style={css}>{ linkify_urls(&seg.text) }</span> },
                None => linkify_urls(&seg.text),
            }) }
        </>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(raw: &str) -> Vec<Segment> {
        parse(raw)
    }

    #[test]
    fn plain_text_is_one_default_segment() {
        let segs = styled("just plain output\nwith two lines");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].style.is_default());
        assert_eq!(segs[0].text, "just plain output\nwith two lines");
    }

    #[test]
    fn no_escapes_means_no_spans_rendered() {
        // A string with no SGR must produce a single default segment, so
        // render_ansi emits a bare text node (no wrapping span) — identical to
        // the old plain path.
        let segs = styled("error: something failed");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].style.css().is_none());
    }

    #[test]
    fn basic_foreground_color() {
        // "\x1b[31mred\x1b[0m normal"
        let segs = styled("\x1b[31mred\x1b[0m normal");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "red");
        assert_eq!(segs[0].style.fg, Some(AnsiColor::Standard(1)));
        assert_eq!(segs[1].text, " normal");
        assert!(segs[1].style.is_default());
        // Red maps to the Tokyo-Night hue, not raw #ff0000.
        assert_eq!(segs[0].style.css().as_deref(), Some("color:#f7768e;"));
    }

    #[test]
    fn compound_codes_bold_and_color() {
        let segs = styled("\x1b[1;32mOK\x1b[0m");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].style.bold);
        assert_eq!(segs[0].style.fg, Some(AnsiColor::Standard(2)));
        assert_eq!(
            segs[0].style.css().as_deref(),
            Some("color:#9ece6a;font-weight:600;")
        );
    }

    #[test]
    fn bright_colors_and_background() {
        // bright red fg (91) + blue bg (44)
        let segs = styled("\x1b[91;44mx\x1b[0m");
        assert_eq!(segs[0].style.fg, Some(AnsiColor::Standard(9)));
        assert_eq!(segs[0].style.bg, Some(AnsiColor::Standard(4)));
    }

    #[test]
    fn empty_sgr_is_reset() {
        // ESC[m with no params resets, same as ESC[0m.
        let segs = styled("\x1b[31mred\x1b[mplain");
        assert_eq!(segs.len(), 2);
        assert!(segs[1].style.is_default());
        assert_eq!(segs[1].text, "plain");
    }

    #[test]
    fn intensity_and_style_reset_codes() {
        let mut st = Style::default();
        apply_sgr(&mut st, "1;2;3;4");
        assert!(st.bold && st.dim && st.italic && st.underline);
        apply_sgr(&mut st, "22"); // normal intensity clears bold+dim
        assert!(!st.bold && !st.dim && st.italic && st.underline);
        apply_sgr(&mut st, "23;24");
        assert!(!st.italic && !st.underline);
    }

    #[test]
    fn xterm_256_color() {
        // 38;5;196 is a bright red in the cube; 38;5;2 folds to standard green.
        let segs = styled("\x1b[38;5;196mA\x1b[38;5;2mB\x1b[0m");
        assert_eq!(segs[0].style.fg, Some(AnsiColor::Indexed(196)));
        assert_eq!(segs[1].style.fg, Some(AnsiColor::Indexed(2)));
        // 196 = cube (5,0,0) → #ff0000; index 2 folds to Tokyo-Night green.
        assert_eq!(segs[0].style.css().as_deref(), Some("color:#ff0000;"));
        assert_eq!(segs[1].style.css().as_deref(), Some("color:#9ece6a;"));
    }

    #[test]
    fn truecolor() {
        let segs = styled("\x1b[38;2;122;162;247mblue\x1b[0m");
        assert_eq!(segs[0].style.fg, Some(AnsiColor::Rgb(122, 162, 247)));
        assert_eq!(segs[0].style.css().as_deref(), Some("color:#7aa2f7;"));
    }

    #[test]
    fn grayscale_ramp_index() {
        // 232 = darkest gray (#080808), 255 = lightest (#eeeeee).
        assert_eq!(indexed_to_hex(232), "#080808");
        assert_eq!(indexed_to_hex(255), "#eeeeee");
    }

    #[test]
    fn non_sgr_csi_is_stripped() {
        // Cursor move (H) and clear-line (K) carry no style and must vanish,
        // leaving only their surrounding text.
        let segs = styled("a\x1b[2Kb\x1b[Hc");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "abc");
    }

    #[test]
    fn osc_sequence_is_stripped() {
        // Window-title set: ESC ] 0 ; title BEL — dropped entirely.
        let segs = styled("before\x1b]0;my title\x07after");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "beforeafter");
    }

    #[test]
    fn carriage_return_progress_collapses_to_last_frame() {
        // A progress bar redraws one line via \r; only the final frame survives.
        let segs = styled("10%\r55%\r100% done\nnext line");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "100% done\nnext line");
    }

    #[test]
    fn incomplete_escape_at_end_is_dropped_not_leaked() {
        // A truncated escape (e.g. output cut mid-sequence) must not leak raw
        // bytes into the rendered text.
        let segs = styled("ok\x1b[");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "ok");
    }

    #[test]
    fn utf8_text_survives_around_escapes() {
        let segs = styled("\x1b[32m✓ passé\x1b[0m — café");
        assert_eq!(segs[0].text, "✓ passé");
        assert_eq!(segs[1].text, " — café");
    }

    #[test]
    fn style_persists_across_text_until_reset() {
        // A color set once applies to following text through a newline until an
        // explicit reset — matches terminal behavior.
        let segs = styled("\x1b[33mline1\nline2\x1b[0m tail");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "line1\nline2");
        assert_eq!(segs[0].style.fg, Some(AnsiColor::Standard(3)));
        assert_eq!(segs[1].text, " tail");
    }
}
