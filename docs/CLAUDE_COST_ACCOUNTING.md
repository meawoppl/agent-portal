# Claude cost accounting

Claude `model_usage_since_last_result` reports disjoint call costs. The driver
sums its per-model `costUSD` fields for each turn; an empty map means zero, not
a missing value. Invalid costs are unavailable. Calls that never reach a result
(for example, an agent killed mid-turn) cannot be recovered from this field.

The backend adds these deltas to the portal session's lifetime cost. A durable
`claude_result_cost_ledger` records each result UUID and updates the total in
the same transaction. Replayed results cannot add cost again after a backend
restart or after transcript retention. The ledger is removed when the session
is deleted. Incrementing the existing total in SQL preserves concurrent results.

CLIs without the delta field use cumulative snapshots. The driver differences
snapshots within its CLI process for turn metrics. A new process has a fresh
baseline; a WebSocket reconnect does not reset it. The backend keeps a
nondecreasing cumulative compatibility estimate for legacy results and results
without a stable UUID. It cannot reconstruct exact legacy lifetime cost across
process resumes because those snapshots lack an unambiguous process boundary.
Use a current Claude CLI for exact disjoint-result accounting.

If accounting fails after transcript persistence, the backend emits
`COST_ACCOUNTING_PERSIST_FAILED`. The ledger and total roll back together, but
transport acknowledgement does not automatically retry that accounting
transaction. Restore database/schema health and reconcile the affected result
UUIDs against the retained transcript and ledger before retrying. This is not
an exactly-once guarantee during a database outage.

## Existing data and deployment boundary

The ledger migration marks result UUIDs already present in the hot transcript
as accounted, without changing historical session or turn-metric costs. A proxy
replaying those buffered results during deployment must not charge them again.
Archive viewing and history import do not replay historical results into live
accounting. The ledger retains identities for new results after their transcript
rows are trimmed.

Historical turn metrics can contain cumulative costs, and historical session
costs may have dropped on resume. This change does not invent a numerical
backfill: hot retention may have removed the evidence needed to reconstruct
those totals. Treat analytics spanning this deployment as potentially affected.
Only correct old costs from a complete retained transcript/archive with verified
result identities, process boundaries and metric-to-result correspondence;
legacy cumulative values must not be summed. Results trimmed before the ledger
migration are not seeded, so historical transcript records must never be injected
as newly emitted live results.
