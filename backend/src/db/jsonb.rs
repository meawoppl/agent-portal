//! Typed JSONB list columns.

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
/// warning, rather than failing the whole row: these columns are optional
/// context (launch arguments, open PRs) and a session must stay loadable.
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

#[cfg(test)]
mod tests {
    use super::*;

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
