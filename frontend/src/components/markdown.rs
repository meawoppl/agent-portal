//! Markdown rendering module
//!
//! Parses markdown text and renders it as Yew Html using pulldown-cmark.
//! Supports: headings, bold, italic, strikethrough, links, code blocks,
//! inline code, blockquotes, lists, and tables.

use gloo::timers::callback::Timeout;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::window;
use yew::prelude::*;

/// Render markdown text as HTML
pub fn render_markdown(text: &str) -> Html {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(text, options);
    let events: Vec<Event> = parser.collect();

    render_events(&events)
}

/// Convert pulldown-cmark events to Yew Html
fn render_events(events: &[Event]) -> Html {
    let mut html_parts: Vec<Html> = Vec::new();
    let mut i = 0;

    while i < events.len() {
        let (html, consumed) = render_event(&events[i..]);
        html_parts.push(html);
        i += consumed;
    }

    html! { <>{ for html_parts }</> }
}

/// Render a single event or a group of related events
/// Returns (Html, number of events consumed)
fn render_event(events: &[Event]) -> (Html, usize) {
    if events.is_empty() {
        return (html! {}, 0);
    }

    match &events[0] {
        Event::Start(tag) => render_tag(tag, events),
        Event::Text(text) => (linkify_urls(text), 1),
        Event::Code(code) => (
            html! { <code class="md-inline-code">{ linkify_urls(code) }</code> },
            1,
        ),
        Event::SoftBreak => (html! { <>{" "}</> }, 1),
        Event::HardBreak => (html! { <br /> }, 1),
        Event::Rule => (html! { <hr class="md-rule" /> }, 1),
        Event::End(_) => (html! {}, 1),
        _ => (html! {}, 1),
    }
}

/// Render a tag and its contents
fn render_tag(tag: &Tag, events: &[Event]) -> (Html, usize) {
    let end_tag = get_end_tag(tag);
    let (inner_events, total_consumed) = collect_until_end(events, &end_tag);
    let inner_html = render_events(&inner_events);

    let html = match tag {
        Tag::Paragraph => html! { <p class="md-paragraph">{ inner_html }</p> },
        Tag::Heading { level, .. } => render_heading(*level, inner_html),
        Tag::BlockQuote(_) => {
            html! { <blockquote class="md-blockquote">{ inner_html }</blockquote> }
        }
        Tag::CodeBlock(kind) => render_code_block(kind, &inner_events),
        Tag::List(start) => render_list(*start, inner_html),
        Tag::Item => html! { <li class="md-list-item">{ inner_html }</li> },
        Tag::Emphasis => html! { <em class="md-emphasis">{ inner_html }</em> },
        Tag::Strong => html! { <strong class="md-strong">{ inner_html }</strong> },
        Tag::Strikethrough => html! { <del class="md-strikethrough">{ inner_html }</del> },
        Tag::Link {
            dest_url, title, ..
        } => {
            let href = dest_url.to_string();
            // Guard against false autolinks from angle-bracket syntax like
            // <crate::path::Type> which pulldown-cmark interprets as URLs.
            if !is_valid_url(&href) && !href.starts_with('#') && !href.starts_with('/') {
                // Not a real URL — render as plain text with angle brackets
                html! { <><>{"<"}</>{ inner_html }<>{">"}</></> }
            } else {
                let title_attr = if title.is_empty() {
                    None
                } else {
                    Some(title.to_string())
                };
                html! {
                    <a href={href} title={title_attr} target="_blank" rel="noopener noreferrer" class="md-link">
                        { inner_html }
                    </a>
                }
            }
        }
        Tag::Image {
            dest_url, title, ..
        } => {
            let src = dest_url.to_string();
            let alt = extract_text(&inner_events);
            let title_attr = if title.is_empty() {
                None
            } else {
                Some(title.to_string())
            };
            html! { <img src={src} alt={alt} title={title_attr} class="md-image" /> }
        }
        Tag::Table(alignments) => render_table(&inner_events, alignments),
        Tag::TableHead => html! { <thead class="md-table-head">{ inner_html }</thead> },
        Tag::TableRow => html! { <tr class="md-table-row">{ inner_html }</tr> },
        Tag::TableCell => html! { <td class="md-table-cell">{ inner_html }</td> },
        _ => inner_html,
    };

    (html, total_consumed)
}

/// Get the corresponding end tag for a start tag
fn get_end_tag(tag: &Tag) -> TagEnd {
    match tag {
        Tag::Paragraph => TagEnd::Paragraph,
        Tag::Heading { level, .. } => TagEnd::Heading(*level),
        Tag::BlockQuote(_) => TagEnd::BlockQuote(None),
        Tag::CodeBlock(_) => TagEnd::CodeBlock,
        Tag::List(ordered) => TagEnd::List(ordered.is_some()),
        Tag::Item => TagEnd::Item,
        Tag::Emphasis => TagEnd::Emphasis,
        Tag::Strong => TagEnd::Strong,
        Tag::Strikethrough => TagEnd::Strikethrough,
        Tag::Link { .. } => TagEnd::Link,
        Tag::Image { .. } => TagEnd::Image,
        Tag::Table(_) => TagEnd::Table,
        Tag::TableHead => TagEnd::TableHead,
        Tag::TableRow => TagEnd::TableRow,
        Tag::TableCell => TagEnd::TableCell,
        _ => TagEnd::Paragraph,
    }
}

/// Collect events until we hit the matching end tag
fn collect_until_end(events: &[Event], end_tag: &TagEnd) -> (Vec<Event<'static>>, usize) {
    let mut inner = Vec::new();
    let mut depth = 0;
    let mut consumed = 1; // Start tag

    for event in events.iter().skip(1) {
        consumed += 1;

        match event {
            Event::Start(_) => {
                depth += 1;
                inner.push(event.clone().into_static());
            }
            Event::End(tag) if depth == 0 && tag == end_tag => {
                break;
            }
            Event::End(_) => {
                depth -= 1;
                inner.push(event.clone().into_static());
            }
            _ => {
                inner.push(event.clone().into_static());
            }
        }
    }

    (inner, consumed)
}

/// Render a heading with the appropriate level
fn render_heading(level: pulldown_cmark::HeadingLevel, inner: Html) -> Html {
    match level {
        pulldown_cmark::HeadingLevel::H1 => html! { <h1 class="md-heading md-h1">{ inner }</h1> },
        pulldown_cmark::HeadingLevel::H2 => html! { <h2 class="md-heading md-h2">{ inner }</h2> },
        pulldown_cmark::HeadingLevel::H3 => html! { <h3 class="md-heading md-h3">{ inner }</h3> },
        pulldown_cmark::HeadingLevel::H4 => html! { <h4 class="md-heading md-h4">{ inner }</h4> },
        pulldown_cmark::HeadingLevel::H5 => html! { <h5 class="md-heading md-h5">{ inner }</h5> },
        pulldown_cmark::HeadingLevel::H6 => html! { <h6 class="md-heading md-h6">{ inner }</h6> },
    }
}

/// Render a code block with optional language class and copy button
fn render_code_block(kind: &CodeBlockKind, inner_events: &[Event]) -> Html {
    let code_text = extract_text(inner_events);
    let lang_class = match kind {
        CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(format!(
            "language-{}",
            lang.split_whitespace().next().unwrap_or("")
        )),
        _ => None,
    };

    html! {
        <CodeBlock code_text={code_text} lang_class={lang_class} />
    }
}

#[derive(Properties, PartialEq)]
struct CodeBlockProps {
    code_text: String,
    lang_class: Option<String>,
}

#[function_component(CodeBlock)]
fn code_block(props: &CodeBlockProps) -> Html {
    let copied = use_state(|| false);

    let on_copy = {
        let code_text = props.code_text.clone();
        let copied = copied.clone();

        Callback::from(move |_: MouseEvent| {
            let code_text = code_text.clone();
            let copied = copied.clone();

            spawn_local(async move {
                if let Some(window) = window() {
                    let navigator = window.navigator();
                    let clipboard = js_sys::Reflect::get(&navigator, &"clipboard".into())
                        .ok()
                        .and_then(|v| v.dyn_into::<web_sys::Clipboard>().ok());

                    if let Some(clipboard) = clipboard {
                        let promise = clipboard.write_text(&code_text);
                        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;

                        copied.set(true);
                        let copied_reset = copied.clone();
                        Timeout::new(2000, move || {
                            copied_reset.set(false);
                        })
                        .forget();
                    }
                }
            });
        })
    };

    let button_class = if *copied {
        "code-copy-button copied"
    } else {
        "code-copy-button"
    };

    let button_label = if *copied { "Copied!" } else { "Copy" };

    html! {
        <pre class="md-code-block">
            <button class={button_class} onclick={on_copy} title="Copy to clipboard">
                { button_label }
            </button>
            <code class={classes!("md-code", props.lang_class.clone())}>{ linkify_urls(&props.code_text) }</code>
        </pre>
    }
}

/// Render a list (ordered or unordered)
fn render_list(start: Option<u64>, inner: Html) -> Html {
    match start {
        Some(n) => {
            html! { <ol class="md-list md-ordered-list" start={n.to_string()}>{ inner }</ol> }
        }
        None => html! { <ul class="md-list md-unordered-list">{ inner }</ul> },
    }
}

/// Render a table with alignment support
fn render_table(events: &[Event], alignments: &[pulldown_cmark::Alignment]) -> Html {
    // Tables have: TableHead (with TableRow and TableCells), then TableRows with TableCells
    // We need to process the events to build proper thead/tbody structure
    let mut parts: Vec<Html> = Vec::new();
    let mut i = 0;
    let mut head_processed = false;
    let alignments = alignments.to_vec();

    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::TableHead) => {
                // Find the end of TableHead and render it
                let (inner, consumed) = collect_until_end(&events[i..], &TagEnd::TableHead);
                let head_html = render_table_head(&inner, &alignments);
                parts.push(head_html);
                i += consumed;
                head_processed = true;
            }
            Event::Start(Tag::TableRow) if head_processed => {
                // Body rows come after head is processed
                let (inner, consumed) = collect_until_end(&events[i..], &TagEnd::TableRow);
                let row_html = render_table_row(&inner, &alignments);
                parts.push(row_html);
                i += consumed;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Separate head from body
    let (head, body): (Vec<_>, Vec<_>) = parts.into_iter().enumerate().partition(|(i, _)| *i == 0);
    let head_html: Html = head.into_iter().map(|(_, h)| h).collect();
    let body_html: Html = body.into_iter().map(|(_, h)| h).collect();

    html! {
        <div class="md-table-wrapper">
            <table class="md-table">
                { head_html }
                <tbody class="md-table-body">{ body_html }</tbody>
            </table>
        </div>
    }
}

/// Render table header row
/// Note: pulldown-cmark puts TableCells directly inside TableHead (no TableRow wrapper)
fn render_table_head(events: &[Event], alignments: &[pulldown_cmark::Alignment]) -> Html {
    let mut cells: Vec<Html> = Vec::new();
    let mut i = 0;
    let mut col = 0;

    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::TableCell) => {
                let (inner, consumed) = collect_until_end(&events[i..], &TagEnd::TableCell);
                let inner_html = render_events(&inner);
                let align = alignments
                    .get(col)
                    .copied()
                    .unwrap_or(pulldown_cmark::Alignment::None);
                let style = alignment_style(align);
                cells.push(html! { <th class="md-table-header" style={style}>{ inner_html }</th> });
                col += 1;
                i += consumed;
            }
            _ => {
                i += 1;
            }
        }
    }

    html! { <thead class="md-table-head"><tr class="md-table-row">{ for cells }</tr></thead> }
}

/// Render a table body row
fn render_table_row(events: &[Event], alignments: &[pulldown_cmark::Alignment]) -> Html {
    let mut cells: Vec<Html> = Vec::new();
    let mut i = 0;
    let mut col = 0;

    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::TableCell) => {
                let (inner, consumed) = collect_until_end(&events[i..], &TagEnd::TableCell);
                let inner_html = render_events(&inner);
                let align = alignments
                    .get(col)
                    .copied()
                    .unwrap_or(pulldown_cmark::Alignment::None);
                let style = alignment_style(align);
                cells.push(html! { <td class="md-table-cell" style={style}>{ inner_html }</td> });
                col += 1;
                i += consumed;
            }
            _ => {
                i += 1;
            }
        }
    }

    html! { <tr class="md-table-row">{ for cells }</tr> }
}

/// Get CSS style for table cell alignment
fn alignment_style(align: pulldown_cmark::Alignment) -> Option<String> {
    match align {
        pulldown_cmark::Alignment::Left => Some("text-align: left".to_string()),
        pulldown_cmark::Alignment::Center => Some("text-align: center".to_string()),
        pulldown_cmark::Alignment::Right => Some("text-align: right".to_string()),
        pulldown_cmark::Alignment::None => None,
    }
}

/// Extract plain text from a sequence of events
fn extract_text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Text(t) => Some(t.to_string()),
            Event::Code(c) => Some(c.to_string()),
            Event::SoftBreak | Event::HardBreak => Some(" ".to_string()),
            _ => None,
        })
        .collect()
}

/// Convert raw URLs in text to clickable links
/// Handles http:// and https:// URLs that aren't already in markdown link syntax
pub fn linkify_urls(text: &str) -> Html {
    let mut parts: Vec<Html> = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Find the next URL
        if let Some((before, url, after)) = find_next_url(remaining) {
            // Add text before the URL
            if !before.is_empty() {
                parts.push(html! { <>{ before.to_string() }</> });
            }
            // Add the URL as a link
            parts.push(html! {
                <a href={url.to_string()} target="_blank" rel="noopener noreferrer" class="md-link">
                    { url }
                </a>
            });
            remaining = after;
        } else {
            // No more URLs, add remaining text
            parts.push(html! { <>{ remaining.to_string() }</> });
            break;
        }
    }

    html! { <>{ for parts }</> }
}

/// Find the next URL in text, returning (text_before, url, text_after)
fn find_next_url(text: &str) -> Option<(&str, &str, &str)> {
    // Find http:// or https://
    let https_pos = text.find("https://");
    let http_pos = text.find("http://");

    let start = match (https_pos, http_pos) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }?;

    let before = &text[..start];
    let url_start = &text[start..];

    // Find where the URL ends
    let url_end = find_url_end(url_start);
    let url = trim_url_punctuation(&url_start[..url_end]);

    // Validate it looks like a real URL
    if !is_valid_url(url) {
        // Not a valid URL, skip this match and try to find the next one
        let skip = start + 1;
        if skip < text.len() {
            return find_next_url(&text[skip..]).map(|(b, u, a)| {
                // Adjust the "before" to include the skipped text
                let new_before_end = start + 1 + b.len();
                (&text[..new_before_end], u, a)
            });
        }
        return None;
    }

    let after = &text[start + url.len()..];
    Some((before, url, after))
}

/// Find where a URL ends (whitespace or certain punctuation)
fn find_url_end(text: &str) -> usize {
    let mut end = 0;
    let mut paren_depth = 0;
    let mut bracket_depth = 0;

    for c in text.chars() {
        match c {
            // Whitespace ends URL
            ' ' | '\t' | '\n' | '\r' => break,
            // Track parentheses for Wikipedia-style URLs
            '(' => {
                paren_depth += 1;
                end += c.len_utf8();
            }
            ')' => {
                if paren_depth > 0 {
                    paren_depth -= 1;
                    end += c.len_utf8();
                } else {
                    break;
                }
            }
            // Track brackets
            '[' => {
                bracket_depth += 1;
                end += c.len_utf8();
            }
            ']' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                    end += c.len_utf8();
                } else {
                    break;
                }
            }
            // Common URL-safe characters
            'a'..='z'
            | 'A'..='Z'
            | '0'..='9'
            | '-'
            | '_'
            | '.'
            | '~'
            | '/'
            | '?'
            | '#'
            | '&'
            | '='
            | '+'
            | '%'
            | '@'
            | ':'
            | '!'
            | '$'
            | '\''
            | '*'
            | ',' => {
                end += c.len_utf8();
            }
            // Stop on other characters (like < > " etc)
            _ => break,
        }
    }

    end
}

/// Trim trailing punctuation that's commonly not part of URLs
fn trim_url_punctuation(url: &str) -> &str {
    let mut url = url;
    let trim_chars = ['.', ',', '!', '?', ';', ':', '"', '\''];

    while let Some(c) = url.chars().last() {
        // Handle unbalanced closing parens/brackets
        if c == ')' {
            let open = url.chars().filter(|&ch| ch == '(').count();
            let close = url.chars().filter(|&ch| ch == ')').count();
            if close > open {
                url = &url[..url.len() - 1];
                continue;
            }
            break;
        }
        if c == ']' {
            let open = url.chars().filter(|&ch| ch == '[').count();
            let close = url.chars().filter(|&ch| ch == ']').count();
            if close > open {
                url = &url[..url.len() - 1];
                continue;
            }
            break;
        }
        // Trim common trailing punctuation
        if trim_chars.contains(&c) {
            url = &url[..url.len() - c.len_utf8()];
        } else {
            break;
        }
    }
    url
}

/// Check if a URL looks valid (has domain with dot or is localhost)
fn is_valid_url(url: &str) -> bool {
    let after_protocol = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or("");

    if after_protocol.is_empty() {
        return false;
    }

    // Extract domain (before first /)
    let domain_end = after_protocol.find('/').unwrap_or(after_protocol.len());
    let domain = &after_protocol[..domain_end];

    // Must have a dot or be localhost
    domain.contains('.') || domain.starts_with("localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text() {
        let events = vec![Event::Text("Hello ".into()), Event::Text("World".into())];
        assert_eq!(extract_text(&events), "Hello World");
    }

    #[test]
    fn test_find_next_url_simple() {
        let result = find_next_url("Check https://example.com for info");
        assert_eq!(result, Some(("Check ", "https://example.com", " for info")));
    }

    #[test]
    fn test_find_next_url_at_start() {
        let result = find_next_url("https://example.com is the site");
        assert_eq!(result, Some(("", "https://example.com", " is the site")));
    }

    #[test]
    fn test_find_next_url_at_end() {
        let result = find_next_url("Visit https://example.com");
        assert_eq!(result, Some(("Visit ", "https://example.com", "")));
    }

    #[test]
    fn test_find_next_url_with_path() {
        let result = find_next_url("See https://example.com/path/to/page for details");
        assert_eq!(
            result,
            Some(("See ", "https://example.com/path/to/page", " for details"))
        );
    }

    #[test]
    fn test_find_next_url_trailing_period() {
        let result = find_next_url("Visit https://example.com.");
        assert_eq!(result, Some(("Visit ", "https://example.com", ".")));
    }

    #[test]
    fn test_find_next_url_wikipedia() {
        let result =
            find_next_url("See https://en.wikipedia.org/wiki/Rust_(programming_language) here");
        assert_eq!(
            result,
            Some((
                "See ",
                "https://en.wikipedia.org/wiki/Rust_(programming_language)",
                " here"
            ))
        );
    }

    #[test]
    fn test_find_next_url_localhost() {
        let result = find_next_url("Server at http://localhost:3000/api");
        assert_eq!(
            result,
            Some(("Server at ", "http://localhost:3000/api", ""))
        );
    }

    #[test]
    fn test_find_next_url_none() {
        let result = find_next_url("No URLs here");
        assert_eq!(result, None);
    }

    #[test]
    fn test_is_valid_url() {
        assert!(is_valid_url("https://example.com"));
        assert!(is_valid_url("http://localhost:3000"));
        assert!(is_valid_url("https://sub.domain.com/path"));
        assert!(!is_valid_url("https://"));
        assert!(!is_valid_url("https://nodot"));
    }

    #[test]
    fn test_table_parsing_events() {
        // Test that pulldown-cmark generates expected events for a simple table
        let markdown = r#"| A | B |
|---|---|
| 1 | 2 |
| 3 | 4 |"#;

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        let parser = Parser::new_ext(markdown, options);
        let events: Vec<Event> = parser.collect();

        // Count table rows - body rows only (header cells are in TableHead directly)
        let row_starts: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, Event::Start(Tag::TableRow)))
            .collect();
        assert_eq!(row_starts.len(), 2, "Expected 2 body table rows");

        // Count table cells - should have 2 per row = 6 total
        let cell_starts: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, Event::Start(Tag::TableCell)))
            .collect();
        assert_eq!(cell_starts.len(), 6, "Expected 6 table cells");

        // Verify table head is present
        let has_table_head = events
            .iter()
            .any(|e| matches!(e, Event::Start(Tag::TableHead)));
        assert!(has_table_head, "Expected TableHead event");
    }
}
