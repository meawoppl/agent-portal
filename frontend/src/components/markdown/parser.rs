use pulldown_cmark::{Event, Options, Parser};

use super::math::extract_math_placeholders;

/// Parse markdown into owned pulldown-cmark events after protecting math
/// regions from markdown emphasis/link parsing.
///
/// The math placeholders are deliberately **left in place** in the returned
/// events; the renderer resolves them against the returned literals so each
/// math region becomes its own element (see [`MathSpan`](super::math_span)).
pub(super) fn parse_markdown_events(text: &str) -> (Vec<Event<'static>>, Vec<String>) {
    // Protect math regions ($…$, $$…$$, \(…\), \[…\]) from pulldown-cmark by
    // replacing them with private-use placeholders BEFORE parsing. Otherwise
    // pulldown-cmark would interpret `_` inside an equation as emphasis (and
    // `*`, etc.), splitting one equation across several DOM elements.
    let (pre_processed, math_blocks) = extract_math_placeholders(text);

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let events: Vec<Event<'static>> = Parser::new_ext(&pre_processed, options)
        .map(Event::into_static)
        .collect();
    (events, math_blocks)
}
