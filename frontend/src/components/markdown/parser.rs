use pulldown_cmark::{Event, Options, Parser};

use super::math::{extract_math_placeholders, restore_math_in_events};

/// Parse markdown into owned pulldown-cmark events after protecting math
/// regions from markdown emphasis/link parsing.
pub(super) fn parse_markdown_events(text: &str) -> Vec<Event<'static>> {
    // Protect math regions ($…$, $$…$$, \(…\), \[…\]) from pulldown-cmark by
    // replacing them with private-use placeholders BEFORE parsing. Otherwise
    // pulldown-cmark would interpret `_` inside an equation as emphasis (and
    // `*`, etc.) which splits the math text across DOM elements and prevents
    // KaTeX's auto-render from matching the surrounding delimiters.
    let (pre_processed, math_blocks) = extract_math_placeholders(text);

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let events: Vec<Event> = Parser::new_ext(&pre_processed, options).collect();
    restore_math_in_events(events, &math_blocks)
        .into_iter()
        .map(Event::into_static)
        .collect()
}
