// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

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
            let c = text[i..].chars().next().unwrap();
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
            let c = text[i..].chars().next().unwrap();
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
        // Inline math: $...$ on a single line. Skip dollar amounts ("$5",
        // "$100") by requiring the character after `$` not to be a digit or
        // whitespace.
        if bytes[i] == b'$' {
            let line_end = text[i + 1..]
                .find('\n')
                .map(|n| i + 1 + n)
                .unwrap_or(bytes.len());
            if let Some(rel) = text[i + 1..line_end].find('$') {
                let after = bytes.get(i + 1).copied();
                let before_close_idx = i + 1 + rel;
                let before_close = bytes.get(before_close_idx.saturating_sub(1)).copied();
                let looks_like_money =
                    matches!(after, Some(c) if c.is_ascii_digit() || c == b' ' || c == b'\t');
                let trailing_money = matches!(before_close, Some(b' ') | Some(b'\t'));
                if !looks_like_money && !trailing_money {
                    let end = before_close_idx + 1;
                    emit_placeholder(&mut output, &mut math_blocks, &text[i..end]);
                    i = end;
                    continue;
                }
            }
        }

        let c = text[i..].chars().next().unwrap();
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
