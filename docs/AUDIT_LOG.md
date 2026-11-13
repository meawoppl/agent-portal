# Admin audit log

The **Admin → Audit log** tab records three categories only: login/identity,
computers, and secrets. The API (`GET /api/admin/audit`) uses the existing
server-side administrator guard, including bearer-token scope restrictions.
This is an instance-wide admin view, including personal-secret activity metadata;
it grants no access to credential values. Ordinary users cannot query it.

## Storage and retention

`audit_events` is append-only through the application API. IDs have no resource
foreign keys: deleting an account or secret does not erase its evidence. Existing
`secret_audit_events` are copied during migration; new secrets events continue to
write both trails atomically for compatibility. Both have the same retention.

`PORTAL_AUDIT_RETENTION_DAYS` defaults to **365**. A positive integer (1–36500)
sets the time window; the literal **unlimited** disables automatic pruning.
Zero and invalid values fail startup. The worker removes at most 1,000 rows from
each trail per minute; expired history can remain visible while a backlog drains.
Unlimited retention requires ordinary database storage monitoring. There are no
whole-history rebuilds or unbounded API reads. Pages contain at most 100 events,
ordered by timestamp plus ID, with keyset cursors rather than offsets.

## Recorded events

- **Identity:** OAuth login success, rejection and failure; mobile token login;
  logout; Portal access verification; device approval/denial; identity linking
  and unlinking; token issuance, rotation, and revocation.
- **Computers:** authenticated launcher registration (including reconnection),
  update/restart requests and dispatch failures; agent install request/result;
  agent login start, terminal result and cancellation request. Terminal login
  polls are deduplicated by flow ID. Launcher disconnects are not deletion events.
- **Secrets:** create/version replacement, soft delete/restore, sharing changes
  with recipient and permission bits; batch set request/decision and per-item
  creation/update; access request/decision; request expiry/invalidation; binding
  issuance, revocation, ticket issuance/consumption and value release; clipboard
  value release; failed mutation attempts; expunge request and job transitions.

Session/launcher/operation IDs attribute agent approvals and redemption. A
binding ID correlates its redemption stages; access request IDs correlate their
request/decision, and deposit IDs correlate all writes in the batch. Resource
identifiers are expandable, rather than dominating each row.

Database triggers capture identity and token lifecycle changes, sharing history,
request expiry, cascading binding invalidation and cleanup transitions in the
same transaction as their source rows. These automated changes identify the
subject account separately from the actor; they do not pretend the owner initiated
an administrator or background action. Explicit human and agent actions record
those actors. Secret mutation events roll back with failed mutations; failed HTTP
attempts are recorded separately after rollback.

Secrets, tokens, token hashes, authorization codes, ciphertext, provider subjects,
request/response bodies and CLI output are never audit metadata. The API decodes
only the explicitly allowed details fields. Values released for Copy are not proof
of a clipboard write; binding redemption is not proof of command completion.
Update/restart requests likewise do not claim a successful subsequent restart.

Observations after external actions cannot undo those actions if the database is
unavailable. `AUDIT_PERSIST_FAILED` reports such gaps (and failed retention runs)
without logging request payloads. Important secret mutations fail if their audit
transaction fails. The log is not tamper-proof against database administrators.

## Validation

`cargo test -p backend --lib audit::` with `DATABASE_URL` exercises admin access,
transaction rollback, tied-timestamp pagination, filtering, retention, token and
identity metadata, and terminal-poll deduplication. The secrets handler tests also
exercise their new transactional writes/triggers. After `trunk build`, run
`scripts/test-admin-audit.py` with Playwright for desktop/mobile UI checks.
