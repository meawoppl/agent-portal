//! Bounded generated-image projection for imported Codex history. Planning has
//! no side effects. Cache/archive materialization happens only after the whole
//! session/history transaction commits, so losing imports create no blobs.

use base64::Engine;
use codex_codes::{GeneratedImageRef, ThreadItemPayload};
use serde::{Deserialize, Serialize};
use shared::api::{
    ImportedMessage, PreparedSessionImport, MAX_IMPORT_HISTORY_BYTES, MAX_IMPORT_MESSAGES,
};
use shared::codex_media::CodexImageMedia;
use shared::media::MediaKind;
use uuid::Uuid;

use crate::AppState;

const MAX_IMPORT_IMAGES: usize = 128;

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
enum EventKind {
    #[serde(rename = "item.started")]
    Started,
    #[serde(rename = "item.updated")]
    Updated,
    #[serde(rename = "item.completed")]
    Completed,
}

#[derive(Deserialize, Serialize)]
struct ItemEvent {
    #[serde(rename = "type")]
    kind: EventKind,
    item: codex_codes::protocol::ThreadItem,
}
#[derive(Serialize)]
struct NormalizedEvent {
    #[serde(rename = "type")]
    kind: EventKind,
    item: NormalizedItem,
}
#[derive(Serialize)]
struct NormalizedItem {
    #[serde(flatten)]
    item: codex_codes::protocol::ThreadItem,
    #[serde(rename = "portalMedia")]
    media: CodexImageMedia,
}

pub(super) struct ImportMediaPlan {
    pub prepared: PreparedSessionImport,
    pub images: ImportImages,
}
pub(super) struct ImportImages(Vec<PlannedImage>);
struct PlannedImage {
    id: Uuid,
    content_type: &'static str,
    bytes: Vec<u8>,
}

impl ImportMediaPlan {
    /// The original import is already capped at 8 MiB. Bound decoded bytes,
    /// image count and expansion independently; the caller revalidates the
    /// normalized history before its DB transaction.
    pub fn prepare(
        mut prepared: PreparedSessionImport,
        image_limit: usize,
        include_history: bool,
    ) -> Result<Self, serde_json::Error> {
        if !include_history {
            prepared.messages.clear();
        }
        let original_count = prepared.messages.len();
        let mut images = Vec::new();
        let mut decoded_bytes = 0usize;
        let mut messages = Vec::with_capacity(original_count);
        for mut message in prepared.messages {
            // This shallow probe is a fail-closed sanitizer boundary, not a
            // substitute for SDK payload decoding. A malformed outer envelope
            // must not hide a positively identified image's inline bytes.
            let known_image_kind = |value: Option<&serde_json::Value>| {
                matches!(
                    value.and_then(serde_json::Value::as_str),
                    Some("imageGeneration" | "image_generation")
                )
            };
            let is_image = known_image_kind(message.content.get("type"))
                || known_image_kind(
                    message
                        .content
                        .get("item")
                        .and_then(|item| item.get("type")),
                )
                || known_image_kind(
                    message
                        .content
                        .get("params")
                        .and_then(|params| params.get("item"))
                        .and_then(|item| item.get("type")),
                );
            if !is_image {
                messages.push(message);
                continue;
            }
            // A malformed known image must fail closed: never preserve the
            // original payload as a raw fallback containing its base64 bytes.
            let Ok(mut event) = ItemEvent::deserialize(&message.content) else {
                message.content = metadata_failure();
                messages.push(message);
                continue;
            };
            let ThreadItemPayload::ImageGeneration {
                image, saved_path, ..
            } = &mut event.item.payload
            else {
                message.content = metadata_failure();
                messages.push(message);
                continue;
            };
            let native_reference =
                saved_path.is_some() || matches!(image, GeneratedImageRef::File { .. });
            let inline = match image {
                GeneratedImageRef::Inline { result } => std::mem::take(result),
                GeneratedImageRef::File { .. } => String::new(),
            };
            let remaining = MAX_IMPORT_HISTORY_BYTES.saturating_sub(decoded_bytes);
            let can_expand = images.len() < MAX_IMPORT_IMAGES
                && original_count.saturating_add(images.len()) < MAX_IMPORT_MESSAGES;
            let decoded = if can_expand {
                decode_image(&inline, image_limit.min(remaining))
            } else {
                Err("Imported image display budget exceeded; recover the asset in native Codex history.")
            };
            let media = match decoded {
                Ok((bytes, content_type)) => {
                    let id = Uuid::new_v4();
                    decoded_bytes += bytes.len();
                    let image_row =
                        shared::PortalMessage::with_content(vec![shared::PortalContent::Image {
                            media_type: content_type.into(),
                            data: format!("/api/images/{id}"),
                            file_path: None,
                            file_size: Some(bytes.len() as u64),
                            source_type: Some("url".into()),
                        }]);
                    messages.push(ImportedMessage {
                        timestamp: message.timestamp,
                        role: "assistant".into(),
                        content: image_row.to_json(),
                    });
                    images.push(PlannedImage {
                        id,
                        content_type,
                        bytes,
                    });
                    CodexImageMedia {
                        image_id: Some(id),
                        uploaded: true,
                        ..Default::default()
                    }
                }
                Err(_)
                    if inline.is_empty()
                        && !native_reference
                        && matches!(event.kind, EventKind::Started) =>
                {
                    CodexImageMedia::default()
                }
                Err(error) => CodexImageMedia {
                    error: Some(error.into()),
                    ..Default::default()
                },
            };
            message.content = serde_json::to_value(NormalizedEvent {
                kind: event.kind,
                item: NormalizedItem {
                    item: event.item,
                    media,
                },
            })?;
            messages.push(message);
        }
        prepared.messages = messages;
        Ok(Self {
            prepared,
            images: ImportImages(images),
        })
    }
}

impl ImportImages {
    /// Infallible validated cache insertion completes before the HTTP response.
    /// A single bounded blocking batch mirrors live-media best-effort archival,
    /// without holding a DB transaction or spawning one task per image.
    pub fn materialize(
        self,
        state: &AppState,
        owner: Uuid,
        session: Uuid,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let mut archival = Vec::new();
        for image in self.0 {
            let stored = state.image_store.insert_with_id(
                image.id,
                image.content_type,
                image.bytes,
                owner,
                Some(session),
            );
            if state
                .archive
                .as_ref()
                .is_some_and(|runtime| runtime.config.media)
            {
                archival.push(crate::handlers::media_archive::MediaWriteThrough {
                    owner_user_id: owner,
                    session_id: session,
                    media_id: image.id,
                    kind: MediaKind::Image,
                    content_type: image.content_type.into(),
                    filename: None,
                    bytes: stored.data.clone(),
                });
            }
        }
        if let Some(runtime) = state.archive.clone().filter(|runtime| runtime.config.media) {
            if !archival.is_empty() {
                return Some(tokio::task::spawn_blocking(move || {
                    for image in archival {
                        crate::handlers::media_archive::write_through(&runtime, image);
                    }
                }));
            }
        }
        None
    }
}

fn metadata_failure() -> serde_json::Value {
    shared::PortalMessage::text("Imported generated image metadata could not be decoded. Image bytes were omitted; recover it in native Codex history.".into()).to_json()
}

fn decode_image(inline: &str, limit: usize) -> Result<(Vec<u8>, &'static str), &'static str> {
    if inline.is_empty() {
        return Err("Native image attachment was not copied. Open the saved file or provider attachment in Codex.");
    }
    if inline.len() > limit.div_ceil(3).saturating_mul(4).saturating_add(1024) {
        return Err(
            "Imported image exceeds the display limit; recover it in native Codex history.",
        );
    }
    let encoded = inline
        .strip_prefix("data:")
        .and_then(|data| data.split_once(";base64,").map(|(_, data)| data))
        .unwrap_or(inline);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| {
            "Imported image contains invalid base64; recover it in native Codex history."
        })?;
    if bytes.len() > limit {
        return Err(
            "Imported image exceeds the display limit; recover it in native Codex history.",
        );
    }
    let content_type = shared::media::sniff_content_type(&bytes)
        .filter(|mime| shared::media::media_kind(mime) == Some(MediaKind::Image))
        .ok_or(
            "Imported generated data is not a supported image; recover it in native Codex history.",
        )?;
    Ok((bytes, content_type))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use shared::{api::ExternalSession, AgentType};

    pub const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+x0PoAAAAASUVORK5CYII=";

    pub fn image_event(result: &str) -> serde_json::Value {
        serde_json::json!({"type":"item.completed", "item":{"type":"imageGeneration","id":"image-1","status":"completed","result":result,"startedAtMs":1700000001000i64,"completedAtMs":1700000002000i64,
            "portalMedia":{"uploaded":true,"image_id":Uuid::nil()}}})
    }
    pub fn prepared(contents: Vec<serde_json::Value>) -> PreparedSessionImport {
        PreparedSessionImport {
            session: ExternalSession {
                source_id: "native-thread".into(),
                agent_type: AgentType::Codex,
                name: "Imported images".into(),
                working_directory: "/tmp".into(),
                updated_at: 1,
                model: Some("gpt-5-codex".into()),
                portal_session_id: None,
            },
            messages: contents
                .into_iter()
                .map(|content| ImportedMessage {
                    timestamp: chrono::DateTime::from_timestamp_millis(1_700_000_002_000),
                    role: "assistant".into(),
                    content,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn materialized_import_images_survive_in_owner_scoped_archive() {
        use crate::archive::{ArchiveBackendConfig, ArchiveConfig, ArchiveRuntime};
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let temporary = tempfile::tempdir().unwrap();
        let mut state = crate::test_support::test_app_state(pool);
        state.archive = Some(std::sync::Arc::new(
            ArchiveRuntime::new(ArchiveConfig {
                backend: ArchiveBackendConfig::Local {
                    root: temporary.path().to_path_buf(),
                },
                transcripts: true,
                media: true,
            })
            .unwrap(),
        ));
        let plan = ImportMediaPlan::prepare(prepared(vec![image_event(PNG)]), 1024, true).unwrap();
        let id = plan.images.0[0].id;
        let expected = plan.images.0[0].bytes.clone();
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        plan.images
            .materialize(&state, owner, session)
            .unwrap()
            .await
            .unwrap();
        let runtime = state.archive.as_ref().unwrap();
        assert_eq!(
            runtime
                .store
                .get_media_bytes(owner, session, id)
                .unwrap()
                .unwrap(),
            expected
        );
        assert!(runtime
            .store
            .get_media_bytes(Uuid::new_v4(), session, id)
            .unwrap()
            .is_none());
        let cached = state.image_store.get(&id).unwrap();
        assert_eq!(cached.user_id, owner);
        assert_eq!(cached.session_id, Some(session));
        assert_eq!(
            runtime
                .store
                .get_media_meta(owner, session, id)
                .unwrap()
                .unwrap()
                .content_type,
            "image/png"
        );
    }

    #[test]
    fn imported_inline_image_is_a_bound_plan_and_no_base64_is_persisted() {
        let source = prepared(vec![image_event(PNG)]);
        let native_time = source.messages[0].timestamp;
        let plan = ImportMediaPlan::prepare(source, MAX_IMPORT_HISTORY_BYTES, true).unwrap();
        assert_eq!(plan.images.0.len(), 1);
        assert_eq!(plan.prepared.messages.len(), 2);
        assert!(plan
            .prepared
            .messages
            .iter()
            .all(|message| message.timestamp == native_time));
        let item = &plan.prepared.messages[1].content;
        assert_eq!(item["item"]["result"], "");
        assert_eq!(item["item"]["completedAtMs"], 1_700_000_002_000i64);
        assert_eq!(item["item"]["portalMedia"]["uploaded"], true);
        assert_ne!(
            item["item"]["portalMedia"]["image_id"],
            Uuid::nil().to_string()
        );
        assert!(!serde_json::to_string(&plan.prepared.messages)
            .unwrap()
            .contains(PNG));
    }

    #[test]
    fn malformed_known_image_envelopes_fail_closed_without_raw_fallback() {
        let malformed = [
            serde_json::json!({"item":{"type":"imageGeneration","result":PNG}}),
            serde_json::json!({"type":42,"item":{"type":"imageGeneration","result":PNG}}),
            serde_json::json!({"type":"imageGeneration","result":PNG,"item":false}),
            serde_json::json!({"type":"item/completed","params":{"item":{"type":"imageGeneration","result":PNG}}}),
            serde_json::json!({"type":"item.completed","item":{"type":"contextCompaction","id":"compact"},"params":{"item":{"type":"imageGeneration","result":PNG}}}),
        ];
        let plan =
            ImportMediaPlan::prepare(prepared(malformed.into()), MAX_IMPORT_HISTORY_BYTES, true)
                .unwrap();
        assert!(plan.images.0.is_empty());
        for message in plan.prepared.messages {
            let encoded = message.content.to_string();
            assert!(!encoded.contains(PNG));
            assert!(encoded.contains("Image bytes were omitted"));
        }
    }

    #[test]
    fn invalid_oversize_and_native_only_images_are_visible_without_uploaded_claims() {
        let file = serde_json::json!({"type":"item.completed","item":{"type":"imageGeneration","id":"file","status":"completed","fileId":"provider-reference","savedPath":"/do/not/read/backend-secret"}});
        let plan = ImportMediaPlan::prepare(
            prepared(vec![image_event("invalid!"), image_event(PNG), file]),
            1,
            true,
        )
        .unwrap();
        assert!(plan.images.0.is_empty());
        for message in plan.prepared.messages {
            assert_eq!(message.content["item"]["portalMedia"]["uploaded"], false);
            assert!(message.content["item"]["portalMedia"]["error"]
                .as_str()
                .is_some_and(|error| error.len() < 200));
            assert!(message.content["item"]["portalMedia"]
                .get("image_id")
                .is_none());
        }
    }

    #[test]
    fn history_opt_out_and_image_count_budget_do_not_materialize_extra_media() {
        let opted_out = ImportMediaPlan::prepare(
            prepared(vec![image_event(PNG)]),
            MAX_IMPORT_HISTORY_BYTES,
            false,
        )
        .unwrap();
        assert!(opted_out.prepared.messages.is_empty());
        assert!(opted_out.images.0.is_empty());
        let plan = ImportMediaPlan::prepare(
            prepared((0..=MAX_IMPORT_IMAGES).map(|_| image_event(PNG)).collect()),
            MAX_IMPORT_HISTORY_BYTES,
            true,
        )
        .unwrap();
        assert_eq!(plan.images.0.len(), MAX_IMPORT_IMAGES);
        assert_eq!(plan.prepared.messages.len(), 2 * MAX_IMPORT_IMAGES + 1);
        assert!(
            plan.prepared.messages.last().unwrap().content["item"]["portalMedia"]["error"]
                .as_str()
                .unwrap()
                .contains("budget")
        );
    }
}
