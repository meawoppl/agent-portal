use pulldown_cmark::{CodeBlockKind, Event, Tag, TagEnd};
use uuid::Uuid;
use yew::prelude::*;

use crate::components::copy_button::copy_to_clipboard;

use super::links::{classify_link_destination, linkify_urls, LinkDestination};

/// Convert pulldown-cmark events to Yew Html.
pub(super) fn render_events(events: &[Event], session_id: Option<Uuid>) -> Html {
    let mut html_parts: Vec<Html> = Vec::new();
    let mut i = 0;

    while i < events.len() {
        let (html, consumed) = render_event(&events[i..], session_id);
        html_parts.push(html);
        i += consumed;
    }

    html! { <>{ for html_parts }</> }
}

/// Render a single event or a group of related events.
/// Returns (Html, number of events consumed).
fn render_event(events: &[Event], session_id: Option<Uuid>) -> (Html, usize) {
    if events.is_empty() {
        return (html! {}, 0);
    }

    match &events[0] {
        Event::Start(tag) => render_tag(tag, events, session_id),
        Event::Text(text) => (linkify_urls(text), 1),
        Event::Code(code) => (
            html! { <code class="md-inline-code">{ linkify_urls(code) }</code> },
            1,
        ),
        Event::SoftBreak => (html! { <>{" "}</> }, 1),
        Event::HardBreak => (html! { <br /> }, 1),
        Event::Rule => (html! { <hr class="md-rule" /> }, 1),
        Event::End(_) => (html! {}, 1),
        Event::Html(html_text) | Event::InlineHtml(html_text) => {
            (html! { <>{ html_text.to_string() }</> }, 1)
        }
        _ => (html! {}, 1),
    }
}

/// Render a tag and its contents.
fn render_tag(tag: &Tag, events: &[Event], session_id: Option<Uuid>) -> (Html, usize) {
    let end_tag = get_end_tag(tag);
    let (inner_events, total_consumed) = collect_until_end(events, &end_tag);
    let inner_html = render_events(&inner_events, session_id);

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
            match classify_link_destination(&href, session_id) {
                LinkDestination::PortalDownload(download_href) => {
                    let title_attr = if title.is_empty() {
                        Some("Download file from session workspace".to_string())
                    } else {
                        Some(title.to_string())
                    };
                    html! {
                        <a href={download_href} title={title_attr} class="md-link portal-file-link">
                            { inner_html }
                        </a>
                    }
                }
                LinkDestination::LiteralAngleText => {
                    // Guard against false autolinks from angle-bracket syntax like
                    // <crate::path::Type> which pulldown-cmark interprets as URLs.
                    // Not a real URL — render as plain text with angle brackets.
                    html! { <><>{"<"}</>{ inner_html }<>{">"}</></> }
                }
                LinkDestination::ExternalOrRelative(href) => {
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
        Tag::Table(alignments) => render_table(&inner_events, alignments, session_id),
        Tag::TableHead => html! { <thead class="md-table-head">{ inner_html }</thead> },
        Tag::TableRow => html! { <tr class="md-table-row">{ inner_html }</tr> },
        Tag::TableCell => html! { <td class="md-table-cell">{ inner_html }</td> },
        _ => inner_html,
    };

    (html, total_consumed)
}

/// Get the corresponding end tag for a start tag.
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

/// Collect events until we hit the matching end tag.
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

/// Render a heading with the appropriate level.
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

/// Render a code block with optional language class and copy button.
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
            copy_to_clipboard(code_text.clone(), copied.clone(), 2000);
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

/// Render a list (ordered or unordered).
fn render_list(start: Option<u64>, inner: Html) -> Html {
    match start {
        Some(n) => {
            html! { <ol class="md-list md-ordered-list" start={n.to_string()}>{ inner }</ol> }
        }
        None => html! { <ul class="md-list md-unordered-list">{ inner }</ul> },
    }
}

/// Render a table with alignment support.
fn render_table(
    events: &[Event],
    alignments: &[pulldown_cmark::Alignment],
    session_id: Option<Uuid>,
) -> Html {
    // Tables have: TableHead (with TableRow and TableCells), then TableRows with TableCells.
    // We need to process the events to build proper thead/tbody structure.
    let mut parts: Vec<Html> = Vec::new();
    let mut i = 0;
    let mut head_processed = false;
    let alignments = alignments.to_vec();

    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::TableHead) => {
                let (inner, consumed) = collect_until_end(&events[i..], &TagEnd::TableHead);
                let head_html = render_table_head(&inner, &alignments, session_id);
                parts.push(head_html);
                i += consumed;
                head_processed = true;
            }
            Event::Start(Tag::TableRow) if head_processed => {
                let (inner, consumed) = collect_until_end(&events[i..], &TagEnd::TableRow);
                let row_html = render_table_row(&inner, &alignments, session_id);
                parts.push(row_html);
                i += consumed;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Separate head (first part) from body (the rest).
    let mut parts = parts.into_iter();
    let head_html: Html = parts.next().into_iter().collect();
    let body_html: Html = parts.collect();

    html! {
        <div class="md-table-wrapper">
            <table class="md-table">
                { head_html }
                <tbody class="md-table-body">{ body_html }</tbody>
            </table>
        </div>
    }
}

/// Render the `<th>`/`<td>` cells of one table row (header or body).
fn render_table_cells(
    events: &[Event],
    alignments: &[pulldown_cmark::Alignment],
    session_id: Option<Uuid>,
    is_header: bool,
) -> Vec<Html> {
    let mut cells: Vec<Html> = Vec::new();
    let mut i = 0;
    let mut col = 0;

    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::TableCell) => {
                let (inner, consumed) = collect_until_end(&events[i..], &TagEnd::TableCell);
                let inner_html = render_events(&inner, session_id);
                let align = alignments
                    .get(col)
                    .copied()
                    .unwrap_or(pulldown_cmark::Alignment::None);
                let style = alignment_style(align);
                cells.push(if is_header {
                    html! { <th class="md-table-header" style={style}>{ inner_html }</th> }
                } else {
                    html! { <td class="md-table-cell" style={style}>{ inner_html }</td> }
                });
                col += 1;
                i += consumed;
            }
            _ => {
                i += 1;
            }
        }
    }

    cells
}

/// Render table header row.
/// Note: pulldown-cmark puts TableCells directly inside TableHead (no TableRow wrapper).
fn render_table_head(
    events: &[Event],
    alignments: &[pulldown_cmark::Alignment],
    session_id: Option<Uuid>,
) -> Html {
    let cells = render_table_cells(events, alignments, session_id, true);
    html! { <thead class="md-table-head"><tr class="md-table-row">{ for cells }</tr></thead> }
}

/// Render a table body row.
fn render_table_row(
    events: &[Event],
    alignments: &[pulldown_cmark::Alignment],
    session_id: Option<Uuid>,
) -> Html {
    let cells = render_table_cells(events, alignments, session_id, false);
    html! { <tr class="md-table-row">{ for cells }</tr> }
}

/// Get CSS style for table cell alignment.
fn alignment_style(align: pulldown_cmark::Alignment) -> Option<String> {
    match align {
        pulldown_cmark::Alignment::Left => Some("text-align: left".to_string()),
        pulldown_cmark::Alignment::Center => Some("text-align: center".to_string()),
        pulldown_cmark::Alignment::Right => Some("text-align: right".to_string()),
        pulldown_cmark::Alignment::None => None,
    }
}

/// Extract plain text from a sequence of events.
pub(super) fn extract_text(events: &[Event]) -> String {
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
