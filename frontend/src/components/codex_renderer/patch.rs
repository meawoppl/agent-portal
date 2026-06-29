use super::tool_card::tool_card;
use crate::components::diff::{DiffCard, DiffSource};
use codex_codes::io::items::{FileChangeItem, FileUpdateChange, PatchApplyStatus, PatchChangeKind};
use yew::prelude::*;

fn patch_status_label(status: &PatchApplyStatus) -> &'static str {
    match status {
        PatchApplyStatus::InProgress => "in progress",
        PatchApplyStatus::Completed => "completed",
        PatchApplyStatus::Failed => "failed",
        PatchApplyStatus::Declined => "declined",
    }
}

/// Tagged-enum `PatchChangeKind` to a CSS suffix that pairs with the existing
/// `.diff-card-kind.{add,update,delete}` styles from #823's unified DiffCard.
fn patch_kind_css(kind: &PatchChangeKind) -> &'static str {
    match kind {
        PatchChangeKind::Add => "add",
        PatchChangeKind::Delete => "delete",
        PatchChangeKind::Update { .. } => "update",
    }
}

pub(super) fn render_file_change(it: &FileChangeItem, completed: bool) -> Html {
    if it.changes.is_empty() {
        return html! {};
    }
    let status_label = patch_status_label(&it.status).to_string();
    // Closes #827 part 2 — render the actual diff bodies through the unified
    // `<DiffCard>` from #823 instead of just chip + path. Each per-file
    // change becomes its own framed card with kind chip + path + diff body,
    // matching the layout of `render_file_change_patch`.
    let body = html! {
        <>
            { for it.changes.iter().map(render_diff_card) }
        </>
    };
    tool_card(
        "\u{1f4dd}",
        "File Changes".into(),
        Some(html! { { status_label } }),
        body,
        completed,
    )
}

pub(super) fn render_file_change_patch(changes: Option<&[FileUpdateChange]>) -> Html {
    let changes = changes.unwrap_or(&[]);
    let cards: Vec<Html> = changes
        .iter()
        .filter(|c| !c.diff.trim().is_empty())
        .map(render_diff_card)
        .collect();
    if cards.is_empty() {
        return html! {};
    }
    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                { for cards.into_iter() }
            </div>
        </div>
    }
}

/// Render one `FileUpdateChange` through the shared `<DiffCard>`. Returns
/// the bare path + kind chip (without a diff body) for empty-diff entries
/// — `item.started{file_change}` events typically carry the diff text
/// already, but a defensive empty-diff path still shows the file path.
fn render_diff_card(c: &FileUpdateChange) -> Html {
    let kind_css = AttrValue::from(patch_kind_css(&c.kind));
    let path = AttrValue::from(c.path.clone());
    if c.diff.trim().is_empty() {
        return html! {
            <div class="diff-card">
                <div class="diff-card-header">
                    <span class="tool-icon">{ "\u{1f4dd}" }</span>
                    <span class={classes!("diff-card-kind", kind_css.to_string())}>{ kind_css.clone() }</span>
                    <span class="diff-card-path">{ path }</span>
                </div>
            </div>
        };
    }
    let source = DiffSource::Unified {
        text: c.diff.clone(),
    };
    html! {
        <DiffCard {source} file_path={path} kind={kind_css} />
    }
}
