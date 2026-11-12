//! Vault reads require dedicated CLI authority or explicitly verified browser copy access.
pub(crate) mod archive_versions;
pub mod auth;
pub mod crypto;
pub mod expunge;
pub mod redaction;
pub mod store;
use crate::{
    db::DbPool,
    errors::AppError,
    schema::{secret_versions, secrets, secrets_generation},
};
use crypto::{EnvelopeCrypto, Identity};
use diesel::prelude::*;
use shared::api::SecretValue;
use std::sync::Arc;
/// Only immutable matcher source bytes are cached; authorization and value
/// read results never use this cache. Bound both bytes and cardinality.
#[derive(Default)]
struct SourceCache {
    bytes: usize,
    values: std::collections::BTreeMap<uuid::Uuid, Arc<crypto::SensitiveValue>>,
}
#[derive(Clone)]
pub struct SecretsService {
    pub mutation_gate: Arc<tokio::sync::Semaphore>,
    pub body_gate: Arc<tokio::sync::Semaphore>,
    pub crypto: Option<EnvelopeCrypto>,
    pub redaction: Arc<redaction::RedactionRuntime>,
    sources: Arc<tokio::sync::Mutex<SourceCache>>,
}
impl SecretsService {
    pub fn new(crypto: Option<EnvelopeCrypto>) -> Self {
        Self {
            mutation_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            body_gate: Arc::new(tokio::sync::Semaphore::new(8)),
            crypto,
            redaction: Arc::new(redaction::RedactionRuntime::new()),
            sources: Arc::new(tokio::sync::Mutex::new(SourceCache::default())),
        }
    }
    pub fn disabled() -> Self {
        Self::new(None)
    }
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self::new(EnvelopeCrypto::from_env()?))
    }
    pub fn crypto(&self) -> Result<&EnvelopeCrypto, AppError> {
        self.crypto.as_ref().ok_or(AppError::ServiceUnavailable(
            "Secrets encryption is not configured",
        ))
    }
    async fn prewarm(
        &self,
        sources: &mut SourceCache,
        rows: Vec<(store::VersionRow, uuid::Uuid)>,
    ) -> Result<(), AppError> {
        use futures_util::{StreamExt, TryStreamExt};
        let missing: Vec<_> = rows
            .into_iter()
            .filter(|(row, _)| !sources.values.contains_key(&row.id))
            .collect();
        let bytes = missing
            .iter()
            .map(|(row, _)| row.encrypted_value.len())
            .sum::<usize>();
        if sources.values.len().saturating_add(missing.len()) > 4096
            || sources.bytes.saturating_add(bytes) > 1024 * 1024
        {
            return Err(AppError::ServiceUnavailable(
                "Secrets matcher cache capacity exceeded",
            ));
        }
        let crypto = self.crypto()?;
        let loaded: Vec<_> =
            futures_util::stream::iter(missing.into_iter().map(|(row, organization)| async move {
                let value = crypto
                    .open(
                        Identity {
                            organization,
                            secret: row.secret_id,
                            version: row.id,
                        },
                        &row.envelope(),
                    )
                    .await
                    .map_err(|_| {
                        AppError::ServiceUnavailable("Secrets key provider unavailable")
                    })?;
                Ok::<_, AppError>((row.id, Arc::new(value)))
            }))
            .buffer_unordered(8)
            .try_collect()
            .await?;
        for (id, value) in loaded {
            sources.bytes += value.bytes().len();
            sources.values.insert(id, value);
        }
        Ok(())
    }
    async fn source(
        &self,
        sources: &mut SourceCache,
        row: &store::VersionRow,
        organization: uuid::Uuid,
    ) -> Result<Arc<crypto::SensitiveValue>, AppError> {
        if let Some(value) = sources.values.get(&row.id) {
            return Ok(value.clone());
        }
        let value = Arc::new(
            self.crypto()?
                .open(
                    Identity {
                        organization,
                        secret: row.secret_id,
                        version: row.id,
                    },
                    &row.envelope(),
                )
                .await
                .map_err(|_| AppError::ServiceUnavailable("Secrets key provider unavailable"))?,
        );
        if sources.values.len() >= 4096
            || sources.bytes.saturating_add(value.bytes().len()) > 1024 * 1024
        {
            return Err(AppError::ServiceUnavailable(
                "Secrets matcher cache capacity exceeded",
            ));
        }
        sources.bytes += value.bytes().len();
        sources.values.insert(row.id, value.clone());
        Ok(value)
    }
    /// Privileged expunge matching is intentionally restricted to one requested
    /// key. It never folds unrelated private rules into another user's job.
    /// The authorized job worker coordinates its own generation fence.
    pub async fn redactor_for_secret(
        &self,
        pool: &DbPool,
        id: uuid::Uuid,
    ) -> Result<Arc<redaction::Redactor>, AppError> {
        self.redactor_for_secret_versions(pool, id, None).await
    }
    /// Global cleanup must supply the immutable version IDs approved by its
    /// curator. Later personal rotations cannot expand that approved scope.
    pub async fn redactor_for_secret_versions(
        &self,
        pool: &DbPool,
        id: uuid::Uuid,
        approved: Option<&[uuid::Uuid]>,
    ) -> Result<Arc<redaction::Redactor>, AppError> {
        let (organization, generation, rows) = {
            let mut conn = pool.get()?;
            let organization = secrets::table
                .find(id)
                .select(secrets::organization_id)
                .first::<uuid::Uuid>(&mut conn)?;
            let generation = secrets_generation::table
                .find(1i16)
                .select(secrets_generation::generation)
                .first::<i64>(&mut conn)?;
            let mut query = secret_versions::table
                .filter(secret_versions::secret_id.eq(id))
                .into_boxed();
            if let Some(approved) = approved {
                if approved.is_empty() {
                    return Err(AppError::BadRequest("No vault versions were approved"));
                };
                query = query.filter(secret_versions::id.eq_any(approved));
            }
            let rows = query.load::<store::VersionRow>(&mut conn)?;
            if let Some(approved) = approved {
                let count = approved
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
                if count != rows.len() {
                    return Err(AppError::ServiceUnavailable(
                        "Approved vault versions unavailable",
                    ));
                }
            }
            (organization, generation, rows)
        };
        let mut sources = self.sources.lock().await;
        self.prewarm(
            &mut sources,
            rows.iter()
                .cloned()
                .map(|row| (row, organization))
                .collect(),
        )
        .await?;
        let mut values = Vec::with_capacity(rows.len());
        for row in rows {
            let value = self.source(&mut sources, &row, organization).await?;
            let text = String::from_utf8(value.bytes().to_vec())
                .map_err(|_| AppError::ServiceUnavailable("Invalid vault value encoding"))?;
            values.push((row.id, SecretValue(text), None));
        }
        Ok(Arc::new(redaction::Redactor::build(generation, values)?))
    }
    /// Install all retained values, deleted keys included. A generation recheck
    /// prevents publishing a matcher assembled across concurrent version writes.
    pub async fn refresh_redaction(&self, pool: &DbPool) -> Result<(), AppError> {
        {
            let mut conn = pool.get()?;
            if self.redaction.snapshot(&mut conn).is_ok() {
                return Ok(());
            }
        }
        let mut sources = self.sources.lock().await;
        for _ in 0..3 {
            let (organization, generation, rows, readers, global) = {
                let mut conn = pool.get()?;
                let (org, generation) = secrets_generation::table
                    .find(1i16)
                    .select((
                        secrets_generation::organization_id,
                        secrets_generation::generation,
                    ))
                    .first::<(uuid::Uuid, i64)>(&mut conn)?;
                let rows = secret_versions::table
                    .inner_join(secrets::table.on(secrets::id.eq(secret_versions::secret_id)))
                    .select((
                        store::VersionRow::as_select(),
                        secrets::organization_id,
                        secrets::owner_user_id,
                        secrets::namespace,
                    ))
                    .load::<(store::VersionRow, uuid::Uuid, uuid::Uuid, String)>(&mut conn)?;
                use crate::schema::secret_occurrences as occurrences;
                let readers = occurrences::table
                    .filter(occurrences::record_kind.eq("reader"))
                    .select((occurrences::rule_version_id, occurrences::record_id))
                    .load::<(uuid::Uuid, String)>(&mut conn)?;
                let mut index = std::collections::HashMap::<uuid::Uuid, Vec<uuid::Uuid>>::new();
                for (version, user) in readers {
                    if let Ok(user) = user.parse() {
                        index.entry(version).or_default().push(user)
                    }
                }
                let global = occurrences::table
                    .filter(occurrences::record_kind.eq("global_redaction"))
                    .select(occurrences::rule_version_id)
                    .distinct()
                    .load::<uuid::Uuid>(&mut conn)?
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>();
                (org, generation, rows, index, global)
            };
            self.prewarm(
                &mut sources,
                rows.iter()
                    .map(|(row, org, _, _)| (row.clone(), *org))
                    .collect(),
            )
            .await?;
            let mut values = Vec::with_capacity(rows.len());
            for (row, org, owner, namespace) in rows {
                let value = self.source(&mut sources, &row, org).await?;
                let text = String::from_utf8(value.bytes().to_vec())
                    .map_err(|_| AppError::ServiceUnavailable("Invalid vault value encoding"))?;
                let global_scope = namespace == "organization" || global.contains(&row.id);
                if !global_scope {
                    for reader in readers
                        .get(&row.id)
                        .into_iter()
                        .flatten()
                        .filter(|reader| **reader != owner)
                    {
                        values.push((row.id, SecretValue(text.clone()), Some(*reader)));
                    }
                }
                values.push((
                    row.id,
                    SecretValue(text),
                    if global_scope { None } else { Some(owner) },
                ));
            }
            let latest = secrets_generation::table
                .find(1i16)
                .select(secrets_generation::generation)
                .first::<i64>(&mut *pool.get()?)?;
            if latest == generation {
                return self.redaction.install(organization, generation, values);
            }
        }
        Err(AppError::ServiceUnavailable("Secret rules changed; retry"))
    }
}
