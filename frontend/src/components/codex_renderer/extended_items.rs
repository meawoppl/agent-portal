//! App-server-only items must remain visible even before a dedicated card is
//! available. Diagnostic JSON is capped while serializing, so a large payload
//! never needs a second unbounded string just to build a preview.
use super::tool_card::tool_card;
use codex_codes::protocol::{
    DynamicToolCallOutputContentItem, GeneratedImageRef, SubAgentActivityKind, ThreadItemPayload,
};
use serde::Serialize;
use std::io::{self, Write};
use yew::prelude::*;

const PREVIEW_BYTES: usize = 4096;

struct PreviewWriter(Vec<u8>);
impl Write for PreviewWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = PREVIEW_BYTES.saturating_sub(self.0.len());
        self.0
            .extend_from_slice(&bytes[..remaining.min(bytes.len())]);
        if bytes.len() > remaining {
            Err(io::Error::other("preview limit"))
        } else {
            Ok(bytes.len())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json_preview(value: &impl Serialize) -> String {
    let mut writer = PreviewWriter(Vec::with_capacity(PREVIEW_BYTES));
    let truncated = serde_json::to_writer_pretty(&mut writer, value).is_err();
    let mut text = String::from_utf8_lossy(&writer.0).into_owned();
    if truncated {
        text.push_str("\n[preview truncated]");
    }
    text
}

fn text_preview(value: &str) -> String {
    let mut end = value.len().min(PREVIEW_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut text = value[..end].to_string();
    if end < value.len() {
        text.push_str("\n[preview truncated]");
    }
    text
}

fn dynamic_output_preview(items: &[DynamicToolCallOutputContentItem]) -> String {
    use std::borrow::Cow;
    let summaries: Vec<Cow<'_, str>> = items
        .iter()
        .take(20)
        .map(|item| match item {
            DynamicToolCallOutputContentItem::InputText { text } => Cow::Borrowed(text.as_str()),
            DynamicToolCallOutputContentItem::InputImage { image_url } => {
                Cow::Owned(format!("Image output ({} URL bytes)", image_url.len()))
            }
            DynamicToolCallOutputContentItem::InputAudio { audio_url } => {
                Cow::Owned(format!("Audio output ({} URL bytes)", audio_url.len()))
            }
        })
        .collect();
    let mut preview = json_preview(&summaries);
    if items.len() > summaries.len() {
        preview.push_str("\n[additional output items omitted]");
    }
    preview
}

pub(super) fn render_extended_item(
    item: &ThreadItemPayload,
    completed: bool,
    media: Option<&shared::codex_media::CodexImageMedia>,
) -> Html {
    match item {
        ThreadItemPayload::ImageGeneration {
            status,
            revised_prompt,
            failure,
            saved_path,
            image,
            ..
        } => {
            let source = match image {
                GeneratedImageRef::File { file_id } => format!("Generated attachment: {file_id}"),
                // Never echo image bytes into the transcript DOM or diagnostic
                // JSON. The media normalization path supplies the preview.
                GeneratedImageRef::Inline { result } => {
                    format!("Generated image ({} encoded bytes)", result.len())
                }
            };
            tool_card(
                "Image",
                "Image generation".into(),
                Some(html! { { status } }),
                html! {
                    <>
                        { revised_prompt.as_ref().map(|prompt| html! { <pre class="tool-input-content">{ text_preview(prompt) }</pre> }).unwrap_or_default() }
                        { failure.as_ref().map(|failure| html! { <pre class="tool-result error">{ json_preview(failure) }</pre> }).unwrap_or_default() }
                        {
                        match media {
                            Some(media) if media.uploaded => html! { <div class="tool-meta">{ "Image saved in the media message." }</div> },
                            Some(media) => html! {
                                <div class="tool-result error">
                                    <div>{ text_preview(media.error.as_deref().unwrap_or("Generated image could not be uploaded.")) }</div>
                                    { media.recovery_path.as_ref().map(|path| html! {
                                        <>
                                            <pre class="tool-input-content">{ text_preview(path) }</pre>
                                            <div class="tool-meta">{ "Recovery file: copy it promptly or upload it with agent-portal show. Temporary recovery files expire after 24 hours or when the recovery budget fills. Decode .base64 files before uploading." }</div>
                                        </>
                                    }).unwrap_or_default() }
                                </div>
                            },
                            None => html! { <div class="tool-meta">{ source }</div> },
                        }
                    }
                        { saved_path.as_ref().map(|path| html! { <div class="tool-meta">{ text_preview(&path.0) }</div> }).unwrap_or_default() }
                    </>
                },
                completed,
            )
        }
        ThreadItemPayload::SubAgentActivity {
            agent_path,
            kind,
            model,
            reasoning_effort,
            ..
        } => {
            let status = match kind {
                SubAgentActivityKind::Started => "started",
                SubAgentActivityKind::Interacted => "interacted",
                SubAgentActivityKind::Interrupted => "interrupted",
                SubAgentActivityKind::Completed => "completed",
            };
            tool_card(
                "Agent",
                format!("Sub-agent {}", text_preview(agent_path)),
                Some(html! { { status } }),
                html! {
                    <>
                        { model.as_ref().map(|model| html! { <div class="tool-meta">{ model }</div> }).unwrap_or_default() }
                        { reasoning_effort.as_ref().map(|effort| html! { <div class="tool-meta">{ format!("Effort: {}", json_preview(effort)) }</div> }).unwrap_or_default() }
                    </>
                },
                completed,
            )
        }
        ThreadItemPayload::DynamicToolCall {
            tool,
            namespace,
            status,
            arguments,
            content_items,
            success,
            ..
        } => {
            let name = namespace
                .as_ref()
                .map_or_else(|| tool.clone(), |namespace| format!("{namespace} / {tool}"));
            tool_card(
                "Tool",
                name,
                Some(html! { { json_preview(status) } }),
                html! {
                    <>
                        <pre class="tool-input-content">{ json_preview(arguments) }</pre>
                        { content_items.as_ref().map(|items| html! { <pre class={classes!("tool-result-content", (*success == Some(false)).then_some("error"))}>{ dynamic_output_preview(items) }</pre> }).unwrap_or_default() }
                    </>
                },
                completed,
            )
        }
        ThreadItemPayload::Plan { text, .. } => tool_card(
            "Plan",
            "Plan".into(),
            None,
            html! {
                <crate::components::expandable::ExpandableText full_text={text.clone()} max_len={crate::components::expandable::limits::TOOL_OUTPUT} tag="pre" class={classes!("tool-input-content")} />
            },
            completed,
        ),
        ThreadItemPayload::ImageView { path, .. } => tool_card(
            "Image",
            "View image".into(),
            None,
            html! {
                <>
                    <pre class="tool-input-content">{ text_preview(&path.0) }</pre>
                    if let Some(media) = media {
                        if media.uploaded {
                            <div class="tool-meta">{"Viewed image saved in the media message."}</div>
                        } else {
                            <div class="tool-result error">{text_preview(media.error.as_deref().unwrap_or("Viewed image could not be uploaded."))}</div>
                        }
                    }
                </>
            },
            completed,
        ),
        ThreadItemPayload::EnteredReviewMode { review, .. }
        | ThreadItemPayload::ExitedReviewMode { review, .. } => {
            let label = if matches!(item, ThreadItemPayload::EnteredReviewMode { .. }) {
                "Entered review mode"
            } else {
                "Exited review mode"
            };
            tool_card(
                "Review",
                label.into(),
                None,
                html! { <pre class="tool-input-content">{ text_preview(review) }</pre> },
                completed,
            )
        }
        ThreadItemPayload::HookPrompt { fragments, .. } => tool_card(
            "Hook",
            "Hook prompt".into(),
            None,
            html! { <pre class="tool-input-content">{ json_preview(fragments) }</pre> },
            completed,
        ),
        ThreadItemPayload::Sleep { duration_ms, .. } => tool_card(
            "Wait",
            "Sleep".into(),
            None,
            html! { <div class="tool-meta">{ format!("{duration_ms} ms") }</div> },
            completed,
        ),
        // Keep every typed but not yet specialized item diagnosable. Unknown
        // wire variants fail item decoding and use AgentFrame::RawJson instead.
        _ => tool_card(
            "Item",
            "Codex item".into(),
            None,
            html! { <details><summary>{ "Item details" }</summary><pre class="tool-input-content">{ json_preview(item) }</pre></details> },
            completed,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_is_bounded_while_serializing_large_unicode_payloads() {
        let text = "\u{1f310}".repeat(100_000);
        let preview = json_preview(&text);
        assert!(preview.len() <= PREVIEW_BYTES + 32);
        assert!(preview.ends_with("[preview truncated]"));
        assert!(text_preview(&text).len() <= PREVIEW_BYTES + 32);
    }
}
