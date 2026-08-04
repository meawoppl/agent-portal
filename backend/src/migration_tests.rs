//! Data-cleanup migration harness (#922).
//!
//! Cleanup migrations delete or rewrite rows by predicate, and those
//! predicates are easy to get subtly wrong around null / blank / sentinel
//! values. The existing migration checks cover naming and compile-time schema
//! use, not *behavior* — nothing asserts that a cleanup keeps the rows it
//! should and drops the rows it should.
//!
//! This harness seeds representative rows, applies a migration's cleanup
//! predicate, and asserts which rows survive. It is deliberately scoped to a
//! throwaway user so it never touches real data, and the user's `ON DELETE
//! CASCADE` tears the seeded rows down with it.
//!
//! First case: `2026-05-29-143758_drop_unknown_model_turn_metrics`, which
//! removes `turn_metrics` rows whose `model` is null, blank, or `unknown`.

#![cfg(test)]

use diesel::prelude::*;
use diesel::sql_query;

use crate::models::{NewUser, User};
use crate::test_support::shared_pool;

/// The cleanup predicate from `2026-05-29-143758_drop_unknown_model_turn_metrics`.
///
/// Kept verbatim (minus the leading `DELETE FROM turn_metrics WHERE`) so the
/// assertions below exercise the migration's real semantics — case-folded,
/// whitespace-trimmed, null-aware. If the migration's predicate changes, this
/// must change with it.
const UNKNOWN_MODEL_PREDICATE: &str =
    "model IS NULL OR btrim(model) = '' OR lower(btrim(model)) = 'unknown'";

/// Seed one `turn_metrics` row for `user_id` with the given `model`.
///
/// Only `model` varies; the other NOT NULL columns get fixed placeholders and
/// the nullable ones default. `session_id` is left NULL — it still carries an
/// FK to `sessions`, so seeding a value would drag in an unrelated session
/// fixture the cleanup does not care about. Raw SQL because `NewTurnMetric`'s
/// `session_id` is non-optional and so cannot express the NULL.
fn seed_metric(conn: &mut PgConnection, user_id: uuid::Uuid, model: Option<&str>) {
    sql_query(
        "INSERT INTO turn_metrics \
         (session_id, user_id, agent_type, model, started_at, created_at, \
          input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, \
          thinking_tokens, subagent_tokens, is_error, tool_call_count, stream_restarts) \
         VALUES (NULL, $1, 'claude', $2, now(), now(), 0, 0, 0, 0, 0, 0, false, 0, 0)",
    )
    .bind::<diesel::sql_types::Uuid, _>(user_id)
    .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(model.map(str::to_string))
    .execute(conn)
    .expect("seed turn_metric");
}

/// Seed `models`, run the unknown-model cleanup scoped to a throwaway user, and
/// return the models that survived (sorted for a stable assertion).
///
/// The scoping `user_id = $1` is the harness's safety belt: the migration's
/// real DELETE is table-wide, but here we bound it to seeded rows so a test
/// run can never delete another test's — or real — data.
fn survivors_after_unknown_model_cleanup(models: &[Option<&str>]) -> Option<Vec<String>> {
    #[derive(QueryableByName)]
    struct ModelRow {
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        model: Option<String>,
    }

    let pool = shared_pool()?;
    let conn = &mut pool.get().expect("conn");

    // Throwaway owner; its ON DELETE CASCADE cleans up the seeded rows.
    let user: User = diesel::insert_into(crate::schema::users::table)
        .values(NewUser {
            email: format!("migration_harness_{}@example.invalid", uuid::Uuid::new_v4()),
            name: None,
            avatar_url: None,
        })
        .get_result(conn)
        .expect("insert throwaway user");

    for model in models {
        seed_metric(conn, user.id, *model);
    }

    sql_query(format!(
        "DELETE FROM turn_metrics WHERE user_id = $1 AND ({UNKNOWN_MODEL_PREDICATE})"
    ))
    .bind::<diesel::sql_types::Uuid, _>(user.id)
    .execute(conn)
    .expect("run cleanup predicate");

    let survivors: Vec<String> =
        sql_query("SELECT model FROM turn_metrics WHERE user_id = $1 ORDER BY model NULLS FIRST")
            .bind::<diesel::sql_types::Uuid, _>(user.id)
            .load::<ModelRow>(conn)
            .expect("load survivors")
            .into_iter()
            .filter_map(|r| r.model)
            .collect();

    // Teardown: deleting the user cascades to its turn_metrics.
    diesel::delete(crate::schema::users::table.find(user.id))
        .execute(conn)
        .expect("cleanup throwaway user");

    Some(survivors)
}

#[test]
fn drops_null_blank_and_unknown_models_keeps_real_ones() {
    let Some(survivors) = survivors_after_unknown_model_cleanup(&[
        None,                // NULL           → dropped
        Some(""),            // empty          → dropped
        Some("   "),         // whitespace     → dropped (btrim)
        Some("unknown"),     // sentinel       → dropped
        Some("UNKNOWN"),     // case-folded    → dropped
        Some("  Unknown  "), // trimmed+folded → dropped
        Some("claude-opus-4-8"),
        Some("gpt-5"),
    ]) else {
        return; // no DATABASE_URL: DB-gated test skips, keeping CI green
    };

    let mut survivors = survivors;
    survivors.sort();
    assert_eq!(
        survivors,
        vec!["claude-opus-4-8".to_string(), "gpt-5".to_string()],
        "only real model names should survive the cleanup"
    );
}

/// A model that merely *contains* the word must not be swept — the predicate
/// is an exact (trimmed, folded) match, not a substring one.
#[test]
fn a_model_named_like_unknown_is_not_swept() {
    let Some(survivors) =
        survivors_after_unknown_model_cleanup(&[Some("unknown-9b"), Some("well-known")])
    else {
        return;
    };

    let mut survivors = survivors;
    survivors.sort();
    assert_eq!(
        survivors,
        vec!["unknown-9b".to_string(), "well-known".to_string()]
    );
}
