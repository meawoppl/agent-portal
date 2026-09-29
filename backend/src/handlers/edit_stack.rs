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
    UpdateEditStackItemRequest, EDIT_STACK_MESSAGE_TYPE,
};
use std::collections::HashMap;
use std::sync::Arc;
use tower_cookies::Cookies;
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

fn list_edit_stack_for_user(
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

fn create_edit_stack_items_for_user(
    app_state: &AppState,
    current_user_id: Uuid,
    session_id: Uuid,
    req: CreateEditStackRequest,
) -> Result<EditStackResponse, AppError> {
    let mut conn = app_state.conn()?;
    let _session = verify_session_mutator(&mut conn, session_id, current_user_id)?;
    if req
        .message_type
        .as_deref()
        .is_some_and(|kind| kind != EDIT_STACK_MESSAGE_TYPE)
    {
        return Err(AppError::BadRequest("unsupported edit-stack message type"));
    }
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
        .returning(SessionEditStackItem::as_returning())
        .get_results::<SessionEditStackItem>(&mut conn)?;

    Ok(EditStackResponse {
        items: load_edit_stack_items(&mut conn, session_id)?,
    })
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
    if !matches!(status, "pending" | "sent" | "dismissed") {
        return Err(AppError::BadRequest("unsupported edit stack status"));
    }

    let target = session_edit_stack_items::table
        .filter(session_edit_stack_items::session_id.eq(session_id))
        .filter(session_edit_stack_items::id.eq(item_id));
    let row: Option<SessionEditStackItem> = if status == "sent" {
        diesel::update(target)
            .set((
                session_edit_stack_items::status.eq(status),
                session_edit_stack_items::sent_client_msg_id.eq(req.sent_client_msg_id),
                session_edit_stack_items::sent_at.eq(diesel::dsl::now),
                session_edit_stack_items::updated_at.eq(diesel::dsl::now),
            ))
            .returning(SessionEditStackItem::as_returning())
            .get_result(&mut conn)
            .optional()?
    } else {
        diesel::update(target)
            .set((
                session_edit_stack_items::status.eq(status),
                session_edit_stack_items::sent_client_msg_id.eq(None::<Uuid>),
                session_edit_stack_items::sent_at.eq(None::<chrono::NaiveDateTime>),
                session_edit_stack_items::updated_at.eq(diesel::dsl::now),
            ))
            .returning(SessionEditStackItem::as_returning())
            .get_result(&mut conn)
            .optional()?
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
        .filter(|value| !value.is_empty())
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

fn validate_image_data_url(value: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
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
