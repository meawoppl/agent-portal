use std::rc::Rc;
use yew::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub enum DiffLine {
    Context(String),
    Removed(String),
    Added(String),
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

/// Diff lines shown before the card contracts. Matches the `max_lines` on
/// Claude's Write preview so an added file reads the same from either agent —
/// a codex `add`/`delete` diff *is* the whole file, so uncapped it dumped the
/// entire contents into the transcript.
const DIFF_PREVIEW_LINES: usize = 20;

#[function_component(DiffCard)]
pub fn diff_card(props: &DiffCardProps) -> Html {
    let expanded = use_state(|| false);

    // Memoized on the source: the card re-renders on every expand toggle and
    // whenever its parent does, and an old/new diff is an LCS over the whole
    // edited region.
    let lines = use_memo(props.source.clone(), |source| -> Rc<[DiffLine]> {
        match source {
            DiffSource::OldNew { old, new } => {
                let old_lines: Vec<&str> = old.lines().collect();
                let new_lines: Vec<&str> = new.lines().collect();
                compute_line_diff(&old_lines, &new_lines).into()
            }
            DiffSource::Unified { text } => parse_unified_diff(text).into(),
        }
    });
    let lines: &[DiffLine] = &lines;

    let hidden = lines.len().saturating_sub(DIFF_PREVIEW_LINES);
    let shown = if *expanded || hidden == 0 {
        lines
    } else {
        &lines[..DIFF_PREVIEW_LINES]
    };

    let toggle = {
        let expanded = expanded.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            expanded.set(!*expanded);
        })
    };
    let body = html! {
        <>
            { render_diff_html(shown) }
            if hidden > 0 {
                <div class="write-truncated expandable-toggle" onclick={toggle}>
                    { if *expanded { "show less".to_string() } else { format!("... {hidden} more lines") } }
                </div>
            }
        </>
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
fn render_diff_html(lines: &[DiffLine]) -> Html {
    html! {
        <div class="diff-view">
            {
                lines.iter().map(|change| {
                    match change {
                        DiffLine::Context(line) => html! {
                            <div class="diff-line context">
                                <span class="diff-marker">{ " " }</span>
                                <span class="diff-content">{ line }</span>
                            </div>
                        },
                        DiffLine::Removed(line) => html! {
                            <div class="diff-line removed">
                                <span class="diff-marker">{ "-" }</span>
                                <span class="diff-content">{ line }</span>
                            </div>
                        },
                        DiffLine::Added(line) => html! {
                            <div class="diff-line added">
                                <span class="diff-marker">{ "+" }</span>
                                <span class="diff-content">{ line }</span>
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
fn parse_unified_diff(diff: &str) -> Vec<DiffLine> {
    diff.lines()
        .filter(|l| !l.starts_with("--- ") && !l.starts_with("+++ ") && !l.starts_with("@@"))
        .filter_map(|l| match l.as_bytes().first() {
            Some(b'+') => Some(DiffLine::Added(l[1..].to_string())),
            Some(b'-') => Some(DiffLine::Removed(l[1..].to_string())),
            Some(b' ') => Some(DiffLine::Context(l[1..].to_string())),
            Some(b'\\') => None, // "\ No newline at end of file"
            None => Some(DiffLine::Context(String::new())),
            _ => Some(DiffLine::Context(l.to_string())),
        })
        .collect()
}

/// Compute a line-based diff between old and new content.
///
/// The LCS table is O(m*n) in time and memory, so the common prefix and
/// suffix are emitted as context first and only the differing middle goes
/// through it. Most edits touch a small region of a large file.
fn compute_line_diff(old_lines: &[&str], new_lines: &[&str]) -> Vec<DiffLine> {
    let prefix = old_lines
        .iter()
        .zip(new_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = old_lines[prefix..]
        .iter()
        .rev()
        .zip(new_lines[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_mid = &old_lines[prefix..old_lines.len() - suffix];
    let new_mid = &new_lines[prefix..new_lines.len() - suffix];

    let context = |line: &&str| DiffLine::Context(line.to_string());
    let mut result: Vec<DiffLine> = old_lines[..prefix].iter().map(context).collect();
    let lcs = longest_common_subsequence(old_mid, new_mid);
    let (mut old_idx, mut new_idx) = (0, 0);
    for (lcs_old, lcs_new) in lcs {
        result.extend(
            old_mid[old_idx..lcs_old]
                .iter()
                .map(|l| DiffLine::Removed(l.to_string())),
        );
        result.extend(
            new_mid[new_idx..lcs_new]
                .iter()
                .map(|l| DiffLine::Added(l.to_string())),
        );
        result.push(DiffLine::Context(old_mid[lcs_old].to_string()));
        old_idx = lcs_old + 1;
        new_idx = lcs_new + 1;
    }
    result.extend(
        old_mid[old_idx..]
            .iter()
            .map(|l| DiffLine::Removed(l.to_string())),
    );
    result.extend(
        new_mid[new_idx..]
            .iter()
            .map(|l| DiffLine::Added(l.to_string())),
    );
    result.extend(old_lines[old_lines.len() - suffix..].iter().map(context));
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

    fn classify(lines: &[DiffLine]) -> Vec<(&'static str, &str)> {
        lines
            .iter()
            .map(|l| match l {
                DiffLine::Context(s) => ("ctx", s.as_str()),
                DiffLine::Added(s) => ("add", s.as_str()),
                DiffLine::Removed(s) => ("rem", s.as_str()),
            })
            .collect()
    }

    fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
        let old: Vec<&str> = old.lines().collect();
        let new: Vec<&str> = new.lines().collect();
        compute_line_diff(&old, &new)
    }

    #[test]
    fn line_diff_keeps_shared_prefix_and_suffix_as_context() {
        assert_eq!(
            classify(&line_diff("a\nb\nc\nd\n", "a\nB\nc\nd\n")),
            vec![
                ("ctx", "a"),
                ("rem", "b"),
                ("add", "B"),
                ("ctx", "c"),
                ("ctx", "d"),
            ]
        );
    }

    #[test]
    fn line_diff_handles_pure_insertions_and_deletions() {
        assert_eq!(
            classify(&line_diff("a\nc\n", "a\nb\nc\n")),
            vec![("ctx", "a"), ("add", "b"), ("ctx", "c")]
        );
        assert_eq!(
            classify(&line_diff("a\nb\nc\n", "a\nc\n")),
            vec![("ctx", "a"), ("rem", "b"), ("ctx", "c")]
        );
        assert_eq!(classify(&line_diff("", "x\n")), vec![("add", "x")]);
        assert_eq!(classify(&line_diff("x\n", "")), vec![("rem", "x")]);
        assert_eq!(
            classify(&line_diff("same\n", "same\n")),
            vec![("ctx", "same")]
        );
    }

    /// Repeated lines straddling the edit must not be double-counted by the
    /// prefix and suffix scans.
    #[test]
    fn line_diff_prefix_and_suffix_do_not_overlap() {
        assert_eq!(
            classify(&line_diff("x\nx\n", "x\nx\nx\n")),
            vec![("ctx", "x"), ("ctx", "x"), ("add", "x")]
        );
    }

    #[test]
    fn line_diff_interleaves_changes_around_common_lines() {
        assert_eq!(
            classify(&line_diff("p\n1\nk\n2\nq\n", "p\nA\nk\nB\nq\n")),
            vec![
                ("ctx", "p"),
                ("rem", "1"),
                ("add", "A"),
                ("ctx", "k"),
                ("rem", "2"),
                ("add", "B"),
                ("ctx", "q"),
            ]
        );
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
    fn parses_muse_hunk_header_without_ranges() {
        let parsed = parse_unified_diff("--- original\n+++ updated\n@@\n-old\n+new\n");
        assert_eq!(classify(&parsed), vec![("rem", "old"), ("add", "new")]);
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
