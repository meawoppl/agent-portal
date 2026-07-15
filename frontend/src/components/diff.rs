use yew::prelude::*;

#[derive(Debug, Clone)]
pub enum DiffLine<'a> {
    Context(&'a str),
    Removed(&'a str),
    Added(&'a str),
}

/// Source for a `DiffCard` body: either two snapshots (Claude `Edit` /
/// `Write`-style: compute the diff via LCS) or a pre-formatted unified-diff
/// string (Codex per-file `item/fileChange/patchUpdated`: parse the existing
/// patch text). Both paths funnel through `render_diff_html`, so the per-line
/// styling and parser tests stay shared.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffSource {
    OldNew { old: String, new: String },
    Unified { text: String },
}

/// One shared diff card for both Claude (`Edit` tool) and Codex per-file
/// (`item/fileChange/patchUpdated`) diffs — see #823.
///
/// The card is the only place that gets the `.diff-card` framed treatment
/// (background, rounded corners, scrollable body). Header fields are all
/// optional so the card collapses to just a framed body when called without
/// labels.
#[derive(Properties, PartialEq)]
pub struct DiffCardProps {
    pub source: DiffSource,
    /// File path label shown in the header. Omit for cumulative diffs that
    /// span multiple files.
    #[prop_or_default]
    pub file_path: Option<AttrValue>,
    /// Codex per-file kind: `"add"` / `"update"` / `"delete"`. Renders a
    /// colored chip next to the path. Ignored when `file_path` is absent.
    #[prop_or_default]
    pub kind: Option<AttrValue>,
    /// Claude `Edit { replace_all: true }` chip. Renders only when set.
    #[prop_or_default]
    pub replace_all: bool,
}

#[function_component(DiffCard)]
pub fn diff_card(props: &DiffCardProps) -> Html {
    let body = match &props.source {
        DiffSource::OldNew { old, new } => {
            let old_lines: Vec<&str> = old.lines().collect();
            let new_lines: Vec<&str> = new.lines().collect();
            let diff = compute_line_diff(&old_lines, &new_lines);
            render_diff_html(&diff)
        }
        DiffSource::Unified { text } => {
            let lines = parse_unified_diff(text);
            render_diff_html(&lines)
        }
    };

    html! {
        <div class="diff-card">
            <div class="diff-card-header">
                <span class="tool-icon">{ "\u{1f4dd}" }</span>
                if let Some(path) = &props.file_path {
                    if let Some(kind) = &props.kind {
                        <span class={classes!("diff-card-kind", kind.to_string())}>{ kind }</span>
                    }
                    <span class="diff-card-path">{ path }</span>
                } else {
                    <span class="diff-card-title">{ "Diff" }</span>
                }
                if props.replace_all {
                    <span class="diff-card-replace-all">{ "(replace all)" }</span>
                }
            </div>
            <div class="diff-card-body">
                { body }
            </div>
        </div>
    }
}

/// Emit the inner `<div class="diff-view">` block for a sequence of diff lines.
fn render_diff_html(lines: &[DiffLine<'_>]) -> Html {
    html! {
        <div class="diff-view">
            {
                lines.iter().map(|change| {
                    match change {
                        DiffLine::Context(line) => html! {
                            <div class="diff-line context">
                                <span class="diff-marker">{ " " }</span>
                                <span class="diff-content">{ *line }</span>
                            </div>
                        },
                        DiffLine::Removed(line) => html! {
                            <div class="diff-line removed">
                                <span class="diff-marker">{ "-" }</span>
                                <span class="diff-content">{ *line }</span>
                            </div>
                        },
                        DiffLine::Added(line) => html! {
                            <div class="diff-line added">
                                <span class="diff-marker">{ "+" }</span>
                                <span class="diff-content">{ *line }</span>
                            </div>
                        },
                    }
                }).collect::<Html>()
            }
        </div>
    }
}

/// One file section parsed out of a multi-file `git diff` payload by
/// [`split_git_diff`], ready to feed one `DiffCard`.
#[derive(Debug, Clone, PartialEq)]
pub struct GitDiffFile {
    /// Display path (the post-image `b/…` path, which is also the pre-image
    /// path for ordinary edits and deletions).
    pub path: String,
    /// `add` / `delete` / `update` — drives the `DiffCard` kind chip.
    pub kind: String,
    /// Hunk text from the first `@@` onward, fed to `DiffSource::Unified`.
    /// Empty when the file has no textual hunks (binary, or a pure
    /// rename/mode change) — the card then shows just the header.
    pub hunks: String,
}

/// Split a raw `git diff` payload into per-file sections. Each section begins
/// at a `diff --git a/… b/…` line. The `index`/`mode`/`---`/`+++` preamble is
/// dropped (only the path, a change kind, and the hunk bodies are kept) so the
/// UI can render one clean `DiffCard` per file.
pub fn split_git_diff(diff: &str) -> Vec<GitDiffFile> {
    let mut files: Vec<GitDiffFile> = Vec::new();
    let mut path = String::new();
    let mut kind = "update".to_string();
    let mut hunks = String::new();
    let mut in_file = false;
    let mut in_hunks = false;

    // Push the section we've been accumulating, if any.
    let flush = |files: &mut Vec<GitDiffFile>, path: &str, kind: &str, hunks: &str| {
        if !path.is_empty() {
            files.push(GitDiffFile {
                path: path.to_string(),
                kind: kind.to_string(),
                hunks: hunks.to_string(),
            });
        }
    };

    for line in diff.lines() {
        if let Some(header) = line.strip_prefix("diff --git ") {
            flush(&mut files, &path, &kind, &hunks);
            path = parse_git_header_path(header);
            kind = "update".to_string();
            hunks = String::new();
            in_file = true;
            in_hunks = false;
            continue;
        }
        if !in_file {
            continue;
        }
        if line.starts_with("new file mode") {
            kind = "add".to_string();
        } else if line.starts_with("deleted file mode") {
            kind = "delete".to_string();
        }
        if line.starts_with("@@ ") {
            in_hunks = true;
        }
        if in_hunks {
            hunks.push_str(line);
            hunks.push('\n');
        }
    }
    flush(&mut files, &path, &kind, &hunks);
    files
}

/// Extract the display path from a `diff --git a/<path> b/<path>` header body
/// (the part after `diff --git `). Prefers the post-image `b/` path (correct
/// for renames); falls back to the raw header when the shape is unexpected.
fn parse_git_header_path(header: &str) -> String {
    if let Some(idx) = header.rfind(" b/") {
        return header[idx + 3..].to_string();
    }
    header
        .strip_prefix("a/")
        .unwrap_or(header)
        .split(" b/")
        .next()
        .unwrap_or(header)
        .to_string()
}

/// Best-effort parse of a unified-diff payload. Skips file headers
/// (`--- `, `+++ `), hunk headers (`@@ `), and the `\ No newline at end of file`
/// marker. Body lines are classified by their leading character; lines without
/// a recognized prefix are treated as context (some tools omit the leading
/// space on context lines).
fn parse_unified_diff(diff: &str) -> Vec<DiffLine<'_>> {
    diff.lines()
        .filter(|l| !l.starts_with("--- ") && !l.starts_with("+++ ") && !l.starts_with("@@ "))
        .filter_map(|l| match l.as_bytes().first() {
            Some(b'+') => Some(DiffLine::Added(&l[1..])),
            Some(b'-') => Some(DiffLine::Removed(&l[1..])),
            Some(b' ') => Some(DiffLine::Context(&l[1..])),
            Some(b'\\') => None, // "\ No newline at end of file"
            None => Some(DiffLine::Context("")),
            _ => Some(DiffLine::Context(l)),
        })
        .collect()
}

/// Compute a line-based diff between old and new content
fn compute_line_diff<'a>(old_lines: &[&'a str], new_lines: &[&'a str]) -> Vec<DiffLine<'a>> {
    let lcs = longest_common_subsequence(old_lines, new_lines);

    let mut result = Vec::new();
    let mut old_idx = 0;
    let mut new_idx = 0;
    let mut lcs_idx = 0;

    while old_idx < old_lines.len() || new_idx < new_lines.len() {
        if lcs_idx < lcs.len() {
            let (lcs_old, lcs_new) = lcs[lcs_idx];

            while old_idx < lcs_old {
                result.push(DiffLine::Removed(old_lines[old_idx]));
                old_idx += 1;
            }

            while new_idx < lcs_new {
                result.push(DiffLine::Added(new_lines[new_idx]));
                new_idx += 1;
            }

            result.push(DiffLine::Context(old_lines[old_idx]));
            old_idx += 1;
            new_idx += 1;
            lcs_idx += 1;
        } else {
            while old_idx < old_lines.len() {
                result.push(DiffLine::Removed(old_lines[old_idx]));
                old_idx += 1;
            }
            while new_idx < new_lines.len() {
                result.push(DiffLine::Added(new_lines[new_idx]));
                new_idx += 1;
            }
        }
    }

    result
}

/// Compute longest common subsequence indices for line diff
fn longest_common_subsequence(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let m = old.len();
    let n = new.len();

    if m == 0 || n == 0 {
        return Vec::new();
    }

    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if old[i - 1] == new[j - 1] {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    let mut result = Vec::new();
    let mut i = m;
    let mut j = n;

    while i > 0 && j > 0 {
        if old[i - 1] == new[j - 1] {
            result.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] > dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    result.reverse();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify<'a>(lines: &'a [DiffLine<'a>]) -> Vec<(&'static str, &'a str)> {
        lines
            .iter()
            .map(|l| match l {
                DiffLine::Context(s) => ("ctx", *s),
                DiffLine::Added(s) => ("add", *s),
                DiffLine::Removed(s) => ("rem", *s),
            })
            .collect()
    }

    #[test]
    fn parse_single_hunk() {
        let diff =
            "--- a/foo\n+++ b/foo\n@@ -1,3 +1,3 @@\n line one\n-old line\n+new line\n line three\n";
        let parsed = parse_unified_diff(diff);
        assert_eq!(
            classify(&parsed),
            vec![
                ("ctx", "line one"),
                ("rem", "old line"),
                ("add", "new line"),
                ("ctx", "line three"),
            ]
        );
    }

    #[test]
    fn parse_multiple_hunks() {
        let diff =
            "--- a/foo\n+++ b/foo\n@@ -1,2 +1,2 @@\n a\n-b\n+B\n@@ -10,2 +10,2 @@\n c\n-d\n+D\n";
        let parsed = parse_unified_diff(diff);
        assert_eq!(
            classify(&parsed),
            vec![
                ("ctx", "a"),
                ("rem", "b"),
                ("add", "B"),
                ("ctx", "c"),
                ("rem", "d"),
                ("add", "D"),
            ]
        );
    }

    #[test]
    fn parse_blank_context_lines() {
        // A truly empty line (no leading space) should be treated as context.
        let diff = "--- a/foo\n+++ b/foo\n@@ -1,3 +1,3 @@\n one\n\n+two\n";
        let parsed = parse_unified_diff(diff);
        assert_eq!(
            classify(&parsed),
            vec![("ctx", "one"), ("ctx", ""), ("add", "two"),]
        );
    }

    #[test]
    fn parse_no_newline_marker_skipped() {
        let diff = "--- a/foo\n+++ b/foo\n@@ -1 +1 @@\n-bar\n+baz\n\\ No newline at end of file\n";
        let parsed = parse_unified_diff(diff);
        assert_eq!(classify(&parsed), vec![("rem", "bar"), ("add", "baz"),]);
    }

    #[test]
    fn parse_without_file_headers() {
        // Some tools emit only hunk + body; ensure the body still classifies.
        let diff = "@@ -1 +1 @@\n-old\n+new\n";
        let parsed = parse_unified_diff(diff);
        assert_eq!(classify(&parsed), vec![("rem", "old"), ("add", "new"),]);
    }

    #[test]
    fn split_git_diff_multiple_files_with_kinds() {
        let diff = "diff --git a/src/foo.rs b/src/foo.rs\n\
index 111..222 100644\n\
--- a/src/foo.rs\n\
+++ b/src/foo.rs\n\
@@ -1,2 +1,2 @@\n a\n-b\n+B\n\
diff --git a/new.txt b/new.txt\n\
new file mode 100644\n\
index 0000000..333\n\
--- /dev/null\n\
+++ b/new.txt\n\
@@ -0,0 +1 @@\n+hello\n\
diff --git a/gone.txt b/gone.txt\n\
deleted file mode 100644\n\
index 444..0000000\n\
--- a/gone.txt\n\
+++ /dev/null\n\
@@ -1 +0,0 @@\n-bye\n";
        let files = split_git_diff(diff);
        assert_eq!(files.len(), 3);

        assert_eq!(files[0].path, "src/foo.rs");
        assert_eq!(files[0].kind, "update");
        assert!(files[0].hunks.starts_with("@@ -1,2 +1,2 @@"));
        // Preamble (index / ---/+++) must be excluded from the hunk body.
        assert!(!files[0].hunks.contains("index 111"));

        assert_eq!(files[1].path, "new.txt");
        assert_eq!(files[1].kind, "add");
        assert!(files[1].hunks.contains("+hello"));

        assert_eq!(files[2].path, "gone.txt");
        assert_eq!(files[2].kind, "delete");
        assert!(files[2].hunks.contains("-bye"));
    }

    #[test]
    fn split_git_diff_binary_file_has_empty_hunks() {
        let diff = "diff --git a/img.png b/img.png\n\
new file mode 100644\n\
index 0000000..555\n\
Binary files /dev/null and b/img.png differ\n";
        let files = split_git_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "img.png");
        assert_eq!(files[0].kind, "add");
        assert!(files[0].hunks.is_empty());
    }

    #[test]
    fn split_git_diff_empty_input() {
        assert!(split_git_diff("").is_empty());
    }

    #[test]
    fn parse_unprefixed_line_falls_back_to_context() {
        // A body line that lost its leading space (legal in some tooling) is
        // still surfaced as context rather than dropped.
        let diff = "@@ -1 +1 @@\ncontext_line\n+added\n";
        let parsed = parse_unified_diff(diff);
        assert_eq!(
            classify(&parsed),
            vec![("ctx", "context_line"), ("add", "added"),]
        );
    }
}
