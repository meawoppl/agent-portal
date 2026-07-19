//! Markdown rendering module
//!
//! Parses markdown text and renders it as Yew Html using pulldown-cmark.
//! Supports: headings, bold, italic, strikethrough, links, code blocks,
//! inline code, blockquotes, lists, and tables.

use uuid::Uuid;
use wasm_bindgen::JsCast;
use yew::prelude::*;

mod links;
mod math;
mod parser;
mod renderer;
mod sanitizer;

pub use links::linkify_urls;
use parser::parse_markdown_events;
use renderer::render_events;

#[cfg(test)]
use links::{find_next_url, is_valid_url};
#[cfg(test)]
use math::{
    extract_math_placeholders, restore_math, restore_math_in_events, MATH_CLOSE, MATH_OPEN,
};
#[cfg(test)]
use pulldown_cmark::{Event, Options, Parser, Tag};
#[cfg(test)]
use renderer::extract_text;
#[cfg(test)]
use sanitizer::{classify_link_destination, sanitize_raw_html, LinkDestination};

/// Render markdown text as HTML, with a post-render hook that triggers KaTeX
/// to render any LaTeX math expressions (`$...$`, `$$...$$`).
pub fn render_markdown(text: &str) -> Html {
    html! {
        <MarkdownView text={text.to_string()} />
    }
}

pub fn render_markdown_for_session(text: &str, session_id: Uuid) -> Html {
    html! {
        <MarkdownView text={text.to_string()} session_id={Some(session_id)} />
    }
}

#[derive(Properties, PartialEq)]
struct MarkdownViewProps {
    text: String,
    #[prop_or_default]
    session_id: Option<Uuid>,
}

#[function_component(MarkdownView)]
fn markdown_view(props: &MarkdownViewProps) -> Html {
    let node_ref = use_node_ref();

    // After render, call the JS helper to render math in this subtree
    {
        let node_ref = node_ref.clone();
        use_effect_with(props.text.clone(), move |_| {
            if let Some(node) = node_ref.cast::<web_sys::Element>() {
                if let Some(window) = web_sys::window() {
                    if let Ok(func) = js_sys::Reflect::get(&window, &"renderMathInNode".into()) {
                        if let Ok(func) = func.dyn_into::<js_sys::Function>() {
                            let _ = func.call1(&window, &node);
                        }
                    }
                }
            }
            || ()
        });
    }

    let events = parse_markdown_events(&props.text);

    html! {
        <span ref={node_ref}>{ render_events(&events, props.session_id) }</span>
    }
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

    #[test]
    fn test_extract_math_inline_with_underscore() {
        // The bug case: pulldown-cmark would otherwise treat `_{1D}` as emphasis,
        // splitting the equation across an <em> element and breaking KaTeX.
        let (out, blocks) = extract_math_placeholders("So $\\sigma_{1D}$ is small.");
        assert_eq!(blocks, vec!["$\\sigma_{1D}$"]);
        assert!(out.contains(MATH_OPEN));
        assert!(out.contains(MATH_CLOSE));
        assert!(!out.contains('_'));
    }

    #[test]
    fn test_extract_math_display_block() {
        let input = "Math: $$X(t)\\sim\\text{Pink}(0,1) \\quad S_{XX}(f)$$\nthen text.";
        let (out, blocks) = extract_math_placeholders(input);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].starts_with("$$") && blocks[0].ends_with("$$"));
        assert!(out.contains("then text."));
    }

    #[test]
    fn test_extract_math_round_trip() {
        let input = "Inline $a_1 + b_2$ and display $$\\sigma_{XX}$$ done.";
        let (out, blocks) = extract_math_placeholders(input);
        // Restoring placeholders should reproduce the original text verbatim.
        assert_eq!(restore_math(&out, &blocks), input);
    }

    #[test]
    fn test_extract_math_skips_inline_code() {
        // Math-looking syntax inside backticks must not be extracted.
        let input = "Use `$1` as the price marker.";
        let (out, blocks) = extract_math_placeholders(input);
        assert!(
            blocks.is_empty(),
            "no math should be extracted from inline code: blocks={:?}",
            blocks
        );
        assert_eq!(out, input);
    }

    #[test]
    fn test_extract_math_skips_fenced_code() {
        let input = "Before\n```rust\nlet s = \"$not_math$\";\n```\nAfter $real_math$.";
        let (out, blocks) = extract_math_placeholders(input);

        assert_eq!(blocks, vec!["$real_math$"]);
        assert!(out.contains("let s = \"$not_math$\";"));
        assert_eq!(restore_math(&out, &blocks), input);
    }

    #[test]
    fn test_extract_math_skips_dollar_amounts() {
        // "$5 and $10" looks like inline math to a naïve scanner but isn't.
        let input = "I have $5 and $10 left.";
        let (_out, blocks) = extract_math_placeholders(input);
        assert!(
            blocks.is_empty(),
            "money amounts shouldn't be extracted: blocks={:?}",
            blocks
        );
    }

    /// Regression for #684 — message shape: empty `<thinking>` HTML block,
    /// fenced ```latex``` code block (no `$` inside), then real `$$…$$`
    /// display math, then inline `$…$` math in a list. We verify the math
    /// placeholders survive the pulldown-cmark round-trip and end up as
    /// plain `Event::Text` (not `Event::Html`). My initial
    /// hypothesis for #684 was that the `<thinking>` block was swallowing
    /// the math into raw HTML events — this test disproves that.
    #[test]
    fn issue_684_math_survives_thinking_html_block() {
        let input = "<thinking>\n\n</thinking>\n\n\
Standard incompressible Newtonian form, vector notation:\n\n\
```latex\n\
\\rho \\left( \\frac{\\partial \\mathbf{v}}{\\partial t} \\right)\n\
```\n\n\
Renders as:\n\n\
$$\n\
\\rho \\left( \\frac{\\partial \\mathbf{v}}{\\partial t} \\right) = -\\nabla p\n\
$$\n\n\
Where:\n\
- $\\rho$ \u{2014} fluid density\n\
- $\\mathbf{v}$ \u{2014} velocity field\n";

        let (pre_processed, math_blocks) = extract_math_placeholders(input);
        // Three math regions: one $$…$$ display block plus two inline $…$ items.
        assert_eq!(
            math_blocks.len(),
            3,
            "expected 3 math blocks, got {math_blocks:?}"
        );

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        let parser = Parser::new_ext(&pre_processed, options);
        let events: Vec<Event> = parser.collect();
        let restored = restore_math_in_events(events, &math_blocks);

        // The math content (`\rho`) must land in plain Text events so KaTeX
        // auto-render can find the delimiters. If it ends up in `Event::Html`
        // or `Event::InlineHtml`, KaTeX cannot find the math reliably.
        let math_in_text = restored.iter().any(|e| match e {
            Event::Text(t) => t.contains("\\rho"),
            _ => false,
        });
        let math_in_html = restored.iter().any(|e| match e {
            Event::Html(t) | Event::InlineHtml(t) => t.contains("\\rho"),
            _ => false,
        });
        assert!(math_in_text, "math should reach Event::Text");
        assert!(!math_in_html, "math should not leak into Event::Html");
    }

    #[test]
    fn angle_bracket_text_is_html_not_plain_text() {
        let input = "<Download sensor-report.csv>";

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        let parser = Parser::new_ext(input, options);
        let events: Vec<Event> = parser.collect();

        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Html(t) | Event::InlineHtml(t) if t.as_ref() == input
            )),
            "pulldown-cmark classifies angle-bracket labels as raw HTML: {events:?}"
        );
    }

    #[test]
    fn bare_angle_bracket_description_is_html_not_a_link() {
        let input = "<description>";

        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        let parser = Parser::new_ext(input, options);
        let events: Vec<Event> = parser.collect();

        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Html(t) | Event::InlineHtml(t) if t.as_ref() == input
            )),
            "pulldown-cmark classifies bare <description> as raw HTML text, not a markdown link: {events:?}"
        );
    }

    #[test]
    fn raw_html_sanitizer_keeps_tags_literal() {
        assert_eq!(
            sanitize_raw_html("<script>alert('x')</script>"),
            "<script>alert('x')</script>"
        );
    }

    #[test]
    fn hash_and_absolute_paths_are_regular_links() {
        assert_eq!(
            classify_link_destination("#details", None),
            LinkDestination::ExternalOrRelative("#details".to_string())
        );
        assert_eq!(
            classify_link_destination("/api/sessions", None),
            LinkDestination::ExternalOrRelative("/api/sessions".to_string())
        );
    }

    #[test]
    fn non_link_angle_destinations_render_as_literal_text() {
        assert_eq!(
            classify_link_destination("crate::components::Thing", None),
            LinkDestination::LiteralAngleText
        );
        assert_eq!(
            classify_link_destination("not a url", None),
            LinkDestination::LiteralAngleText
        );
    }

    #[test]
    fn portal_file_markdown_link_rewrites_description_to_download_href() {
        let session_id = Uuid::parse_str("11111111-2222-3333-4444-555555555555")
            .expect("static uuid should parse");

        assert_eq!(
            classify_link_destination("portal://file/docs/portal link.svg", Some(session_id)),
            LinkDestination::PortalDownload(
                "/api/sessions/11111111-2222-3333-4444-555555555555/files/pull?path=docs%2Fportal%20link.svg"
                    .to_string()
            )
        );
    }

    #[test]
    fn portal_file_markdown_link_encodes_nested_special_chars() {
        let session_id = Uuid::parse_str("11111111-2222-3333-4444-555555555555")
            .expect("static uuid should parse");

        assert_eq!(
            classify_link_destination(
                "portal://file/reports/final report (v2).md",
                Some(session_id)
            ),
            LinkDestination::PortalDownload(
                "/api/sessions/11111111-2222-3333-4444-555555555555/files/pull?path=reports%2Ffinal%20report%20(v2).md"
                    .to_string()
            )
        );
    }

    #[test]
    fn portal_file_markdown_link_without_session_is_literal_text() {
        assert_eq!(
            classify_link_destination("portal://file/docs/portal_link_rendering.svg", None),
            LinkDestination::LiteralAngleText
        );
    }
}
