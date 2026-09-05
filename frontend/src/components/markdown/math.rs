pub(super) const MATH_OPEN: char = '\u{E000}';
pub(super) const MATH_CLOSE: char = '\u{E001}';

/// Scan `text` for math regions (`$...$`, `$$...$$`, `\(...\)`, `\[...\]`)
/// outside of inline-code spans and fenced code blocks, and replace each
/// occurrence with a private-use placeholder of the form
/// `\u{E000}MATH<idx>\u{E001}`. Returns the rewritten text plus the original
/// math literals indexed by `<idx>`.
pub(super) fn extract_math_placeholders(text: &str) -> (String, Vec<String>) {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut math_blocks: Vec<String> = Vec::new();
    let mut i = 0;
    let mut in_code_fence = false;
    let mut in_inline_code = false;

    while i < bytes.len() {
        // Fenced code block toggle (``` at start of a line or after a newline)
        if bytes[i] == b'`' && bytes.get(i + 1) == Some(&b'`') && bytes.get(i + 2) == Some(&b'`') {
            output.push_str("```");
            i += 3;
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            let Some(c) = text.get(i..).and_then(|tail| tail.chars().next()) else {
                break;
            };
            output.push(c);
            i += c.len_utf8();
            continue;
        }
        // Inline-code toggle
        if bytes[i] == b'`' {
            output.push('`');
            i += 1;
            in_inline_code = !in_inline_code;
            continue;
        }
        if in_inline_code {
            let Some(c) = text.get(i..).and_then(|tail| tail.chars().next()) else {
                break;
            };
            output.push(c);
            i += c.len_utf8();
            continue;
        }
        // Display math: $$...$$
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'$') {
            if let Some(rel) = text[i + 2..].find("$$") {
                let end = i + 2 + rel + 2;
                emit_placeholder(&mut output, &mut math_blocks, &text[i..end]);
                i = end;
                continue;
            }
        }
        // LaTeX-style display: \[...\]
        if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'[') {
            if let Some(rel) = text[i + 2..].find("\\]") {
                let end = i + 2 + rel + 2;
                emit_placeholder(&mut output, &mut math_blocks, &text[i..end]);
                i = end;
                continue;
            }
        }
        // LaTeX-style inline: \(...\)
        if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'(') {
            if let Some(rel) = text[i + 2..].find("\\)") {
                let end = i + 2 + rel + 2;
                emit_placeholder(&mut output, &mut math_blocks, &text[i..end]);
                i = end;
                continue;
            }
        }
        // Inline math: $...$ on a single line.
        //
        // Telling math from money is the whole problem with `$` delimiters, and
        // the guard is on the CLOSING side. A digit after the opening `$` says
        // nothing — `$9N = 10N - N$` and `$105 \times 9 = 945$` are math, and
        // rejecting them (as an earlier "no digit after `$`" rule did) silently
        // drops most arithmetic anyone writes. What money reliably looks like is
        // a *second* amount: in "spent $0.03 and $0.05" the candidate closer is
        // followed by a digit, which is the tell. A lone `$20` never pairs.
        //
        // The three conditions, matching markdown-it-texmath:
        //   1. the opening `$` is not followed by whitespace,
        //   2. the closing `$` is not preceded by whitespace,
        //   3. the closing `$` is not followed by a digit.
        //
        // Known give-up: "$5 per hour, or $x" pairs as math. That shape is far
        // rarer than numeric math, which is why the tradeoff runs this way.
        if bytes[i] == b'$' {
            let line_end = text[i + 1..]
                .find('\n')
                .map(|n| i + 1 + n)
                .unwrap_or(bytes.len());
            if let Some(rel) = text[i + 1..line_end].find('$') {
                let after_open = bytes.get(i + 1).copied();
                let before_close_idx = i + 1 + rel;
                let before_close = bytes.get(before_close_idx.saturating_sub(1)).copied();
                let after_close = bytes.get(before_close_idx + 1).copied();

                let empty = before_close_idx == i + 1;
                let opens_on_space = matches!(after_open, Some(b' ') | Some(b'\t') | None);
                let closes_on_space = matches!(before_close, Some(b' ') | Some(b'\t'));
                let another_amount_follows = matches!(after_close, Some(c) if c.is_ascii_digit());

                if !empty && !opens_on_space && !closes_on_space && !another_amount_follows {
                    let end = before_close_idx + 1;
                    emit_placeholder(&mut output, &mut math_blocks, &text[i..end]);
                    i = end;
                    continue;
                }
            }
        }

        let Some(c) = text.get(i..).and_then(|tail| tail.chars().next()) else {
            break;
        };
        output.push(c);
        i += c.len_utf8();
    }

    (output, math_blocks)
}

fn emit_placeholder(output: &mut String, math_blocks: &mut Vec<String>, math: &str) {
    let idx = math_blocks.len();
    math_blocks.push(math.to_string());
    output.push(MATH_OPEN);
    output.push_str("MATH");
    output.push_str(&idx.to_string());
    output.push(MATH_CLOSE);
}

/// One piece of a parsed text run: literal prose, or a math region that must
/// be typeset by KaTeX into its own element.
pub(super) enum MathSegment {
    Text(String),
    Math { latex: String, display: bool },
}

/// Split a text run on math placeholders, resolving each back to its captured
/// literal and classifying it as inline or display math.
///
/// The placeholders (not the restored literals) are what survive markdown
/// parsing, which is why the split happens here at render time: it lets each
/// math region become its own DOM element instead of loose text that a
/// DOM-mutating auto-renderer would have to find and rewrite in place.
pub(super) fn split_math_segments(text: &str, math_blocks: &[String]) -> Vec<MathSegment> {
    let mut segments = Vec::new();
    let mut buf = String::new();
    let mut chars = text.chars();

    while let Some(c) = chars.next() {
        if c != MATH_OPEN {
            buf.push(c);
            continue;
        }
        let mut token = String::new();
        for tc in chars.by_ref() {
            if tc == MATH_CLOSE {
                break;
            }
            token.push(tc);
        }
        let resolved = token
            .strip_prefix("MATH")
            .and_then(|idx| idx.parse::<usize>().ok())
            .and_then(|idx| math_blocks.get(idx))
            .and_then(|literal| strip_math_delimiters(literal));
        // A malformed placeholder is dropped, matching `restore_math`.
        if let Some((latex, display)) = resolved {
            if !buf.is_empty() {
                segments.push(MathSegment::Text(std::mem::take(&mut buf)));
            }
            segments.push(MathSegment::Math { latex, display });
        }
    }

    if !buf.is_empty() {
        segments.push(MathSegment::Text(buf));
    }
    segments
}

/// Strip the delimiters off a captured literal, yielding the LaTeX source and
/// whether it typesets in display mode. `$$` is tested before `$` so display
/// math isn't mistaken for inline math wrapping a `$`-delimited body.
fn strip_math_delimiters(literal: &str) -> Option<(String, bool)> {
    for (open, close, display) in [
        ("$$", "$$", true),
        ("\\[", "\\]", true),
        ("\\(", "\\)", false),
        ("$", "$", false),
    ] {
        if let Some(rest) = literal.strip_prefix(open) {
            if let Some(inner) = rest.strip_suffix(close) {
                return Some((inner.to_string(), display));
            }
        }
    }
    None
}

pub(super) fn restore_math(text: &str, math_blocks: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == MATH_OPEN {
            let mut token = String::new();
            for tc in chars.by_ref() {
                if tc == MATH_CLOSE {
                    break;
                }
                token.push(tc);
            }
            if let Some(n_str) = token.strip_prefix("MATH") {
                if let Ok(idx) = n_str.parse::<usize>() {
                    if let Some(math) = math_blocks.get(idx) {
                        out.push_str(math);
                        continue;
                    }
                }
            }
            // Malformed: drop silently
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{extract_math_placeholders, MATH_CLOSE, MATH_OPEN};

    /// The math literals `extract_math_placeholders` pulled out of `text`.
    fn math_in(text: &str) -> Vec<String> {
        extract_math_placeholders(text).1
    }

    fn is_math(text: &str) -> bool {
        !math_in(text).is_empty()
    }

    #[test]
    fn numeric_math_is_not_mistaken_for_money() {
        // Every one of these rendered as raw source before the closing-side
        // rule, because each opens on a digit. They came from one real
        // transcript about digit patterns, where that is the norm, not an edge
        // case.
        for source in [
            r"$111111 \times 7 = 777777$",
            r"$1\underline{99999}8$",
            r"$777777\cdot9=6\underline{99999}3$",
            r"$9N = 10N - N$",
            r"$105 \times 9 = 945$",
            r"$89\cdot9 = 801$",
        ] {
            assert!(is_math(source), "should typeset: {source}");
        }
    }

    #[test]
    fn letter_led_math_still_works() {
        for source in [r"$a \cdot 9 = 10a - a$", "$N$", "$b > a$", r"$c \le a+1$"] {
            assert!(is_math(source), "should typeset: {source}");
        }
    }

    #[test]
    fn money_pairs_are_left_alone() {
        // A second amount is the tell: the candidate closer is followed by a
        // digit. This is the case the old opening-side digit check was aiming
        // at, and it is the one that actually shows up in transcripts.
        for source in [
            "spent $0.03 and $0.05 on that turn",
            "between $5 and $10",
            "prices: $100, $250, $999",
            "the run cost $1.20 and the retry cost $0.40",
        ] {
            assert!(!is_math(source), "should stay literal: {source}");
        }
    }

    #[test]
    fn a_lone_amount_never_pairs() {
        for source in ["it cost $20", "$0.03", "paid $5 today"] {
            assert!(!is_math(source), "should stay literal: {source}");
        }
    }

    #[test]
    fn whitespace_hugging_the_delimiters_is_not_math() {
        // Opening on a space is how prose like "$ 5" reads, and a closer that
        // trails a space is the other half of the same shape.
        assert!(!is_math("$ x + 1$"));
        assert!(!is_math("$x + 1 $"));
        assert!(!is_math("$$"), "empty span is not math");
    }

    #[test]
    fn math_is_ignored_inside_code() {
        assert!(!is_math("`$9N = 10N$`"), "inline code is verbatim");
        assert!(!is_math("```\n$9N = 10N$\n```"), "fenced code is verbatim");
    }

    #[test]
    fn a_placeholder_replaces_the_span_in_place() {
        let (text, blocks) = extract_math_placeholders(r"before $9N = 10N - N$ after");
        assert_eq!(blocks, vec![r"$9N = 10N - N$".to_string()]);
        assert_eq!(
            text,
            format!("before {MATH_OPEN}MATH0{MATH_CLOSE} after"),
            "the span should be swapped for its placeholder, surroundings intact"
        );
    }

    #[test]
    fn two_spans_in_one_paragraph_both_typeset() {
        // The reported symptom: within a single paragraph the digit-led span
        // stayed raw while the letter-led one rendered.
        let blocks =
            math_in(r"windows: $111111 \times 7 = 777777$ — because $a \cdot 9 = 10a - a$.");
        assert_eq!(blocks.len(), 2, "both spans should typeset, got {blocks:?}");
    }

    #[test]
    fn display_and_latex_delimiters_are_unchanged() {
        assert_eq!(math_in("$$9N = 10N$$"), vec!["$$9N = 10N$$".to_string()]);
        assert_eq!(math_in(r"\(9N\)"), vec![r"\(9N\)".to_string()]);
        assert_eq!(math_in(r"\[9N\]"), vec![r"\[9N\]".to_string()]);
    }
}
