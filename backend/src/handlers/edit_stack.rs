use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::session_access::{verify_session_mutator, verify_session_reader};
use crate::models::{NewSessionEditStackItem, SessionEditStackItem};
use crate::schema::{session_edit_stack_items, users};
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use diesel::prelude::*;
use diesel::result::OptionalExtension;
use shared::api::{
    CreateEditStackRequest, EditStackItem, EditStackItemInput, EditStackResponse,
    UpdateEditStackItemRequest,
};
use std::collections::HashMap;
use std::sync::Arc;
use tower_cookies::Cookies;
use tracing::{info, warn};
use uuid::Uuid;

const MAX_EDIT_STACK_ITEMS_PER_REQUEST: usize = 50;
const MAX_EDIT_STACK_TITLE_CHARS: usize = 200;
const MAX_EDIT_STACK_TEXT_CHARS: usize = 16_000;
const MAX_EDIT_STACK_JSON_CHARS: usize = 64_000;
const MAX_EDIT_STACK_IMAGE_CHARS: usize = 1_500_000;

pub async fn list_edit_stack(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
) -> Result<Json<EditStackResponse>, AppError> {
    Ok(Json(list_edit_stack_for_user(
        &app_state,
        current_user_id,
        session_id,
    )?))
}

pub async fn list_agent_edit_stack(
    State(app_state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<EditStackResponse>, AppError> {
    let current_user_id =
        crate::handlers::agent_comms::resolve_user(&app_state, &headers, &cookies)?;
    Ok(Json(list_edit_stack_for_user(
        &app_state,
        current_user_id,
        session_id,
    )?))
}

pub(crate) fn list_edit_stack_for_user(
    app_state: &AppState,
    current_user_id: Uuid,
    session_id: Uuid,
) -> Result<EditStackResponse, AppError> {
    let mut conn = app_state.conn()?;
    let _session = verify_session_reader(&mut conn, session_id, current_user_id)?;
    Ok(EditStackResponse {
        items: load_edit_stack_items(&mut conn, session_id)?,
    })
}

pub async fn create_edit_stack_items(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
    Json(req): Json<CreateEditStackRequest>,
) -> Result<Json<EditStackResponse>, AppError> {
    Ok(Json(create_edit_stack_items_for_user(
        &app_state,
        current_user_id,
        session_id,
        req,
    )?))
}

pub async fn create_agent_edit_stack_items(
    State(app_state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    cookies: Cookies,
    Json(req): Json<CreateEditStackRequest>,
) -> Result<Json<EditStackResponse>, AppError> {
    let current_user_id =
        crate::handlers::agent_comms::resolve_user(&app_state, &headers, &cookies)?;
    Ok(Json(create_edit_stack_items_for_user(
        &app_state,
        current_user_id,
        session_id,
        req,
    )?))
}

pub(crate) fn create_edit_stack_items_for_user(
    app_state: &AppState,
    current_user_id: Uuid,
    session_id: Uuid,
    req: CreateEditStackRequest,
) -> Result<EditStackResponse, AppError> {
    let mut conn = app_state.conn()?;
    let _session = verify_session_mutator(&mut conn, session_id, current_user_id)?;
    if req.items.is_empty() {
        return Err(AppError::BadRequest("edit stack request had no items"));
    }
    if req.items.len() > MAX_EDIT_STACK_ITEMS_PER_REQUEST {
        return Err(AppError::BadRequest("too many edit stack items"));
    }

    let source = validate_json(req.source, "source")?;
    let inserts = req
        .items
        .into_iter()
        .enumerate()
        .map(|(index, item)| normalize_item(session_id, current_user_id, index, item, &source))
        .collect::<Result<Vec<_>, _>>()?;
    if inserts.is_empty() {
        return Err(AppError::BadRequest(
            "edit stack request had no usable items",
        ));
    }

    diesel::insert_into(session_edit_stack_items::table)
        .values(&inserts)
        .execute(&mut conn)?;

    Ok(EditStackResponse {
        items: load_edit_stack_items(&mut conn, session_id)?,
    })
}

pub(crate) fn try_send_next_pending_edit_stack_item(
    db_pool: &crate::db::DbPool,
    session_manager: &crate::handlers::websocket::SessionManager,
    session_key: &str,
    session_id: Uuid,
) -> Result<bool, AppError> {
    let mut conn = db_pool.get()?;
    if !session_ready_for_edit_stack_send(&mut conn, session_manager, session_id)? {
        return Ok(false);
    }

    let Some(row) = session_edit_stack_items::table
        .filter(session_edit_stack_items::session_id.eq(session_id))
        .filter(session_edit_stack_items::status.eq("pending"))
        .order(session_edit_stack_items::created_at.asc())
        .select(SessionEditStackItem::as_select())
        .first::<SessionEditStackItem>(&mut conn)
        .optional()?
    else {
        return Ok(false);
    };

    let Some(item) = enrich_items(&mut conn, vec![row.clone()])?
        .into_iter()
        .next()
    else {
        return Ok(false);
    };

    let client_msg_id = Uuid::new_v4();
    let claimed = diesel::update(
        session_edit_stack_items::table
            .filter(session_edit_stack_items::session_id.eq(session_id))
            .filter(session_edit_stack_items::id.eq(row.id))
            .filter(session_edit_stack_items::status.eq("pending")),
    )
    .set((
        session_edit_stack_items::status.eq("sent"),
        session_edit_stack_items::sent_client_msg_id.eq(client_msg_id),
        session_edit_stack_items::sent_at.eq(diesel::dsl::now),
        session_edit_stack_items::updated_at.eq(diesel::dsl::now),
    ))
    .execute(&mut conn)?;
    if claimed == 0 {
        return Ok(false);
    }

    let display_name = crate::handlers::helpers::user_display_name(&mut conn, row.created_by)
        .unwrap_or_else(|| "Unknown".to_string());
    session_manager.set_last_input_sender(session_id, row.created_by, display_name);
    drop(conn);

    let content = serde_json::Value::String(edit_stack_prompt_content(&item));
    let enqueue = session_manager.enqueue_input(
        db_pool,
        session_key,
        session_id,
        crate::handlers::websocket::EnqueueInput {
            content,
            send_mode: None,
            reasoning_effort: None,
            client_msg_id: Some(client_msg_id),
        },
    );

    if enqueue.delivered || enqueue.persisted {
        info!(
            "Edit-stack item: session {} item {} (seq {}, delivered={}, persisted={})",
            session_id, row.id, enqueue.seq, enqueue.delivered, enqueue.persisted
        );
        broadcast_edit_stack_items(db_pool, session_manager, session_key, session_id)?;
        return Ok(true);
    }

    warn!(
        "Edit-stack item {} could not be enqueued for session {}; returning it to pending",
        row.id, session_id
    );
    let mut conn = db_pool.get()?;
    diesel::update(
        session_edit_stack_items::table
            .filter(session_edit_stack_items::session_id.eq(session_id))
            .filter(session_edit_stack_items::id.eq(row.id))
            .filter(session_edit_stack_items::sent_client_msg_id.eq(client_msg_id)),
    )
    .set((
        session_edit_stack_items::status.eq("pending"),
        session_edit_stack_items::sent_client_msg_id.eq(None::<Uuid>),
        session_edit_stack_items::sent_at.eq(None::<chrono::NaiveDateTime>),
        session_edit_stack_items::updated_at.eq(diesel::dsl::now),
    ))
    .execute(&mut conn)?;
    broadcast_edit_stack_items(db_pool, session_manager, session_key, session_id)?;
    Ok(false)
}

pub(crate) fn broadcast_edit_stack_items(
    db_pool: &crate::db::DbPool,
    session_manager: &crate::handlers::websocket::SessionManager,
    session_key: &str,
    session_id: Uuid,
) -> Result<EditStackResponse, AppError> {
    let mut conn = db_pool.get()?;
    let response = EditStackResponse {
        items: load_edit_stack_items(&mut conn, session_id)?,
    };
    session_manager.broadcast_to_web_clients(
        session_key,
        shared::ServerToClient::EditStackUpdated {
            session_id,
            items: response.items.clone(),
        },
    );
    Ok(response)
}

fn session_ready_for_edit_stack_send(
    conn: &mut crate::db::DbConnection,
    session_manager: &crate::handlers::websocket::SessionManager,
    session_id: Uuid,
) -> Result<bool, AppError> {
    use crate::schema::{messages, pending_inputs, pending_permission_requests};
    use shared::api::SessionActivityState;

    if session_manager.is_turn_active(session_id) {
        return Ok(false);
    }

    let pending_input_count: i64 = pending_inputs::table
        .filter(pending_inputs::session_id.eq(session_id))
        .count()
        .get_result(conn)?;
    if pending_input_count > 0 {
        return Ok(false);
    }

    let pending_permission_count: i64 = pending_permission_requests::table
        .filter(pending_permission_requests::session_id.eq(session_id))
        .count()
        .get_result(conn)?;
    if pending_permission_count > 0 {
        return Ok(false);
    }

    let latest_signal: Option<(String, String)> = messages::table
        .filter(messages::session_id.eq(session_id))
        .filter(messages::role.eq_any(["user", "assistant", "result", "unknown", "error"]))
        .order(messages::created_at.desc())
        .select((messages::agent_type, messages::content))
        .first(conn)
        .optional()?;

    let state =
        latest_signal
            .as_ref()
            .map_or(SessionActivityState::Idle, |(agent_type, content)| {
                crate::handlers::agent_comms::turn_signal_activity_state(agent_type, content)
            });
    Ok(matches!(state, SessionActivityState::Idle))
}

pub async fn update_edit_stack_item(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path((session_id, item_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateEditStackItemRequest>,
) -> Result<Json<EditStackResponse>, AppError> {
    let mut conn = app_state.conn()?;
    let _session = verify_session_mutator(&mut conn, session_id, current_user_id)?;
    let status = req.status.as_deref().unwrap_or("pending");
    if !matches!(
        status,
        "pending" | "sent" | "completed" | "failed" | "dismissed"
    ) {
        return Err(AppError::BadRequest("unsupported edit stack status"));
    }

    let row: Option<SessionEditStackItem> = match status {
        "sent" => {
            let updated = diesel::update(
                session_edit_stack_items::table
                    .filter(session_edit_stack_items::session_id.eq(session_id))
                    .filter(session_edit_stack_items::id.eq(item_id))
                    .filter(session_edit_stack_items::status.eq_any(["pending", "sent"])),
            )
            .set((
                session_edit_stack_items::status.eq(status),
                session_edit_stack_items::sent_client_msg_id.eq(req.sent_client_msg_id),
                session_edit_stack_items::sent_at.eq(diesel::dsl::now),
                session_edit_stack_items::updated_at.eq(diesel::dsl::now),
            ))
            .returning(SessionEditStackItem::as_returning())
            .get_result(&mut conn)
            .optional()?;
            match updated {
                Some(row) => Some(row),
                None => session_edit_stack_items::table
                    .filter(session_edit_stack_items::session_id.eq(session_id))
                    .filter(session_edit_stack_items::id.eq(item_id))
                    .select(SessionEditStackItem::as_select())
                    .first(&mut conn)
                    .optional()?,
            }
        }
        "completed" | "failed" => diesel::update(
            session_edit_stack_items::table
                .filter(session_edit_stack_items::session_id.eq(session_id))
                .filter(session_edit_stack_items::id.eq(item_id)),
        )
        .set((
            session_edit_stack_items::status.eq(status),
            session_edit_stack_items::updated_at.eq(diesel::dsl::now),
        ))
        .returning(SessionEditStackItem::as_returning())
        .get_result(&mut conn)
        .optional()?,
        _ => diesel::update(
            session_edit_stack_items::table
                .filter(session_edit_stack_items::session_id.eq(session_id))
                .filter(session_edit_stack_items::id.eq(item_id)),
        )
        .set((
            session_edit_stack_items::status.eq(status),
            session_edit_stack_items::sent_client_msg_id.eq(None::<Uuid>),
            session_edit_stack_items::sent_at.eq(None::<chrono::NaiveDateTime>),
            session_edit_stack_items::updated_at.eq(diesel::dsl::now),
        ))
        .returning(SessionEditStackItem::as_returning())
        .get_result(&mut conn)
        .optional()?,
    };
    if row.is_none() {
        return Err(AppError::NotFound("edit stack item not found"));
    }

    Ok(Json(EditStackResponse {
        items: load_edit_stack_items(&mut conn, session_id)?,
    }))
}

pub async fn delete_edit_stack_item(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path((session_id, item_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<EditStackResponse>, AppError> {
    let mut conn = app_state.conn()?;
    let _session = verify_session_mutator(&mut conn, session_id, current_user_id)?;
    let updated = diesel::update(
        session_edit_stack_items::table
            .filter(session_edit_stack_items::session_id.eq(session_id))
            .filter(session_edit_stack_items::id.eq(item_id)),
    )
    .set((
        session_edit_stack_items::status.eq("deleted"),
        session_edit_stack_items::updated_at.eq(diesel::dsl::now),
    ))
    .execute(&mut conn)?;
    if updated == 0 {
        return Err(AppError::NotFound("edit stack item not found"));
    }

    Ok(Json(EditStackResponse {
        items: load_edit_stack_items(&mut conn, session_id)?,
    }))
}

fn normalize_item(
    session_id: Uuid,
    created_by: Uuid,
    index: usize,
    item: EditStackItemInput,
    source: &Option<serde_json::Value>,
) -> Result<NewSessionEditStackItem, AppError> {
    let title = clean_text(item.title.as_deref(), MAX_EDIT_STACK_TITLE_CHARS)
        .filter(|value| shared::strings::is_non_empty(value))
        .unwrap_or_else(|| format!("Edit annotation {}", index + 1));
    let body = clean_text(item.body.as_deref(), MAX_EDIT_STACK_TEXT_CHARS).unwrap_or_default();
    let image_data_url = validate_image_data_url(image_data_url(&item))?;
    let context = validate_json(item.context, "context")?;

    if body.is_empty() && image_data_url.is_none() {
        return Err(AppError::BadRequest(
            "edit stack items need a note or captured image",
        ));
    }

    Ok(NewSessionEditStackItem {
        session_id,
        created_by,
        title,
        body,
        source: source.clone(),
        context,
        image_data_url,
    })
}

fn clean_text(value: Option<&str>, max_chars: usize) -> Option<String> {
    let raw = value?;
    let mut cleaned = raw
        .replace('\r', "")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .chars()
        .take(max_chars)
        .collect::<String>();
    if raw.chars().count() > max_chars {
        cleaned.push_str("\n\n[truncated]");
    }
    Some(cleaned)
}

fn validate_json(
    value: Option<serde_json::Value>,
    label: &'static str,
) -> Result<Option<serde_json::Value>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if serde_json::to_string(&value)
        .map(|raw| raw.chars().count())
        .unwrap_or(MAX_EDIT_STACK_JSON_CHARS + 1)
        > MAX_EDIT_STACK_JSON_CHARS
    {
        return Err(AppError::BadRequest(match label {
            "source" => "edit stack source is too large",
            _ => "edit stack context is too large",
        }));
    }
    Ok(Some(value))
}

fn image_data_url(item: &EditStackItemInput) -> Option<&str> {
    item.image
        .as_ref()
        .and_then(|image| image.data_url.as_deref())
        .or(item.image_data_url.as_deref())
}

fn pretty_json(value: &serde_json::Value) -> Option<String> {
    serde_json::to_string_pretty(value).ok()
}

pub(crate) fn edit_stack_prompt_content(item: &EditStackItem) -> String {
    let mut content = String::new();
    content.push_str("Portal edit-stack annotation.\n\n");
    content.push_str(&format!("Title: {}\n\n", item.title));
    if let Some(source) = item.source.as_ref().and_then(pretty_json) {
        content.push_str("Source:\n```json\n");
        content.push_str(&source);
        content.push_str("\n```\n\n");
    }
    if let Some(context) = item.context.as_ref().and_then(pretty_json) {
        content.push_str("Selected region / view context:\n```json\n");
        content.push_str(&context);
        content.push_str("\n```\n\n");
    }
    if !shared::strings::is_non_blank(&item.body) {
        content.push_str("User note: (no text note provided)\n\n");
    } else {
        content.push_str("User note:\n");
        content.push_str(&item.body);
        content.push_str("\n\n");
    }
    if let Some(image) = item.image_data_url.as_deref() {
        content.push_str("Captured region:\n");
        content.push_str(&format!("![edit-stack capture]({image})\n\n"));
    }
    content.push_str(
        "Please handle this single edit-stack item as one focused change. Verify it, then stop so Portal can send the next pending item if requested.",
    );
    content
}

fn validate_image_data_url(value: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(value) = shared::strings::trimmed_non_blank(value) else {
        return Ok(None);
    };
    if !value.starts_with("data:image/") {
        return Err(AppError::BadRequest(
            "edit stack image must be a data:image URL",
        ));
    }
    if value.len() > MAX_EDIT_STACK_IMAGE_CHARS {
        return Err(AppError::PayloadTooLarge(format!(
            "edit stack image is {} bytes; limit is {} bytes",
            value.len(),
            MAX_EDIT_STACK_IMAGE_CHARS
        )));
    }
    Ok(Some(value.to_string()))
}

fn load_edit_stack_items(
    conn: &mut crate::db::DbConnection,
    session_id: Uuid,
) -> Result<Vec<EditStackItem>, AppError> {
    let rows: Vec<SessionEditStackItem> = session_edit_stack_items::table
        .filter(session_edit_stack_items::session_id.eq(session_id))
        .filter(session_edit_stack_items::status.ne("deleted"))
        .order(session_edit_stack_items::created_at.asc())
        .select(SessionEditStackItem::as_select())
        .load(conn)?;
    enrich_items(conn, rows)
}

fn enrich_items(
    conn: &mut crate::db::DbConnection,
    rows: Vec<SessionEditStackItem>,
) -> Result<Vec<EditStackItem>, AppError> {
    let creator_ids = rows.iter().map(|row| row.created_by).collect::<Vec<_>>();
    let user_rows: Vec<(Uuid, Option<String>, Option<String>, String)> = users::table
        .filter(users::id.eq_any(&creator_ids))
        .select((users::id, users::nickname, users::name, users::email))
        .load(conn)?;
    let names: HashMap<Uuid, String> = user_rows
        .into_iter()
        .map(|(id, nickname, name, email)| (id, nickname.or(name).unwrap_or(email)))
        .collect();

    Ok(rows
        .into_iter()
        .map(|row| EditStackItem {
            id: row.id,
            session_id: row.session_id,
            created_by: row.created_by,
            created_by_name: names.get(&row.created_by).cloned(),
            title: row.title,
            body: row.body,
            source: row.source,
            context: row.context,
            image_data_url: row.image_data_url,
            status: row.status,
            created_at: row.created_at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string(),
            updated_at: row.updated_at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string(),
            sent_at: row
                .sent_at
                .map(|ts| ts.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
            sent_client_msg_id: row.sent_client_msg_id,
        })
        .collect())
}
