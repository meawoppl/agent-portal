//! Typed JSONB columns with explicit legacy-read policies.

use diesel::deserialize::{self, FromSql, FromSqlRow};
use diesel::expression::AsExpression;
use diesel::pg::{Pg, PgValue};
use diesel::serialize::{self, IsNull, Output, ToSql};
use diesel::sql_types::Jsonb;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::Write;

/// A JSONB array column read and written as `Vec<T>`, stored and serialized
/// exactly as the bare array (`serde(transparent)`), so neither the column
/// nor the API changes shape.
///
/// A stored value that is not an array of `T` reads as empty, with a
/// warning, rather than failing the whole row. That keeps the leniency these
/// columns always had, so a damaged row stays loadable; it is not harmless
/// for launch arguments, which change how a session runs, hence the warning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, AsExpression, FromSqlRow)]
#[serde(transparent)]
#[diesel(sql_type = Jsonb)]
pub struct JsonVec<T>(pub Vec<T>);

impl<T> Default for JsonVec<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> From<Vec<T>> for JsonVec<T> {
    fn from(items: Vec<T>) -> Self {
        Self(items)
    }
}

impl<T: DeserializeOwned> FromSql<Jsonb, Pg> for JsonVec<T> {
    fn from_sql(value: PgValue<'_>) -> deserialize::Result<Self> {
        let value = <serde_json::Value as FromSql<Jsonb, Pg>>::from_sql(value)?;
        Ok(Self(serde_json::from_value(value).unwrap_or_else(
            |error| {
                tracing::warn!("JSONB list column did not decode, reading it as empty: {error}");
                Vec::new()
            },
        )))
    }
}

impl<T: Serialize + std::fmt::Debug> ToSql<Jsonb, Pg> for JsonVec<T> {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        // JSONB's binary format: a version byte, then the JSON text.
        out.write_all(&[1])?;
        serde_json::to_writer(out, &self.0)?;
        Ok(IsNull::No)
    }
}

/// Notification preferences stored as the unchanged bare JSON object.
///
/// Partial objects use the shared field defaults; unknown fields are ignored
/// on read and omitted on replacement, matching the HTTP replacement contract.
/// Malformed legacy values fall back to the entire default preference set,
/// consistently for user rows, the settings endpoint and push dispatch. Reads
/// never rewrite the stored value. SQL NULL remains None on the user model.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, AsExpression, FromSqlRow,
)]
#[serde(transparent)]
#[diesel(sql_type = Jsonb)]
pub struct NotificationPrefsJson(pub shared::api::NotificationPrefs);

impl FromSql<Jsonb, Pg> for NotificationPrefsJson {
    fn from_sql(value: PgValue<'_>) -> deserialize::Result<Self> {
        let value = <serde_json::Value as FromSql<Jsonb, Pg>>::from_sql(value)?;
        Ok(Self(serde_json::from_value(value).unwrap_or_default()))
    }
}

impl ToSql<Jsonb, Pg> for NotificationPrefsJson {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        out.write_all(&[1])?;
        serde_json::to_writer(out, &self.0)?;
        Ok(IsNull::No)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_preferences_preserve_storage_shape_and_legacy_reads() {
        use crate::schema::users;
        use crate::test_support::{insert_user, shared_pool};
        use diesel::prelude::*;
        use shared::api::NotificationPrefs;
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        conn.test_transaction::<_, diesel::result::Error, _>(|conn| {
            let owner = insert_user(conn, "typed-prefs");
            let load = |conn: &mut diesel::pg::PgConnection| {
                users::table
                    .find(owner.id)
                    .select(users::notification_prefs)
                    .first::<Option<NotificationPrefsJson>>(conn)
            };
            assert_eq!(load(conn)?, None);
            let custom = NotificationPrefs {
                turn_complete: false,
                ..NotificationPrefs::default()
            };
            diesel::update(users::table.find(owner.id))
                .set(users::notification_prefs.eq(Some(NotificationPrefsJson(custom))))
                .execute(conn)?;
            let raw: serde_json::Value = users::table
                .find(owner.id)
                .select(users::notification_prefs.assume_not_null())
                .first(conn)?;
            assert_eq!(raw, serde_json::to_value(custom).unwrap());
            assert_eq!(load(conn)?.unwrap().0, custom);
            for (wire, expected) in [
                (r#"{"turn_complete":false,"future_toggle":true}"#, custom),
                (r#"{"turn_complete":"bad"}"#, NotificationPrefs::default()),
                ("null", NotificationPrefs::default()),
                ("42", NotificationPrefs::default()),
            ] {
                let value: serde_json::Value = serde_json::from_str(wire).unwrap();
                diesel::update(users::table.find(owner.id))
                    .set(users::notification_prefs.eq(Some(value.clone())))
                    .execute(conn)?;
                assert_eq!(load(conn)?.unwrap().0, expected);
                let row = users::table
                    .find(owner.id)
                    .first::<crate::models::User>(conn)?;
                assert_eq!(row.notification_prefs.unwrap().0, expected);
                let retained: serde_json::Value = users::table
                    .find(owner.id)
                    .select(users::notification_prefs.assume_not_null())
                    .first(conn)?;
                assert_eq!(retained, value, "a read must not normalize legacy storage");
            }
            Ok(())
        });
    }

    #[test]
    fn serializes_as_the_bare_array() {
        let args = JsonVec(vec!["--model".to_string(), "opus".to_string()]);
        let wire = serde_json::to_string(&args).unwrap();
        assert_eq!(wire, r#"["--model","opus"]"#);
        assert_eq!(
            serde_json::from_str::<JsonVec<String>>(&wire).unwrap(),
            args
        );
    }

    #[test]
    fn round_trips_through_the_column_and_reads_garbage_as_empty() {
        use crate::schema::sessions;
        use crate::test_support::{insert_session, insert_user, shared_pool};
        use diesel::prelude::*;
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        conn.test_transaction::<_, diesel::result::Error, _>(|conn| {
            let owner = insert_user(conn, "jsonb-columns");
            let session = insert_session(conn, owner.id, "jsonb");
            let prs = vec![shared::PrRef {
                number: 7,
                url: "https://example.com/pr/7".into(),
                branch: "feature".into(),
            }];
            diesel::update(sessions::table.find(session.id))
                .set((
                    sessions::claude_args.eq(JsonVec(vec!["--model".to_string()])),
                    sessions::open_prs.eq(JsonVec(prs.clone())),
                ))
                .execute(conn)?;
            let stored: serde_json::Value = sessions::table
                .find(session.id)
                .select(sessions::claude_args)
                .first(conn)?;
            assert_eq!(stored, serde_json::json!(["--model"]));
            let row = sessions::table
                .find(session.id)
                .first::<crate::models::Session>(conn)?;
            assert_eq!(row.claude_args.0, ["--model"]);
            assert_eq!(row.open_prs.0, prs);

            diesel::update(sessions::table.find(session.id))
                .set((
                    sessions::claude_args.eq(serde_json::json!({"oops": 1})),
                    sessions::open_prs.eq(serde_json::json!([{"number": "seven"}])),
                ))
                .execute(conn)?;
            let row = sessions::table
                .find(session.id)
                .first::<crate::models::Session>(conn)?;
            assert!(row.claude_args.0.is_empty());
            assert!(row.open_prs.0.is_empty());
            Ok(())
        });
    }
}
