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
        // Measure the hidden remainder from where the text was actually cut:
        // `truncate_str` backs off to a char boundary, so `max_len` is an upper
        // bound on the cut, not the cut itself.
        let shown = truncate_str(text, props.max_len);
        let summary = truncation_summary(&text[shown.len()..]);
        (shown.to_string(), summary)
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
        _ => html! {
            <pre class={props.class.clone()}>
                { render_body(&display, props.ansi) }
                <div class="expandable-toggle" onclick={toggle}>{ toggle_label }</div>
            </pre>
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

    html! {
        <pre class={classes!(props.class.clone(), "write-content")}>
            { for visible.iter().enumerate().map(|(i, line)| html! {
                <div class="write-line">
                    <span class="line-number">{ format!("{:>4}", i + 1) }</span>
                    <span class="line-content">{ linkify_urls(line) }</span>
                </div>
            })}
            <div class="write-truncated expandable-toggle" onclick={toggle}>
                { if *expanded {
                    "show less".to_string()
                } else {
                    format!("... {} more lines", remaining)
                }}
            </div>
        </pre>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
