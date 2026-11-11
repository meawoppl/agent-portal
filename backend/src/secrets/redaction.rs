//! The encryption service installs matchers; ordinary persistence helpers can
//! only redact. A DB generation check prevents a stale process from publishing
//! content after a newly registered value becomes readable.
use crate::errors::AppError;
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, AhoCorasickKind, MatchKind};
use base64::Engine;
use diesel::prelude::*;
use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use shared::api::SecretValue;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

pub const MASK: &str = "[secret redacted]";
const MAX_RULE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_JSON_NODES: usize = 32_768;
const MAX_JSON_DEPTH: usize = 64;

/// Validate before building a Value tree: byte limits alone do not bound JSON
/// heap expansion (millions of tiny array values can dwarf their wire size).
/// The visitor borrows strings and counts keys/values without retaining them.
pub fn validate_json_budget(bytes: &[u8]) -> Result<(), AppError> {
    if bytes.len() > MAX_JSON_BYTES {
        return Err(AppError::PayloadTooLarge(
            "JSON exceeds redaction byte budget".into(),
        ));
    }
    let mut budget = JsonBudget {
        nodes: 0,
        depth: 0,
        exceeded: false,
    };
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    let result = BudgetSeed(&mut budget)
        .deserialize(&mut parser)
        .and_then(|()| parser.end());
    if budget.exceeded {
        return Err(AppError::PayloadTooLarge(
            "JSON exceeds redaction node/depth budget".into(),
        ));
    }
    result.map_err(|_| AppError::BadRequest("Invalid JSON"))
}
struct JsonBudget {
    nodes: usize,
    depth: usize,
    exceeded: bool,
}
struct BudgetSeed<'a>(&'a mut JsonBudget);
struct BudgetVisitor<'a>(&'a mut JsonBudget);
impl<'de> DeserializeSeed<'de> for BudgetSeed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, parser: D) -> Result<(), D::Error> {
        self.0.nodes += 1;
        if self.0.nodes > MAX_JSON_NODES {
            self.0.exceeded = true;
            return Err(D::Error::custom("JSON node budget exceeded"));
        }
        parser.deserialize_any(BudgetVisitor(self.0))
    }
}
impl<'de> Visitor<'de> for BudgetVisitor<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<(), A::Error> {
        self.0.depth += 1;
        if self.0.depth > MAX_JSON_DEPTH {
            self.0.exceeded = true;
            return Err(A::Error::custom("JSON depth budget exceeded"));
        }
        while values.next_element_seed(BudgetSeed(self.0))?.is_some() {}
        self.0.depth -= 1;
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<(), A::Error> {
        self.0.depth += 1;
        if self.0.depth > MAX_JSON_DEPTH {
            self.0.exceeded = true;
            return Err(A::Error::custom("JSON depth budget exceeded"));
        }
        while values.next_key_seed(BudgetSeed(self.0))?.is_some() {
            values.next_value_seed(BudgetSeed(self.0))?;
        }
        self.0.depth -= 1;
        Ok(())
    }
}
fn value_budget(value: &serde_json::Value, count: &mut usize, depth: usize) -> bool {
    *count += 1;
    if *count > MAX_JSON_NODES || depth > MAX_JSON_DEPTH {
        return false;
    }
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .all(|value| value_budget(value, count, depth + 1)),
        serde_json::Value::Object(values) => values.values().all(|value| {
            *count += 1;
            value_budget(value, count, depth + 1)
        }),
        _ => true,
    }
}

type Registry = HashMap<Uuid, Weak<RwLock<Option<Arc<Redactor>>>>>;
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static HTTP_REDACTION_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

#[derive(QueryableByName)]
struct Generation {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    organization_id: Uuid,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    generation: i64,
}

#[derive(Default, Clone)]
pub struct RedactionRuntime {
    current: Arc<RwLock<Option<Arc<Redactor>>>>,
}
impl RedactionRuntime {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn install(
        &self,
        organization_id: Uuid,
        generation: i64,
        values: Vec<(Uuid, SecretValue, Option<Uuid>)>,
    ) -> Result<(), AppError> {
        let next = Arc::new(Redactor::build(generation, values)?);
        let mut guard = self.current.write().map_err(|_| unavailable())?;
        if guard
            .as_ref()
            .is_some_and(|old| old.generation > generation)
        {
            return Ok(());
        }
        *guard = Some(next);
        let mut registry = REGISTRY
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| unavailable())?;
        registry.retain(|_, entry| entry.strong_count() > 0);
        registry.insert(organization_id, Arc::downgrade(&self.current));
        Ok(())
    }
    pub fn snapshot(&self, conn: &mut PgConnection) -> Result<Arc<Redactor>, AppError> {
        snapshot(conn)
    }
}

fn unavailable() -> AppError {
    AppError::ServiceUnavailable("Secret redaction is refreshing; retry shortly")
}

pub fn snapshot(conn: &mut PgConnection) -> Result<Arc<Redactor>, AppError> {
    let row = diesel::sql_query(
        "SELECT organization_id, generation FROM secrets_generation WHERE id = 1",
    )
    .get_result::<Generation>(conn)?;
    if row.generation == 0 {
        return Ok(Arc::new(Redactor::build(0, Vec::new())?));
    }
    let cache = REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| unavailable())?
        .get(&row.organization_id)
        .and_then(Weak::upgrade)
        .ok_or_else(unavailable)?;
    let guard = cache.read().map_err(|_| unavailable())?;
    guard
        .as_ref()
        .filter(|rules| rules.generation == row.generation)
        .cloned()
        .ok_or_else(unavailable)
}

/// Compiled groups are immutable and shared. A personal matcher is only
/// consulted for that owner's content; it cannot censor another user's data.
struct PatternSet {
    matcher: Option<AhoCorasick>,
    rule_ids: Vec<Vec<Uuid>>,
}
impl PatternSet {
    fn build(patterns: BTreeMap<String, BTreeSet<Uuid>>) -> Result<Self, AppError> {
        let (mut strings, ids): (Vec<_>, Vec<_>) = patterns
            .into_iter()
            .map(|(s, ids)| (s, ids.into_iter().collect()))
            .unzip();
        let built = if strings.is_empty() {
            Ok(None)
        } else {
            AhoCorasickBuilder::new()
                .kind(Some(AhoCorasickKind::ContiguousNFA))
                .match_kind(MatchKind::LeftmostLongest)
                .build(&strings)
                .map(Some)
                .map_err(|_| AppError::ServiceUnavailable("Cannot build secret redaction rules"))
        };
        strings.zeroize();
        Ok(Self {
            matcher: built?,
            rule_ids: ids,
        })
    }
}
#[derive(Clone)]
pub struct Redactor {
    pub generation: i64,
    all: Arc<PatternSet>,
    organization: Arc<PatternSet>,
    personal: Arc<HashMap<Uuid, Arc<PatternSet>>>,
    owner: Option<Uuid>,
}
impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
impl Redactor {
    pub(crate) fn build(
        generation: i64,
        values: Vec<(Uuid, SecretValue, Option<Uuid>)>,
    ) -> Result<Self, AppError> {
        let mut all: BTreeMap<String, BTreeSet<Uuid>> = BTreeMap::new();
        let mut groups: BTreeMap<Option<Uuid>, BTreeMap<String, BTreeSet<Uuid>>> = BTreeMap::new();
        let mut budget = 0usize;
        for (id, mut value, owner) in values {
            if value.0.is_empty() {
                continue;
            }
            let encoded = base64::engine::general_purpose::STANDARD.encode(value.0.as_bytes());
            let url = urlencoding::encode(&value.0).into_owned();
            let raw = std::mem::take(&mut value.0);
            for mut form in [raw, encoded, url] {
                let group = groups.entry(owner).or_default();
                // Account for shared global and owner-group copies, including duplicate
                // values owned by multiple users. No per-request compiled matcher cache.
                if !all.contains_key(&form) {
                    budget = budget.saturating_add(form.len());
                }
                if !group.contains_key(&form) {
                    budget = budget.saturating_add(form.len());
                }
                if budget > MAX_RULE_BYTES {
                    form.zeroize();
                    for (key, _) in all {
                        let mut key = key;
                        key.zeroize();
                    }
                    for (_, group) in groups {
                        for (key, _) in group {
                            let mut key = key;
                            key.zeroize();
                        }
                    }
                    return Err(AppError::ServiceUnavailable(
                        "Secret redaction capacity exceeded",
                    ));
                }
                all.entry(form.clone()).or_default().insert(id);
                group
                    .entry(std::mem::take(&mut form))
                    .or_default()
                    .insert(id);
            }
        }
        let all = Arc::new(PatternSet::build(all)?);
        let organization = Arc::new(PatternSet::build(groups.remove(&None).unwrap_or_default())?);
        let mut personal = HashMap::new();
        for (owner, patterns) in groups {
            if let Some(owner) = owner {
                personal.insert(owner, Arc::new(PatternSet::build(patterns)?));
            }
        }
        Ok(Self {
            generation,
            all,
            organization,
            personal: Arc::new(personal),
            owner: None,
        })
    }
    /// Content scope only, never an authorization decision or secret-access ACL.
    pub fn scoped_for(&self, user: Uuid) -> Self {
        Self {
            owner: Some(user),
            ..self.clone()
        }
    }
    pub fn organization_only(&self) -> Self {
        Self {
            generation: self.generation,
            all: self.organization.clone(),
            organization: self.organization.clone(),
            personal: Arc::new(HashMap::new()),
            owner: None,
        }
    }
    fn groups(&self) -> Vec<&PatternSet> {
        match self.owner {
            None => vec![self.all.as_ref()],
            Some(owner) => {
                let mut groups = vec![self.organization.as_ref()];
                if let Some(personal) = self.personal.get(&owner) {
                    groups.push(personal.as_ref());
                }
                groups
            }
        }
    }
    fn raw_matches(&self, input: &[u8]) -> bool {
        self.groups().iter().any(|group| {
            group
                .matcher
                .as_ref()
                .is_some_and(|matcher| matcher.is_match(input))
        })
    }
    pub fn contains_bytes(&self, input: &[u8]) -> bool {
        if self.is_empty() {
            return false;
        }
        if self.raw_matches(input) {
            return true;
        }
        if input.len() > MAX_JSON_BYTES {
            return true;
        }
        if input.contains(&b'%') {
            let decoded = Zeroizing::new(urlencoding::decode_binary(input).into_owned());
            return self.raw_matches(&decoded);
        }
        false
    }
    pub fn is_empty(&self) -> bool {
        self.groups().iter().all(|group| group.matcher.is_none())
    }
    pub fn matching_version_ids(&self, input: &str) -> BTreeSet<Uuid> {
        let mut ids = BTreeSet::new();
        let decoded = if input.len() <= MAX_CONTENT_BYTES && input.contains('%') {
            Some(Zeroizing::new(
                urlencoding::decode_binary(input.as_bytes()).into_owned(),
            ))
        } else {
            None
        };
        for group in self.groups() {
            if let Some(matcher) = &group.matcher {
                for found in matcher.find_iter(input) {
                    ids.extend(group.rule_ids[found.pattern().as_usize()].iter().copied());
                }
                if let Some(decoded) = &decoded {
                    for found in matcher.find_iter(decoded.as_slice()) {
                        ids.extend(group.rule_ids[found.pattern().as_usize()].iter().copied());
                    }
                }
            }
        }
        ids
    }
    pub fn redact_str(&self, input: &str) -> String {
        if self.is_empty() {
            return input.to_owned();
        }
        if input.len() > MAX_CONTENT_BYTES {
            return MASK.into();
        }
        if input.contains('%') {
            let decoded = Zeroizing::new(urlencoding::decode_binary(input.as_bytes()).into_owned());
            if decoded.as_slice() != input.as_bytes() && self.raw_matches(&decoded) {
                return MASK.into();
            }
        }
        input
            .split(MASK)
            .map(|part| {
                // Merge at most two already-compiled match streams. Union intersections so
                // an owner's match cannot expose a suffix covered by an organization rule.
                let groups = self.groups();
                let mut matches: Vec<_> = groups
                    .iter()
                    .filter_map(|group| {
                        group
                            .matcher
                            .as_ref()
                            .map(|matcher| matcher.find_iter(part).peekable())
                    })
                    .collect();
                let mut output = String::with_capacity(part.len());
                let mut cursor = 0;
                let mut pending: Option<(usize, usize)> = None;
                loop {
                    let next = matches
                        .iter_mut()
                        .enumerate()
                        .filter_map(|(index, iter)| {
                            iter.peek().map(|found| (index, found.start(), found.end()))
                        })
                        .min_by_key(|(_, start, end)| (*start, std::cmp::Reverse(*end)));
                    let Some((index, start, end)) = next else {
                        break;
                    };
                    matches[index].next();
                    if let Some((prior_start, prior_end)) = pending {
                        if start <= prior_end {
                            pending = Some((prior_start, prior_end.max(end)));
                        } else {
                            output.push_str(&part[cursor..prior_start]);
                            output.push_str(MASK);
                            cursor = prior_end;
                            pending = Some((start, end));
                        }
                    } else {
                        pending = Some((start, end));
                    }
                }
                if let Some((start, end)) = pending {
                    output.push_str(&part[cursor..start]);
                    output.push_str(MASK);
                    cursor = end;
                }
                output.push_str(&part[cursor..]);
                output
            })
            .collect::<Vec<_>>()
            .join(MASK)
    }
    pub fn redact_json(&self, value: &mut serde_json::Value) {
        if self.is_empty() {
            return;
        }
        if !value_budget(value, &mut 0, 0) {
            *value = serde_json::Value::String(MASK.into());
            return;
        }
        self.redact_json_inner(value);
    }
    fn redact_json_inner(&self, value: &mut serde_json::Value) {
        suppress_delta_payloads(value);
        match value {
            serde_json::Value::String(text) => {
                if (text.starts_with('{') || text.starts_with('['))
                    && text.len() <= MAX_CONTENT_BYTES
                {
                    if let Err(AppError::PayloadTooLarge(_)) = validate_json_budget(text.as_bytes())
                    {
                        *text = MASK.into();
                        return;
                    }
                    if let Ok(mut nested) = serde_json::from_str::<serde_json::Value>(text) {
                        self.redact_json_inner(&mut nested);
                        *text = serde_json::to_string(&nested).unwrap_or_else(|_| MASK.into());
                        return;
                    }
                }
                *text = self.redact_str(text);
            }
            serde_json::Value::Array(values) => values
                .iter_mut()
                .for_each(|value| self.redact_json_inner(value)),
            serde_json::Value::Object(values) => {
                let prior = std::mem::take(values);
                for (key, mut value) in prior {
                    self.redact_json_inner(&mut value);
                    values.insert(self.redact_str(&key), value);
                }
            }
            serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Null => {
                if self.contains_bytes(value.to_string().as_bytes()) {
                    *value = serde_json::Value::String(MASK.into());
                }
            }
        }
    }
    pub fn redact_content(&self, input: &str) -> String {
        if self.is_empty() {
            return input.to_owned();
        }
        if input.len() > MAX_CONTENT_BYTES {
            return MASK.into();
        }
        if let Err(AppError::PayloadTooLarge(_)) = validate_json_budget(input.as_bytes()) {
            return MASK.into();
        }
        match serde_json::from_str::<serde_json::Value>(input) {
            Ok(mut value) => {
                self.redact_json(&mut value);
                serde_json::to_string(&value).unwrap_or_else(|_| MASK.into())
            }
            Err(_) => self.redact_str(input),
        }
    }
}
/// Streaming text/tool-argument fragments cannot be matched independently.
/// Suppress their payloads while rules apply; completed snapshots are redacted
/// normally. Protocol tags, stream IDs and lifecycle markers remain intact.
fn suppress_delta_payloads(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let delta = [
        object.get("type"),
        object.get("method"),
        object.get("payload_type"),
    ]
    .into_iter()
    .flatten()
    .filter_map(serde_json::Value::as_str)
    .any(|tag| tag.to_ascii_lowercase().contains("delta"));
    if let Some(text) = object
        .get_mut("text_delta")
        .filter(|value| value.is_string())
    {
        *text = serde_json::Value::String(MASK.into());
    }
    if !delta {
        return;
    }
    fn suppress(object: &mut serde_json::Map<String, serde_json::Value>) {
        for key in ["delta", "text", "thinking", "partial_json", "output"] {
            if let Some(value) = object.get_mut(key) {
                if value.is_string() {
                    *value = serde_json::Value::String(MASK.into());
                } else if let Some(object) = value.as_object_mut() {
                    suppress(object);
                }
            }
        }
    }
    suppress(object);
    for key in ["params", "payload"] {
        if let Some(params) = object
            .get_mut(key)
            .and_then(serde_json::Value::as_object_mut)
        {
            suppress(params);
        }
    }
}

pub fn snapshot_for_user(conn: &mut PgConnection, user: Uuid) -> Result<Arc<Redactor>, AppError> {
    Ok(Arc::new(snapshot(conn)?.scoped_for(user)))
}

/// The caller must hold a transaction until sanitized rows are committed.
/// Version/ACL registration requires an exclusive lock on this singleton row.
pub fn lock_generation(conn: &mut PgConnection) -> Result<(), AppError> {
    diesel::sql_query("SELECT generation FROM secrets_generation WHERE id=1 FOR SHARE")
        .execute(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rules(values: &[&str]) -> Redactor {
        Redactor::build(
            1,
            values
                .iter()
                .map(|v| (Uuid::new_v4(), SecretValue((*v).into()), None))
                .collect(),
        )
        .unwrap()
    }
    #[test]
    fn exact_encoded_json_and_repeated_redaction_are_safe() {
        let rules = rules(&["secret-value+/=", "redact", "hello world"]);
        let encoded = base64::engine::general_purpose::STANDARD.encode("secret-value+/=");
        let input = format!("secret-value+/= {encoded} hello%20world");
        let sanitized = rules.redact_str(&input);
        // URL decoding deliberately masks the entire semantic field: an
        // encoded occurrence can have different byte offsets from plaintext.
        assert_eq!(sanitized, MASK);
        assert_eq!(rules.redact_str(&sanitized), sanitized);
        let mut json: serde_json::Value =
            serde_json::from_str(r#"{"tool":{"result":"secret\u002dvalue+/="}}"#).unwrap();
        rules.redact_json(&mut json);
        assert_eq!(json["tool"]["result"], MASK);
    }
    #[test]
    fn longest_overlapping_match_is_not_partially_disclosed() {
        let rules = rules(&["abcd", "abcdef", "bc"]);
        assert_eq!(
            rules.redact_str("prefix abcdef suffix"),
            format!("prefix {MASK} suffix")
        );
    }
    #[test]
    fn oversize_records_fail_closed() {
        let rules = rules(&["needle"]);
        assert_eq!(rules.redact_str(&"a".repeat(MAX_CONTENT_BYTES + 1)), MASK);
    }
    #[test]
    fn debug_never_formats_patterns() {
        assert!(!format!("{:?}", rules(&["private-value"])).contains("private-value"));
    }
}

/// Sanitize ordinary JSON ingress and egress. Vault/auth traffic has its own
/// sensitive DTOs and must never pass through content redaction. Large streaming
/// exports are sanitized record-by-record by their producer instead.
pub async fn http_middleware(
    axum::extract::State(state): axum::extract::State<Arc<crate::AppState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::{
        body::{to_bytes, Body},
        http::header,
        response::IntoResponse,
    };
    let path = request.uri().path();
    if !path.starts_with("/api/")
        || path.starts_with("/api/secrets")
        || path.starts_with("/api/auth/")
        || path == "/api/health"
    {
        return next.run(request).await;
    }
    let actor = request
        .extensions()
        .get::<tower_cookies::Cookies>()
        .and_then(|cookies| {
            crate::auth::extract_user_id(&state, Some(request.headers()), cookies).ok()
        });
    let scoped = |rules: Arc<Redactor>| {
        Arc::new(match actor {
            Some(user) => rules.scoped_for(user),
            None => rules.organization_only(),
        })
    };
    let global = match state.conn().and_then(|mut conn| snapshot(&mut conn)) {
        Ok(rules) => rules,
        Err(error) => return error.into_response(),
    };
    // A scoped empty view still shares its catalog. Reserve before buffering
    // or waiting on handlers so slow callers cannot pin unlimited generations.
    let mut permit = if global.is_empty() {
        None
    } else {
        match HTTP_REDACTION_GATE.try_acquire() {
            Ok(permit) => Some(permit),
            Err(_) => {
                return AppError::ServiceUnavailable("Content redaction is busy; retry shortly")
                    .into_response()
            }
        }
    };
    let rules = scoped(global);
    let request = if !rules.is_empty() && is_json(request.headers()) {
        let (mut parts, body) = request.into_parts();
        let bytes = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            to_bytes(body, 16 * 1024 * 1024),
        )
        .await
        {
            Ok(Ok(bytes)) => bytes,
            Err(_) => {
                return AppError::GatewayTimeout("Content request body timed out").into_response()
            }
            Ok(Err(_)) => {
                return AppError::PayloadTooLarge("JSON exceeds secret-redaction budget".into())
                    .into_response()
            }
        };
        if let Err(error) = validate_json_budget(&bytes) {
            return error.into_response();
        }
        let mut value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return AppError::BadRequest("Invalid JSON").into_response(),
        };
        rules.redact_transport_json(&mut value);
        let body = match serde_json::to_vec(&value) {
            Ok(body) => body,
            Err(_) => return unavailable().into_response(),
        };
        parts.headers.remove(header::CONTENT_LENGTH);
        axum::extract::Request::from_parts(parts, Body::from(body))
    } else {
        request
    };
    let response = next.run(request).await;
    let latest = match state
        .conn()
        .and_then(|mut conn| snapshot(&mut conn))
        .map(scoped)
    {
        Ok(rules) => rules,
        Err(error) => return error.into_response(),
    };
    if latest.is_empty() || !is_json(response.headers()) {
        return response;
    }
    if permit.is_none() {
        permit = match HTTP_REDACTION_GATE.try_acquire() {
            Ok(permit) => Some(permit),
            Err(_) => {
                return AppError::ServiceUnavailable("Content redaction is busy; retry shortly")
                    .into_response()
            }
        };
    }
    let _permit = permit;
    let (mut parts, body) = response.into_parts();
    let bytes = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        to_bytes(body, 16 * 1024 * 1024),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Err(_) => {
            return AppError::GatewayTimeout("Content response body timed out").into_response()
        }
        Ok(Err(_)) => {
            return AppError::ServiceUnavailable(
                "Use paginated history: response exceeds redaction budget",
            )
            .into_response()
        }
    };
    if let Err(error) = validate_json_budget(&bytes) {
        return error.into_response();
    }
    let mut value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return unavailable().into_response(),
    };
    latest.redact_transport_json(&mut value);
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    match serde_json::to_vec(&value) {
        Ok(bytes) => axum::response::Response::from_parts(parts, Body::from(bytes)),
        Err(_) => unavailable().into_response(),
    }
}
fn is_json(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime == "application/json" || mime.ends_with("+json"))
        })
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    fn rules(values: &[&str]) -> Redactor {
        Redactor::build(
            1,
            values
                .iter()
                .map(|value| (Uuid::new_v4(), SecretValue((*value).into()), None))
                .collect(),
        )
        .unwrap()
    }
    #[test]
    fn numeric_boolean_and_null_values_cannot_bypass_semantic_redaction() {
        for secret in ["123456", "true", "false", "null"] {
            let rules = rules(&[secret]);
            assert_eq!(
                rules.redact_content(secret),
                serde_json::to_string(MASK).unwrap()
            );
            let mut nested: serde_json::Value =
                serde_json::from_str(&format!("{{\"value\":{secret}}}")).unwrap();
            rules.redact_json(&mut nested);
            assert_eq!(nested["value"], MASK);
        }
        let rules = rules(&["123456"]);
        assert_eq!(rules.redact_content("42"), "42");
    }
    #[test]
    fn mixed_case_url_escapes_mask_whole_field_with_bounded_decoding() {
        let rules = rules(&["a/b?c"]);
        for encoded in [
            "a%2fb%3fc",
            "a%2Fb%3fc",
            "prefix a%2fb%3Fc suffix",
            "a%2fb%3fc%FF",
        ] {
            assert_eq!(rules.redact_str(encoded), MASK);
            assert!(rules.contains_bytes(encoded.as_bytes()));
            assert!(!rules.matching_version_ids(encoded).is_empty());
        }
        assert_eq!(rules.redact_str("safe%20content"), "safe%20content");
    }
    #[test]
    fn split_stream_fragments_are_suppressed_but_completed_snapshots_survive() {
        let rules = rules(&["secret"]);
        for fragment in ["sec", "ret"] {
            let mut claude = serde_json::json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":fragment}}});
            rules.redact_json(&mut claude);
            assert_eq!(claude["event"]["delta"]["text"], MASK);
            assert_eq!(claude["event"]["delta"]["type"], "text_delta");
            let mut codex = serde_json::json!({"method":"item/agentMessage/delta","params":{"itemId":"stable","delta":fragment}});
            rules.redact_json(&mut codex);
            assert_eq!(codex["params"]["delta"], MASK);
            assert_eq!(codex["params"]["itemId"], "stable");
        }
        let mut tool =
            serde_json::json!({"type":"input_json_delta","partial_json":"{\"token\":\"sec"});
        rules.redact_json(&mut tool);
        assert_eq!(tool["partial_json"], MASK);
        let mut complete = serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":"safe answer and secret"}});
        rules.redact_json(&mut complete);
        assert_eq!(complete["item"]["text"], format!("safe answer and {MASK}"));
        let empty = Redactor::build(0, vec![]).unwrap();
        let mut fragment = serde_json::json!({"type":"text_delta","text":"safe"});
        empty.redact_json(&mut fragment);
        assert_eq!(fragment["text"], "safe");
    }
    #[test]
    fn node_and_depth_budgets_reject_heap_amplification_before_value_allocation() {
        let many = format!(
            "[{}]",
            std::iter::repeat_n("0", MAX_JSON_NODES + 1)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(matches!(
            validate_json_budget(many.as_bytes()),
            Err(AppError::PayloadTooLarge(_))
        ));
        let deep = format!(
            "{}0{}",
            "[".repeat(MAX_JSON_DEPTH + 1),
            "]".repeat(MAX_JSON_DEPTH + 1)
        );
        assert!(matches!(
            validate_json_budget(deep.as_bytes()),
            Err(AppError::PayloadTooLarge(_))
        ));
        assert!(validate_json_budget(br#"{"safe":[1,true,null,"hello"]}"#).is_ok());
        assert!(validate_json_budget(br#"{"invalid":}"#).is_err());
        assert_eq!(rules(&["needle"]).redact_content(&many), MASK);
    }
    #[test]
    fn personal_rules_never_censor_unconsented_other_owners_or_hide_org_matches() {
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let personal_id = Uuid::new_v4();
        let rules = Redactor::build(
            1,
            vec![
                (personal_id, SecretValue("abcdef".into()), Some(owner)),
                (Uuid::new_v4(), SecretValue("abc".into()), None),
            ],
        )
        .unwrap();
        assert_eq!(rules.scoped_for(owner).redact_str("abcdef"), MASK);
        assert_eq!(
            rules.scoped_for(other).redact_str("abcdef"),
            format!("{MASK}def")
        );
        assert!(!rules
            .scoped_for(other)
            .matching_version_ids("abcdef")
            .contains(&personal_id));
        let low =
            Redactor::build(1, vec![(personal_id, SecretValue("1".into()), Some(owner))]).unwrap();
        assert_eq!(low.scoped_for(other).redact_str("12345"), "12345");
        assert!(low.scoped_for(other).is_empty());
        assert!(low.organization_only().is_empty());
        let mut untouched = serde_json::json!({"type":"text_delta","text":"1"});
        low.scoped_for(other).redact_json(&mut untouched);
        assert_eq!(untouched["text"], "1");
        assert_eq!(
            low.scoped_for(owner).redact_str("12345"),
            format!("{MASK}2345")
        );
    }
    #[test]
    fn explicit_reader_catalog_retains_scope_without_cross_user_rule_leakage() {
        let owner = Uuid::new_v4();
        let reader = Uuid::new_v4();
        let other = Uuid::new_v4();
        let version = Uuid::new_v4();
        let rules = Redactor::build(
            1,
            vec![
                (version, SecretValue("shared-value".into()), Some(owner)),
                (version, SecretValue("shared-value".into()), Some(reader)),
            ],
        )
        .unwrap();
        for user in [owner, reader] {
            assert_eq!(rules.scoped_for(user).redact_str("shared-value"), MASK);
        }
        assert_eq!(
            rules.scoped_for(other).redact_str("shared-value"),
            "shared-value"
        );
        assert_eq!(
            rules.matching_version_ids("shared-value"),
            BTreeSet::from([version])
        );
    }
}

/// A content owner's matcher view; sharing access alone does not register
/// another person's private values as censorship rules for this user's data.
pub fn sanitize_for_user<T: serde::Serialize + serde::de::DeserializeOwned>(
    pool: &crate::db::DbPool,
    user: Uuid,
    input: T,
) -> Result<T, AppError> {
    // A replay producer can already hold a pooled cursor while awaiting this
    // consumer. Never wait for that same exhausted pool at the send boundary.
    let mut conn = pool.try_get().ok_or(AppError::ServiceUnavailable(
        "Content authorization is busy; reconnect shortly",
    ))?;
    let rules = snapshot_for_user(&mut conn, user)?;
    drop(conn);
    if rules.is_empty() {
        return Ok(input);
    }
    let mut value = serde_json::to_value(input).map_err(|_| unavailable())?;
    rules.redact_transport_json(&mut value);
    serde_json::from_value(value).map_err(|_| unavailable())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvelopeContext {
    Native,
    Transport,
    Message,
    Metadata,
    Counters,
    Muse,
}

// These are the portal's closed native projection boundary. The hybrid Codex
// and Muse/Antigravity outer envelopes have no single upstream SDK enum; unknown
// future tags deliberately fall back to strict data redaction until supported.
fn known_codex_event(tag: &str) -> bool {
    matches!(
        tag,
        "thread.started"
            | "turn.started"
            | "turn.completed"
            | "turn.failed"
            | "item.started"
            | "item.updated"
            | "item.completed"
            | "hook/completed"
            | "turn/diff/updated"
            | "item/fileChange/patchUpdated"
            | "turn/plan/updated"
            | "item/plan/delta"
            | "item/reasoning/summaryPartAdded"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/agentMessage/delta"
            | "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta"
            | "thread/compacted"
    )
}
fn known_muse_tag(tag: &str) -> bool {
    matches!(
        tag,
        "runtime.command.accepted"
            | "session.run.linked"
            | "turn.input.user"
            | "run.lifecycle.started"
            | "run.model.configured"
            | "tool.result"
            | "run.output.delta"
            | "run.usage"
            | "run.terminal.completed"
            | "run.terminal.failed"
            | "run.terminal.cancelled"
            | "task.stream.linked"
            | "task.lifecycle.proposed"
            | "task.lifecycle.accepted"
            | "task.lifecycle.started"
            | "task.lifecycle.scheduled"
            | "task.lifecycle.side_effect_intent"
            | "task.lifecycle.completed"
            | "task.lifecycle.failed"
    )
}
fn native_tag(value: &serde_json::Value) -> Option<&str> {
    let tag = value
        .get("type")
        .or_else(|| value.get("method"))?
        .as_str()?;
    if matches!(tag, "assistant" | "user") {
        let message = value.get("message").is_some_and(|message| {
            message.get("role").and_then(serde_json::Value::as_str) == Some(tag)
                && message
                    .get("type")
                    .is_none_or(|kind| kind.as_str() == Some("message"))
                && message
                    .get("content")
                    .is_some_and(|content| content.is_array() || content.is_string())
        });
        if !(message
            || tag == "user"
                && value
                    .get("content")
                    .is_some_and(serde_json::Value::is_string))
        {
            return None;
        }
    }
    if tag == "muse_record"
        && !value
            .get("payload_type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(known_muse_tag)
    {
        return None;
    }
    if tag == "system"
        && !value
            .get("subtype")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|subtype| {
                !matches!(
                    shared::SystemSubtype::from(subtype),
                    shared::SystemSubtype::Unknown(_)
                )
            })
    {
        return None;
    }
    (known_codex_event(tag)
        || matches!(
            tag,
            "assistant"
                | "user"
                | "system"
                | "result"
                | "error"
                | "control_request"
                | "control_response"
                | "control_cancel_request"
                | "stream_event"
                | "rate_limit_event"
                | "tool_progress"
                | "command_lifecycle"
                | "auth_status"
                | "tool_use_summary"
                | "prompt_suggestion"
                | "conversation_reset"
                | "progress"
                | "queue-operation"
                | "portal"
                | "muse_record"
                | "muse_decode_error"
                | "muse_wire_serialize_error"
                | "antigravity_event"
                | "antigravity_turn_completed"
                | "message_start"
                | "message_delta"
                | "message_stop"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
                | "text_delta"
                | "thinking_delta"
                | "input_json_delta"
                | "signature_delta"
                | "text"
                | "thinking"
                | "redacted_thinking"
                | "image"
                | "video"
                | "document"
                | "file"
                | "figure"
                | "tool_use"
                | "server_tool_use"
                | "mcp_tool_use"
                | "tool_result"
                | "mcp_tool_result"
                | "agent_message"
                | "agentMessage"
                | "reasoning"
                | "command_execution"
                | "commandExecution"
                | "file_change"
                | "fileChange"
                | "mcp_tool_call"
                | "mcpToolCall"
                | "dynamic_tool_call"
                | "dynamicToolCall"
                | "function_call"
                | "web_search"
                | "webSearch"
                | "todo_list"
                | "todoList"
                | "user_message"
                | "userMessage"
                | "contextCompaction"
                | "collabAgentToolCall"
                | "imageGeneration"
                | "plan"
                | "hookPrompt"
                | "sleep"
        ))
    .then_some(tag)
}
fn structural_field(context: EnvelopeContext, tag: &str, key: &str, muse: Option<&str>) -> bool {
    if context == EnvelopeContext::Counters {
        return false;
    }
    if key == "type" || key == "method" {
        return true;
    }
    if key == "payload_type" {
        return context == EnvelopeContext::Transport || muse.is_some_and(known_muse_tag);
    }
    if tag == "portal" || matches!(tag, "text" | "thinking" | "redacted_thinking") {
        return false;
    }
    if key == "name" {
        return matches!(
            tag,
            "tool_use"
                | "server_tool_use"
                | "mcp_tool_use"
                | "mcp_tool_call"
                | "mcpToolCall"
                | "dynamic_tool_call"
                | "dynamicToolCall"
                | "function_call"
        );
    }
    if matches!(context, EnvelopeContext::Muse | EnvelopeContext::Metadata)
        && matches!(key, "kind" | "task_kind" | "outcome")
    {
        return true;
    }
    if context == EnvelopeContext::Transport && (key.ends_with("_id") || key.ends_with("Id")) {
        return true;
    }
    if key == "id"
        && context == EnvelopeContext::Native
        && (known_codex_event(tag)
            || matches!(
                tag,
                "assistant"
                    | "user"
                    | "system"
                    | "result"
                    | "error"
                    | "stream_event"
                    | "muse_record"
                    | "antigravity_event"
                    | "antigravity_turn_completed"
            ))
    {
        return false;
    }
    matches!(
        key,
        "session_id"
            | "sessionId"
            | "user_id"
            | "owner_user_id"
            | "account_id"
            | "thread_id"
            | "threadId"
            | "turn_id"
            | "turnId"
            | "item_id"
            | "itemId"
            | "tool_use_id"
            | "toolCallId"
            | "tool_call_id"
            | "parent_tool_use_id"
            | "parentToolUseId"
            | "parent_task_id"
            | "task_id"
            | "run_id"
            | "stream_id"
            | "record_id"
            | "causation_id"
            | "command_id"
            | "call_id"
            | "request_id"
            | "client_msg_id"
            | "trajectory_id"
            | "conversation_id"
            | "model_id"
            | "profile_id"
            | "provider_id"
            | "user_message_uuid"
            | "user_message_uuids"
            | "id"
            | "uuid"
            | "session_key"
            | "model"
            | "role"
            | "status"
            | "subtype"
            | "phase"
            | "agent_type"
            | "tool_name"
            | "toolName"
            | "timestamp"
            | "createdAt"
            | "startedAt"
            | "completedAt"
            | "created_at"
            | "source_created_at"
            | "started_at"
            | "completed_at"
            | "last_created_at"
            | "recorded_at"
            | "record_type"
            | "durability"
            | "step_kind"
            | "state"
            | "stage"
            | "stop_reason"
            | "media_type"
            | "source_type"
    )
}
fn known_data_field(key: &str) -> bool {
    matches!(
        key,
        "message"
            | "content"
            | "event"
            | "params"
            | "payload"
            | "input"
            | "arguments"
            | "result"
            | "tool_result"
            | "raw"
            | "data"
            | "text"
            | "thinking"
            | "delta"
            | "partial_json"
            | "output"
            | "source"
            | "usage"
            | "modelUsage"
            | "model_usage"
            | "rate_limit_info"
            | "error"
            | "summary"
            | "metadata"
            | "update"
            | "request"
            | "response"
            | "command"
            | "changes"
            | "code"
            | "patch"
            | "diff"
            | "description"
            | "filename"
            | "name"
            | "run_stream"
            | "task_stream"
            | "correlation_facts"
            | "entries"
            | "messages"
            | "sessions"
            | "events"
            | "records"
            | "items"
    )
}

impl Redactor {
    /// Only known provider/projection tags enter the structural policy. This
    /// is the safety boundary for native frames, not a protocol classifier:
    /// unknown frames are fully semantic data and cannot exempt `type`/`id`.
    /// Free-text strings are never promoted to envelopes by this walker.
    pub fn redact_agent_json(&self, value: &mut serde_json::Value) {
        if self.is_empty() {
            return;
        }
        if native_tag(value).is_none() {
            self.redact_json(value);
            return;
        }
        self.walk_envelope(value, EnvelopeContext::Native, 0);
    }
    /// Caller-owned typed transport/REST framing. Content members cross back
    /// into the native/data boundary rather than inheriting transport trust.
    pub fn redact_transport_json(&self, value: &mut serde_json::Value) {
        if !self.is_empty() {
            self.walk_envelope(value, EnvelopeContext::Transport, 0);
        }
    }
    fn walk_envelope(&self, value: &mut serde_json::Value, context: EnvelopeContext, depth: usize) {
        if depth > MAX_JSON_DEPTH {
            self.redact_json(value);
            return;
        }
        suppress_delta_payloads(value);
        match value {
            serde_json::Value::Object(object) => {
                let tag = object
                    .get("type")
                    .or_else(|| object.get("method"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let muse_tag = object
                    .get("payload_type")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                let muse_valid=tag=="muse_record"&&muse_tag.as_deref().is_some_and(known_muse_tag)&&object.get("payload").is_some_and(|payload|{
                    let mut nodes=0;
                    value_budget(payload,&mut nodes,0)&&matches!(muse_codes::MusePayload::from_parts(muse_tag.as_deref().unwrap_or_default(),payload.clone()),Ok(payload) if !matches!(payload,muse_codes::MusePayload::Unknown{..}))
                });
                let record_content = context == EnvelopeContext::Transport
                    && (object.contains_key("role") || object.contains_key("agent_type"));
                let prior = std::mem::take(object);
                for (key, mut child) in prior {
                    let structural = structural_field(context, &tag, &key, muse_tag.as_deref());
                    if structural {
                        object.insert(key, child);
                        continue;
                    }
                    let transport = context == EnvelopeContext::Transport;
                    // Each history record has its own budget. Never erase a
                    // batch's pagination/terminal frame due to aggregate nodes.
                    if transport
                        && matches!(
                            key.as_str(),
                            "entries" | "messages" | "sessions" | "events" | "records" | "items"
                        )
                    {
                        if let serde_json::Value::Array(records) = &mut child {
                            for record in records {
                                self.walk_envelope(record, EnvelopeContext::Transport, depth + 1);
                            }
                            object.insert(key, child);
                            continue;
                        }
                    }
                    match key.as_str() {
                        "content" if transport => {
                            if record_content && child.is_string() {
                                if let Some(text) = child.as_str() {
                                    child =
                                        serde_json::Value::String(self.redact_agent_content(text));
                                }
                            } else {
                                self.redact_agent_json(&mut child);
                            }
                        }
                        "content" => {
                            if let serde_json::Value::Array(blocks) = &mut child {
                                let mut nodes = 0;
                                if !blocks
                                    .iter()
                                    .all(|block| value_budget(block, &mut nodes, 0))
                                {
                                    *blocks = vec![serde_json::json!({"type":"text","text":MASK})];
                                } else {
                                    for block in blocks {
                                        self.redact_agent_json(block);
                                    }
                                }
                            } else {
                                self.redact_json(&mut child);
                            }
                        }
                        "input" | "arguments" | "tool_result" | "raw" | "data" => {
                            self.redact_json(&mut child)
                        }
                        "result" if !transport => self.redact_json(&mut child),
                        "message"
                            if matches!(tag.as_str(), "assistant" | "user" | "message_start") =>
                        {
                            self.walk_envelope(&mut child, EnvelopeContext::Message, depth + 1)
                        }
                        "item" | "content_block"
                            if known_codex_event(&tag)
                                || matches!(
                                    tag.as_str(),
                                    "content_block_start" | "content_block_delta"
                                ) =>
                        {
                            self.redact_agent_json(&mut child)
                        }
                        "event" if tag == "stream_event" => self.redact_agent_json(&mut child),
                        "params" if known_codex_event(&tag) => {
                            self.walk_envelope(&mut child, EnvelopeContext::Metadata, depth + 1)
                        }
                        "payload" if muse_valid => {
                            self.walk_envelope(&mut child, EnvelopeContext::Muse, depth + 1)
                        }
                        "payload" if transport => self.redact_agent_json(&mut child),
                        "payload" => self.redact_json(&mut child),
                        "run_stream" | "task_stream" | "event" | "correlation_facts"
                            if context == EnvelopeContext::Muse =>
                        {
                            self.walk_envelope(&mut child, EnvelopeContext::Muse, depth + 1)
                        }
                        "meta" if transport => {
                            self.walk_envelope(&mut child, EnvelopeContext::Metadata, depth + 1)
                        }
                        "meta" => self.redact_json(&mut child),
                        "usage" | "modelUsage" | "model_usage" | "rate_limit_info" => {
                            if value_budget(&child, &mut 0, 0) {
                                self.walk_envelope(
                                    &mut child,
                                    EnvelopeContext::Counters,
                                    depth + 1,
                                );
                            } else {
                                child = serde_json::json!({});
                            }
                        }
                        "source" | "delivery" if context == EnvelopeContext::Metadata => {
                            self.walk_envelope(&mut child, EnvelopeContext::Metadata, depth + 1);
                        }
                        "delta"
                            if matches!(tag.as_str(), "message_delta" | "content_block_delta") =>
                        {
                            if native_tag(&child).is_some() {
                                self.redact_agent_json(&mut child);
                            } else {
                                self.walk_envelope(
                                    &mut child,
                                    EnvelopeContext::Metadata,
                                    depth + 1,
                                );
                            }
                        }
                        "source" if matches!(tag.as_str(), "image" | "document") => {
                            self.walk_envelope(&mut child, EnvelopeContext::Metadata, depth + 1)
                        }
                        _ => match &mut child {
                            // Generated DTO substructures have no arbitrary
                            // content trust: input/content/raw above are explicit.
                            serde_json::Value::Object(_) | serde_json::Value::Array(_)
                                if transport || context == EnvelopeContext::Counters =>
                            {
                                self.walk_envelope(&mut child, context, depth + 1)
                            }
                            serde_json::Value::String(text) => *text = self.redact_content(text),
                            serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                                self.redact_json(&mut child)
                            }
                            _ => {}
                        },
                    }
                    let output_key = if transport
                        || context == EnvelopeContext::Counters
                        || known_data_field(&key)
                    {
                        key
                    } else {
                        self.redact_str(&key)
                    };
                    object.insert(output_key, child);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    if context == EnvelopeContext::Transport {
                        self.walk_envelope(value, context, depth + 1);
                    } else {
                        self.redact_json(value);
                    }
                }
            }
            serde_json::Value::String(text) => *text = self.redact_content(text),
            _ => {}
        }
    }
    /// Stored native protocol can retain validated framing; arbitrary JSON is
    /// not made trusted merely by having a discriminator-shaped property.
    pub fn redact_agent_content(&self, input: &str) -> String {
        if self.is_empty() {
            return input.into();
        }
        if validate_json_budget(input.as_bytes()).is_err() {
            return self.redact_content(input);
        }
        match serde_json::from_str::<serde_json::Value>(input) {
            Ok(mut value) if native_tag(&value).is_some() => {
                self.redact_agent_json(&mut value);
                serde_json::to_string(&value).unwrap_or_else(|_| MASK.into())
            }
            _ => self.redact_content(input),
        }
    }
    pub fn redact_manifest(&self, manifest: &mut archive_format::SessionArchiveManifest) {
        for field in [
            &mut manifest.owner_email,
            &mut manifest.session_name,
            &mut manifest.working_directory,
            &mut manifest.hostname,
        ] {
            *field = self.redact_str(field);
        }
        for text in [
            &mut manifest.owner_name,
            &mut manifest.git_branch,
            &mut manifest.repo_url,
            &mut manifest.pr_url,
            &mut manifest.client_version,
            &mut manifest.launcher_version,
        ]
        .into_iter()
        .flatten()
        {
            *text = self.redact_str(text);
        }
        for argument in &mut manifest.claude_args {
            *argument = self.redact_str(argument);
        }
        if let Some(members) = &mut manifest.members {
            for member in members {
                member.email = self.redact_str(&member.email);
            }
        }
    }
}

#[cfg(test)]
mod typed_envelope_tests {
    use super::*;
    fn rules(secret: &str) -> Redactor {
        Redactor::build(1, vec![(Uuid::new_v4(), SecretValue(secret.into()), None)]).unwrap()
    }
    #[test]
    fn short_value_preserves_generated_protocol_identity_but_masks_text() {
        let rules = rules("a");
        let mut frame = serde_json::json!({"type":"assistant","uuid":"a-generated-id","session_id":"a-session","model":"claude","status":"active","created_at":"2026-11-10T00:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"a"}]}});
        let before = frame.clone();
        rules.redact_agent_json(&mut frame);
        for key in [
            "type",
            "uuid",
            "session_id",
            "model",
            "status",
            "created_at",
        ] {
            assert_eq!(frame[key], before[key]);
        }
        assert_eq!(frame["message"]["role"], "assistant");
        assert_eq!(frame["message"]["content"][0]["type"], "text");
        assert_eq!(frame["message"]["content"][0]["text"], MASK);
    }
    #[test]
    fn typed_stream_delta_fragments_are_withheld_until_complete_snapshot() {
        let rules = rules("secret");
        for fragment in ["sec", "ret"] {
            let mut frame = serde_json::json!({"type":"stream_event","session_id":"session","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":fragment}}});
            rules.redact_agent_json(&mut frame);
            assert_eq!(frame["event"]["delta"]["text"], MASK);
            assert_eq!(frame["event"]["delta"]["type"], "text_delta");
            assert_eq!(frame["event"]["index"], 1);
        }
        let mut completed = serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"safe secret"}]}});
        rules.redact_agent_json(&mut completed);
        assert_eq!(
            completed["message"]["content"][0]["text"],
            format!("safe {MASK}")
        );
    }
    #[test]
    fn untrusted_input_and_result_keep_full_semantic_key_and_scalar_redaction() {
        let rules = rules("a");
        let mut frame = serde_json::json!({"type":"tool_use","id":"a-public","input":{"a":"a","id":"a"},"result":{"a":"a","id":"a"}});
        rules.redact_agent_json(&mut frame);
        assert_eq!(frame["id"], "a-public");
        for key in ["input", "result"] {
            assert!(frame[key].get("a").is_none());
            assert_eq!(frame[key][MASK], MASK);
            assert_eq!(frame[key]["id"], MASK);
        }
        let numeric = super::typed_envelope_tests::rules("1");
        let mut frame =
            serde_json::json!({"type":"tool_result","seq":1,"count":1,"result":1,"input":true});
        numeric.redact_agent_json(&mut frame);
        assert_eq!(frame["seq"], 1);
        assert_eq!(frame["count"], 1);
        assert_eq!(frame["result"], MASK);
        assert_eq!(frame["input"], true);
    }
}

/// Native/import/internal callers use the same fence as live output. This is
/// deliberately a database transaction, not a sanitize-then-insert pair.
pub fn insert_message(
    conn: &mut PgConnection,
    mut message: crate::models::NewMessage,
) -> Result<crate::models::Message, AppError> {
    use crate::schema::{messages, sessions};
    conn.transaction(|conn| {
        lock_generation(conn)?;
        let owner = sessions::table
            .find(message.session_id)
            .select(sessions::user_id)
            .first::<Uuid>(conn)?;
        message.content = snapshot_for_user(conn, owner)?.redact_agent_content(&message.content);
        diesel::insert_into(messages::table)
            .values(message)
            .get_result(conn)
            .map_err(Into::into)
    })
}
pub fn insert_historical(
    conn: &mut PgConnection,
    mut rows: Vec<crate::models::NewHistoricalMessage>,
) -> Result<(), AppError> {
    use crate::schema::{messages, sessions};
    conn.transaction(|conn| {
        lock_generation(conn)?;
        let mut owners = HashMap::new();
        for row in &mut rows {
            let rules = if let Some(rules) = owners.get(&row.message.session_id) {
                Arc::clone(rules)
            } else {
                let owner = sessions::table
                    .find(row.message.session_id)
                    .select(sessions::user_id)
                    .first::<Uuid>(conn)?;
                let rules = snapshot_for_user(conn, owner)?;
                owners.insert(row.message.session_id, rules.clone());
                rules
            };
            row.message.content = rules.redact_agent_content(&row.message.content);
        }
        for batch in rows.chunks(250) {
            diesel::insert_into(messages::table)
                .values(batch)
                .execute(conn)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod context_boundary_tests {
    use super::*;
    fn rules(secret: &str) -> Redactor {
        Redactor::build(1, vec![(Uuid::new_v4(), SecretValue(secret.into()), None)]).unwrap()
    }
    #[test]
    fn unknown_frames_and_json_text_never_promote_type_or_id_to_trusted_metadata() {
        let rules = rules("REGISTERED_SECRET");
        for text in [
            r#"{"type":"REGISTERED_SECRET","id":"REGISTERED_SECRET"}"#,
            r#"{"type":"assistant","id":"REGISTERED_SECRET"}"#,
        ] {
            let mut native: serde_json::Value = serde_json::from_str(text).unwrap();
            rules.redact_agent_json(&mut native);
            assert!(!native.to_string().contains("REGISTERED_SECRET"));
            assert!(!rules
                .redact_agent_content(text)
                .contains("REGISTERED_SECRET"));
        }
        // Even a fully protocol-shaped string is still user/tool text here.
        let text = r#"{"type":"assistant","id":"REGISTERED_SECRET","message":{"role":"assistant","content":[]}}"#;
        let mut frame = serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":text}]},"extension":{"type":"REGISTERED_SECRET","id":"REGISTERED_SECRET"},"meta":{"type":"REGISTERED_SECRET","id":"REGISTERED_SECRET"},"usage":{"type":"REGISTERED_SECRET","id":"REGISTERED_SECRET"}});
        rules.redact_agent_json(&mut frame);
        assert!(!frame.to_string().contains("REGISTERED_SECRET"));
        let mut tool = serde_json::json!({"type":"tool_use","id":"generated","name":"Bash","input":{"type":"REGISTERED_SECRET","id":"REGISTERED_SECRET"}});
        rules.redact_agent_json(&mut tool);
        assert_eq!(tool["name"], "Bash");
        assert!(!tool.to_string().contains("REGISTERED_SECRET"));
    }
    #[test]
    fn known_claude_tool_and_sdk_validated_muse_payload_preserve_short_value_ids() {
        let rules = rules("a");
        let mut claude = serde_json::json!({"type":"assistant","uuid":"a-message","parent_tool_use_id":"a-parent","session_id":"a-session","message":{"role":"assistant","id":"a-api","content":[{"type":"tool_use","id":"a-tool","name":"Bash","input":{"command":"a"}}]}});
        rules.redact_agent_json(&mut claude);
        assert_eq!(claude["uuid"], "a-message");
        assert_eq!(claude["parent_tool_use_id"], "a-parent");
        assert_eq!(claude["message"]["id"], "a-api");
        assert_eq!(claude["message"]["content"][0]["name"], "Bash");
        assert_eq!(
            claude["message"]["content"][0]["input"]["comm[secret redacted]nd"],
            MASK
        );
        let mut muse = serde_json::json!({"type":"muse_record","payload_type":"task.lifecycle.proposed","stream_id":"a-stream","record_id":"a-record","payload":{"kind":"task.lifecycle","command_id":"a-command","run_stream":{"kind":"run","id":"a-run"},"task_id":"a-task","task_stream":{"kind":"task","id":"a-task-stream"},"event":{"kind":"proposed","task_id":"a-task","task_kind":"tool.command"}}});
        rules.redact_agent_json(&mut muse);
        assert_eq!(muse["payload_type"], "task.lifecycle.proposed");
        assert_eq!(muse["payload"]["task_id"], "a-task");
        assert_eq!(muse["payload"]["event"]["kind"], "proposed");
        assert_eq!(muse["payload"]["event"]["task_kind"], "tool.command");
    }
    #[test]
    fn projected_muse_and_antigravity_deltas_do_not_leave_reconstructable_fragments() {
        let rules = rules("secret");
        for fragment in ["sec", "ret"] {
            let mut muse = serde_json::json!({"type":"muse_record","payload_type":"run.output.delta","stream_id":"s","record_id":"r","payload":{"kind":"run.output.delta","command_id":"c","run_stream":{"kind":"run","id":"run"},"text":fragment}});
            rules.redact_agent_json(&mut muse);
            assert_eq!(muse["payload"]["text"], MASK);
            let mut antigravity = serde_json::json!({"type":"antigravity_event","trajectory_id":"generated","text_delta":fragment,"text":"safe"});
            rules.redact_agent_json(&mut antigravity);
            assert_eq!(antigravity["text_delta"], MASK);
        }
    }
    #[test]
    fn overbudget_terminal_data_preserves_lifecycle_and_transport_batches_budget_each_record() {
        let rules = rules("secret");
        let many = serde_json::Value::Array(
            (0..MAX_JSON_NODES + 1)
                .map(|_| serde_json::json!(0))
                .collect(),
        );
        let mut terminal = serde_json::json!({"type":"result","subtype":"success","session_id":"generated","result":many});
        rules.redact_agent_json(&mut terminal);
        assert_eq!(terminal["type"], "result");
        assert_eq!(terminal["subtype"], "success");
        assert_eq!(terminal["result"], MASK);
        let entries:Vec<_>=(0..128).map(|_|serde_json::json!({"content":{"type":"assistant","session_id":"s","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","id":"tool","input":{"items":vec!["safe";300],"token":"secret"}}]}},"meta":{"created_at":"2026-11-10T00:00:00Z"}})).collect();
        let mut batch = serde_json::json!({"type":"HistoryBatch","entries":entries,"last_created_at":"2026-11-10T00:00:00Z"});
        assert!(!value_budget(&batch, &mut 0, 0));
        rules.redact_transport_json(&mut batch);
        assert_eq!(batch["type"], "HistoryBatch");
        assert_eq!(batch["entries"].as_array().unwrap().len(), 128);
        assert_eq!(batch["last_created_at"], "2026-11-10T00:00:00Z");
        assert!(!batch.to_string().contains("\"secret\""));
        assert!(serde_json::from_value::<shared::ServerToClient>(batch).is_ok());
    }
    #[test]
    fn long_value_build_uses_bounded_nfa_instead_of_auto_dfa() {
        let secret = "x".repeat(65_536);
        let started = std::time::Instant::now();
        let rules = rules(&secret);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "64KiB matcher construction must stay bounded"
        );
        assert_eq!(rules.redact_str(&secret), MASK);
        assert!(rules.contains_bytes(secret.as_bytes()));
    }
}

#[cfg(test)]
mod send_boundary_tests {
    use super::*;
    #[test]
    fn exhausted_replay_pool_fails_fast_instead_of_waiting_for_its_own_producers() {
        let Some(db) = crate::handlers::secrets::tests::Database::new() else {
            return;
        };
        let held: Vec<_> = (0..db.pool.max_size())
            .map(|_| db.pool.get().unwrap())
            .collect();
        let started = std::time::Instant::now();
        let result = sanitize_for_user(
            &db.pool,
            Uuid::new_v4(),
            serde_json::json!({"type":"HelloAck","protocol_version":1,"success":true}),
        );
        assert!(matches!(result, Err(AppError::ServiceUnavailable(_))));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "send-side checkout cannot wait on blocked replay producers"
        );
        drop(held);
    }
}
