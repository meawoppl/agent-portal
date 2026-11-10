# Codex billing and resume

The app-server driver reads `account/usage/read` with the active thread ID.
Only `nativeUsageUsdMicros` supplies USD cost; credit estimates never substitute
for native billing. ChatGPT plans and older servers may omit this value, which
means unknown cost rather than zero.

A fresh thread starts from zero. Resumed and forked threads establish a billing
baseline before accepting turns. Each completed main-thread turn subtracts the
previous observed counter. Missing readings and counter resets break that
baseline: later accumulated spend is not attributed to an unrelated turn.
Queries have a two-second deadline and cannot prevent normal turn completion.
The query's latency does not inflate measured turn duration.

Codex reports a stable metric UUID. The backend inserts the metric and adds its
known native cost to the session total in one transaction; replaying the same
metric cannot add it twice. Claude session cost uses its separate result ledger.
Older drivers without metric IDs retain database-assigned identities and do not
supply native Codex billing. Historical costs are not backfilled. Metric reports
remain best effort, so missing reports cannot be reconstructed from this path;
`TURN_METRICS_PERSIST_FAILED` identifies database accounting failures requiring
reconciliation from retained billing evidence.

A failed resume never silently creates a replacement native conversation.
Provider mismatch, missing configuration and provider-policy conflicts show the
structured reason and recovery action. Restore the original provider/mapping,
use a compatible computer, or explicitly fork a new portal session to start
fresh. Imported/restored sessions also stop if their required mapping is missing.
An ordinary unmapped session may be a never-launched reconciliation target; it
starts fresh with an explicit notice. The existing portal transcript remains
visible; it does not itself restore native
agent context. Turn-start provider failures use the same actionable messages.
