//! The upload-notice message, built and parsed under one roof.
//!
//! When files are uploaded, the input bar composes an agent-facing sentence
//! ("I've uploaded the following files to your working directory: …") because
//! the agent genuinely needs to be told — the files just appeared on disk.
//! But the transcript used to render that machine boilerplate as if the user
//! typed it, most gratingly on agents with no inline media preview (muse).
//!
//! [`build_upload_message`] composes the wire text; [`split_upload_notice`]
//! recovers its structure so the transcript renders the user's own words plus
//! compact attachment chips instead of the prose. They live in one module with
//! a round-trip test so the builder and parser cannot drift.

use shared::fmt::format_file_size;

/// The fixed header line of the agent-facing notice. The wire format is
/// `[user text\n\n]HEADER\n- <name> (<size>)…` — one file per line.
const UPLOAD_NOTICE_HEADER: &str = "I've uploaded the following files to your working directory:";

/// Compose the combined user-text + file-list message the upload pipeline
/// sends to the agent after the last chunk lands.
pub fn build_upload_message(user_input: &str, files: &[(String, u64)]) -> String {
    let file_list: Vec<String> = files
        .iter()
        .map(|(name, size)| format!("- {} ({})", name, format_file_size(*size)))
        .collect();
    let list = file_list.join("\n");
    if user_input.is_empty() {
        format!("{UPLOAD_NOTICE_HEADER}\n{list}")
    } else {
        format!("{user_input}\n\n{UPLOAD_NOTICE_HEADER}\n{list}")
    }
}

/// A parsed upload notice: what the user actually said, and the attachments.
pub struct UploadNotice<'a> {
    /// The user's own words, if they sent any alongside the upload.
    pub user_text: Option<&'a str>,
    /// `(file name, human-readable size)` in upload order.
    pub files: Vec<(&'a str, &'a str)>,
}

/// Recognize a [`build_upload_message`] product and recover its parts.
///
/// Deliberately strict: the header must open the text or follow the exact
/// `\n\n` separator, and every subsequent line must parse as a `- name (size)`
/// row — anything else returns `None` and renders as ordinary prose, so a
/// user who merely *mentions* the header sentence never gets their message
/// rewritten into chips.
pub fn split_upload_notice(text: &str) -> Option<UploadNotice<'_>> {
    let separator = format!("\n\n{UPLOAD_NOTICE_HEADER}");
    let (user_text, list) = if let Some(rest) = text.strip_prefix(UPLOAD_NOTICE_HEADER) {
        (None, rest)
    } else {
        let at = text.find(&separator)?;
        let rest = &text[at + separator.len()..];
        (Some(&text[..at]), rest)
    };

    let mut files = Vec::new();
    for line in list.lines() {
        if line.is_empty() {
            continue;
        }
        let entry = line.strip_prefix("- ")?;
        let open = entry.rfind(" (")?;
        let size = entry[open + 2..].strip_suffix(')')?;
        files.push((&entry[..open], size));
    }
    if files.is_empty() {
        return None;
    }
    Some(UploadNotice {
        user_text: user_text.filter(|t| !t.trim().is_empty()),
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_with_user_text_prepends_text_and_blank_line() {
        let files = vec![("a.txt".to_string(), 100u64)];
        let out = build_upload_message("hello", &files);
        assert_eq!(
            out,
            "hello\n\nI've uploaded the following files to your working directory:\n- a.txt (100 B)"
        );
    }

    #[test]
    fn build_without_user_text_omits_blank_line_and_header() {
        let files = vec![("a.txt".to_string(), 100u64)];
        let out = build_upload_message("", &files);
        assert_eq!(
            out,
            "I've uploaded the following files to your working directory:\n- a.txt (100 B)"
        );
    }

    #[test]
    fn build_lists_one_file_per_line_in_input_order() {
        let files = vec![
            ("first.png".to_string(), 1024u64),
            ("second.jpg".to_string(), 2 * 1024 * 1024),
            ("third.txt".to_string(), 42u64),
        ];
        let out = build_upload_message("ship it", &files);
        let expected = "ship it\n\nI've uploaded the following files to your working directory:\n- first.png (1.0 KB)\n- second.jpg (2.0 MB)\n- third.txt (42 B)";
        assert_eq!(out, expected);
    }

    #[test]
    fn build_with_empty_file_list_still_renders_header() {
        // Defensive: the upload pipeline always supplies at least one file,
        // and the parser refuses a file-less notice (renders as prose).
        let out = build_upload_message("", &[]);
        assert_eq!(
            out,
            "I've uploaded the following files to your working directory:\n"
        );
        assert!(split_upload_notice(&out).is_none());
    }

    /// The invariant this module exists for: anything the builder emits, the
    /// parser recovers — names, sizes, order, and the user's own words.
    #[test]
    fn parser_round_trips_the_builder() {
        let files = vec![
            ("portal_pasted_image_260909_093931.png".to_string(), 34_406),
            ("notes (final).txt".to_string(), 42u64),
        ];
        for user_text in ["", "look at this"] {
            let wire = build_upload_message(user_text, &files);
            let notice = split_upload_notice(&wire).expect("builder output must parse");
            assert_eq!(
                notice.user_text,
                (!user_text.is_empty()).then_some(user_text)
            );
            assert_eq!(
                notice.files,
                vec![
                    ("portal_pasted_image_260909_093931.png", "33.6 KB"),
                    ("notes (final).txt", "42 B"),
                ]
            );
        }
    }

    #[test]
    fn prose_that_mentions_the_header_stays_prose() {
        assert!(split_upload_notice("what does \"I've uploaded the following files to your working directory:\" mean?\nnothing follows").is_none());
        assert!(split_upload_notice("plain message").is_none());
        // Header present but a malformed list row: prose, not chips.
        assert!(split_upload_notice(
            "I've uploaded the following files to your working directory:\nnot a list row"
        )
        .is_none());
    }
}
