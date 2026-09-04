use super::ansi::render_ansi;
use super::markdown::linkify_urls;
use shared::fmt::truncate_str;
use yew::prelude::*;

/// Canonical truncation limits per context — one table to tune instead of
/// grepping for literal `max_len={500}` etc. (see #1677).
pub mod limits {
    /// Tool/command output (bash, file reads with line numbers, etc.)
    pub const TOOL_OUTPUT: usize = 500;
    /// Short prose / user prompts
    pub const PROSE: usize = 300;
    /// Raw JSON fallback (unrecognized event and structured payload dumps)
    pub const RAW_JSON: usize = 800;
    /// Collab agent prompt (prose input, preserved at 500 to avoid truncating prompts)
    pub const AGENT_PROMPT: usize = 500;
    /// Compact WebFetch prompt preview shown beside the fetched URL
    pub const WEBFETCH_PROMPT: usize = 100;
}

#[derive(Properties, PartialEq)]
pub struct ExpandableTextProps {
    pub full_text: AttrValue,
    pub max_len: usize,
    /// Wrapper element tag: "pre", "div", or "span"
    #[prop_or("pre".into())]
    pub tag: AttrValue,
    #[prop_or_default]
    pub class: Classes,
    /// Render the text as terminal output: parse ANSI SGR escapes into styled
    /// spans (#1496). Off by default — only tool/command output opts in, so
    /// ordinary text keeps plain linkified rendering. When on, URL auto-linking
    /// still applies within each styled run.
    #[prop_or_default]
    pub ansi: bool,
}

/// Render `text` as either ANSI-styled terminal output or plain linkified text,
/// per the `ansi` flag. Both escape output content (Yew escapes text nodes;
/// the ANSI path only adds spans with a fixed style vocabulary).
fn render_body(text: &str, ansi: bool) -> Html {
    if ansi {
        render_ansi(text)
    } else {
        linkify_urls(text)
    }
}

/// Split `text` into the shown preview and the hidden remainder, cutting on a
/// **line boundary** at or before `max_len`.
///
/// Collapsing is about vertical space, so a cut mid-line just leaves a ragged
/// half-line that costs the same row it was meant to save. Falls back to the
/// character cut when there is no newline to land on — a single long line still
/// has to be truncated somewhere.
fn split_for_preview(text: &str, max_len: usize) -> (&str, &str) {
    let cut = truncate_str(text, max_len).len();
    match text[..cut].rfind('\n') {
        // Never yield an empty preview: a leading newline inside the budget
        // would otherwise show nothing but the toggle.
        Some(0) | None => text.split_at(cut),
        Some(i) => (&text[..i], &text[i + 1..]),
    }
}

/// Label for the collapsed toggle, describing what staying collapsed is hiding.
///
/// Reports **lines as well as characters**, because the character count alone
/// doesn't tell you what expanding will cost: 4000 hidden characters is two
/// screens of build log or one minified JSON line, and those want different
/// decisions. The line count is omitted when there is only one line to hide —
/// its absence then carries the same information, and it keeps the label short
/// for the inline `span` chips in tool headers, which must stay on one line.
fn truncation_summary(hidden: &str) -> String {
    let lines = hidden.lines().count();
    if lines > 1 {
        format!("... {} more lines, {} chars", lines, hidden.len())
    } else {
        format!("... {} more chars", hidden.len())
    }
}

/// Character-based expandable text. Shows truncated content with a clickable
/// toggle to reveal the full text. If the text fits within `max_len`, renders
/// as-is with no toggle.
#[function_component(ExpandableText)]
pub fn expandable_text(props: &ExpandableTextProps) -> Html {
    let expanded = use_state(|| false);
    let text = &*props.full_text;

    if text.len() <= props.max_len {
        return match props.tag.as_str() {
            "span" => {
                html! { <span class={props.class.clone()}>{ render_body(text, props.ansi) }</span> }
            }
            "div" => {
                html! { <div class={props.class.clone()}>{ render_body(text, props.ansi) }</div> }
            }
            _ => html! { <pre class={props.class.clone()}>{ render_body(text, props.ansi) }</pre> },
        };
    }

    let toggle = {
        let expanded = expanded.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            expanded.set(!*expanded);
        })
    };

    let (display, toggle_label) = if *expanded {
        (text.to_string(), "show less".to_string())
    } else {
        let (shown, hidden) = split_for_preview(text, props.max_len);
        (shown.to_string(), truncation_summary(hidden))
    };

    // Block-context wrappers (`pre`, `div`) render the toggle as a `<div>` so
    // it sits on its own line at the bottom of the output rather than
    // butting up against the truncated content. The inline `span` wrapper
    // keeps the toggle inline — that path is used only for the MCP
    // key=value chips in tool headers, which must stay on one line.
    match props.tag.as_str() {
        "span" => html! {
            <span class={props.class.clone()}>
                { render_body(&display, props.ansi) }
                <span class="expandable-toggle" onclick={toggle}>{ toggle_label }</span>
            </span>
        },
        "div" => html! {
            <div class={props.class.clone()}>
                { render_body(&display, props.ansi) }
                <div class="expandable-toggle" onclick={toggle}>{ toggle_label }</div>
            </div>
        },
        // The toggle sits OUTSIDE the `pre`: several `pre` consumers are
        // scroll containers (`.tool-result-content` caps at 200px,
        // `.write-content` at 400px), and a toggle inside one lands at the
        // bottom of the scroll window — half-clipped at the box edge and
        // reachable only by noticing a thin scrollbar. Outside, it is always
        // fully visible and clickable regardless of scroll position.
        _ => html! {
            <div class="expandable-block">
                <pre class={props.class.clone()}>
                    { render_body(&display, props.ansi) }
                </pre>
                <div class="expandable-toggle" onclick={toggle}>{ toggle_label }</div>
            </div>
        },
    }
}

#[derive(Properties, PartialEq)]
pub struct ExpandableLinesProps {
    pub content: AttrValue,
    pub max_lines: usize,
    #[prop_or_default]
    pub class: Classes,
}

/// Line-based expandable content for file previews. Shows the first N lines
/// with a clickable toggle to reveal all lines.
#[function_component(ExpandableLines)]
pub fn expandable_lines(props: &ExpandableLinesProps) -> Html {
    let expanded = use_state(|| false);
    let content = &*props.content;
    let all_lines: Vec<&str> = content.lines().collect();
    let total = all_lines.len();

    if total <= props.max_lines {
        return html! {
            <pre class={classes!(props.class.clone(), "write-content")}>
                { for all_lines.iter().enumerate().map(|(i, line)| html! {
                    <div class="write-line">
                        <span class="line-number">{ format!("{:>4}", i + 1) }</span>
                        <span class="line-content">{ linkify_urls(line) }</span>
                    </div>
                })}
            </pre>
        };
    }

    let toggle = {
        let expanded = expanded.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            expanded.set(!*expanded);
        })
    };

    let visible = if *expanded {
        &all_lines[..]
    } else {
        &all_lines[..props.max_lines]
    };
    let remaining = total - props.max_lines;

    // Toggle outside the (scrollable) `pre` — see the ExpandableText `pre`
    // arm for why.
    html! {
        <div class="expandable-block">
            <pre class={classes!(props.class.clone(), "write-content")}>
                { for visible.iter().enumerate().map(|(i, line)| html! {
                    <div class="write-line">
                        <span class="line-number">{ format!("{:>4}", i + 1) }</span>
                        <span class="line-content">{ linkify_urls(line) }</span>
                    </div>
                })}
            </pre>
            <div class="write-truncated expandable-toggle" onclick={toggle}>
                { if *expanded {
                    "show less".to_string()
                } else {
                    format!("... {} more lines", remaining)
                }}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collapsing exists to save vertical space, so the cut lands on a line
    /// boundary rather than leaving a ragged half-line behind.
    #[test]
    fn preview_cuts_on_a_line_boundary() {
        let text = "alpha\nbravo\ncharlie\ndelta";
        // 14 lands inside "charlie"; back off to the end of "bravo".
        let (shown, hidden) = split_for_preview(text, 14);
        assert_eq!(shown, "alpha\nbravo");
        assert_eq!(hidden, "charlie\ndelta");
    }

    /// One long line has no boundary to land on, so it still cuts mid-line —
    /// otherwise the preview would be empty.
    #[test]
    fn a_single_long_line_still_truncates() {
        let text = "no newlines here at all";
        let (shown, hidden) = split_for_preview(text, 10);
        assert_eq!(shown, "no newline");
        assert_eq!(hidden, "s here at all");
    }

    /// The case the line count exists for: a wall of build-log output, where
    /// the character count alone doesn't convey how much scrolling is hidden.
    #[test]
    fn reports_lines_and_chars_for_multi_line_remainders() {
        assert_eq!(
            truncation_summary("error one\nerror two\nerror three"),
            "... 3 more lines, 31 chars"
        );
    }

    /// A single hidden line stays chars-only: "1 more lines" reads badly, and
    /// the absence of a line count already says the remainder is one line.
    #[test]
    fn omits_the_line_count_for_single_line_remainders() {
        assert_eq!(truncation_summary("{\"a\":1}"), "... 7 more chars");
        assert_eq!(truncation_summary(""), "... 0 more chars");
    }

    /// A trailing newline must not be counted as a further line of content.
    #[test]
    fn does_not_count_a_trailing_newline_as_a_line() {
        assert_eq!(truncation_summary("one\n"), "... 4 more chars");
        assert_eq!(
            truncation_summary("one\ntwo\n"),
            "... 2 more lines, 8 chars"
        );
    }

    /// Multi-byte input: the summary must count characters the same way the
    /// truncation does (bytes), and never panic on a split boundary.
    #[test]
    fn handles_multi_byte_remainders() {
        assert_eq!(truncation_summary("é"), "... 2 more chars");
    }
}
