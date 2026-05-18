use yew::prelude::*;

#[derive(Debug, Clone)]
pub enum DiffLine<'a> {
    Context(&'a str),
    Removed(&'a str),
    Added(&'a str),
}

/// Source for a `DiffCard` body: either two snapshots (Claude `Edit` /
/// `Write`-style: compute the diff via LCS) or a pre-formatted unified-diff
/// string (Codex `turn/diff/updated` / `item/fileChange/patchUpdated`: parse
/// the existing patch text). Both paths funnel through `render_diff_html`,
/// so the per-line styling and parser tests stay shared.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffSource {
    OldNew { old: String, new: String },
    Unified { text: String },
}

/// One shared diff card for both Claude (`Edit` tool) and Codex
/// (`turn/diff/updated`, per-file `item/fileChange/patchUpdated`) — see #823.
///
/// The card is the only place that gets the `.diff-card` framed treatment
/// (background, rounded corners, scrollable body). Header fields are all
/// optional so the card collapses to just a framed body when called without
/// labels (e.g. a Codex cumulative turn diff with no file path).
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
    /// Codex turn-level cumulative diff label. Renders a `(cumulative)`
    /// chip next to the title so the wire semantics aren't lost when the
    /// card is visually identical to a per-file diff.
    #[prop_or_default]
    pub cumulative: bool,
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
                if props.cumulative {
                    <span class="diff-card-cumulative" title="Codex turn-level cumulative diff">
                        { "(cumulative)" }
                    </span>
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
