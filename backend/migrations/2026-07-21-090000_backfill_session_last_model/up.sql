-- Populate sessions.last_model for sessions that already had turn metrics
-- before the last_model column was added. New turns keep this column current
-- through backend/src/handlers/websocket/turn_metrics.rs.
WITH latest_model AS (
    SELECT DISTINCT ON (tm.session_id)
        tm.session_id,
        LEFT(tm.model, 128) AS model
    FROM turn_metrics tm
    WHERE tm.session_id IS NOT NULL
      AND tm.model IS NOT NULL
      AND btrim(tm.model) <> ''
      AND lower(btrim(tm.model)) <> 'unknown'
    ORDER BY
        tm.session_id,
        COALESCE(tm.completed_at, tm.started_at) DESC,
        tm.started_at DESC,
        tm.id DESC
)
UPDATE sessions s
SET last_model = latest_model.model
FROM latest_model
WHERE s.id = latest_model.session_id
  AND s.last_model IS NULL;
