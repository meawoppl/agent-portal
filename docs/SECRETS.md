# Secrets service

Portal stores immutable, encrypted secret versions in PostgreSQL. The web page manages metadata, sharing, replacement and deletion. An explicit verified Copy action retrieves a value only for the browser clipboard. Humans use the OAuth-authenticated web UI; there is no human CLI vault access. Agents receive only explicitly approved, process-bound access. Ordinary session, launcher and linked-account credentials do not provide general vault access.

## Configure encryption

Production and development use an externally injected master-key file. Store a randomly generated **32-byte raw binary key** as a document in 1Password, and have your deployment retrieve it into a private runtime file. Mount that file read-only into the backend and set `PORTAL_SECRETS_KEY_FILE=/run/secrets/portal-vault.key`. Only the path goes in the environment; never place the key bytes in environment variables, command arguments, container images, source control or logs. Portal does not fetch 1Password credentials itself.

The file must be a regular file owned by the backend's Unix user, with mode `0400` or `0600`; symlinks, shared permissions, missing files and lengths other than exactly 32 bytes are rejected. Set the host/container UID consistently when injecting the file. The backend loads it once at startup into zeroizing process memory. This works in production without `--dev-mode`, cloud credentials, a cloud region or a key-management API.

Each value version gets a fresh random data key and nonce. AES-256-GCM encrypts the value and wraps its data key under the injected master key, authenticating immutable organization/secret/version identity at both layers. PostgreSQL stores ciphertext, authentication data and the wrapped data key; it never stores the master key. The Portal cookie/JWT key is not used for vault encryption. Names, descriptions and sharing metadata are not confidential fields; keep credentials in the value field.

Keep the master key in 1Password permanently, separate from database and host backups. Database backups alone cannot decrypt the vault. A running backend with access to the injected key can: this is server-managed encryption. Losing the master key makes all its retained versions unrecoverable. Replacing the file with a new random key is **not** a rotation procedure: existing versions would fail to decrypt. Restore the original key before restart; keep the same key across backend instances and restores. No key is generated automatically and there is no fallback provider.

Without `PORTAL_SECRETS_KEY_FILE`, vault reads/writes are unavailable. Existing ciphertext still requires current redaction rules before protected content can be served. A configured invalid or unreadable file fails startup; an incorrect key fails ciphertext authentication. Backend health remains available while the bounded background rule loader warms up. Bulk warming and the zeroizing source cache remain bounded; ordinary saved-value reads decrypt and recheck authorization afterward.

S3 archive cleanup is independent of vault encryption. Its archive role needs `s3:ListBucketVersions`, `s3:DeleteObjectVersion` and current-object read/write/delete as appropriate; missing permissions produce blocked cleanup receipts. It uses regional SigV4 and the current HTTPS client. A local archive needs no AWS services or credentials.

Initial limits are 64 KiB per nonempty UTF-8 value, 128-character names, 4,096 retained versions, 8,192 explicit reader registrations, and a combined 1 MiB budget for retained value bytes plus weighted reader matcher sources. These limits bound privileged plaintext matching memory; all versions still remain stored. Capacity is checked before a new version or reader registration commits. There is no automatic history expiry. Sharing is capped at 64 grants per key and 8,192 grants per organization; ACL history survives revocation and soft deletion. Raising these limits requires corresponding matcher-memory measurements.

## Names and identity

References are explicit:

| Reference | Meaning |
| --- | --- |
| `personal/DEPLOY_TOKEN` | The authenticated Portal user's namespace |
| `org/DEPLOY_TOKEN` | The organization namespace |
| `id/UUID` | An exact secret, including one shared by another user |

Names are case-sensitive. Bare names are rejected. Personal and organization names may coexist. Sharing grants access to the same secret UUID; it does not copy the secret into the recipient's personal namespace. Deleted names stay reserved in their original namespace.

One Portal deployment is one organization, containing its active local users. Organization secrets are available to current and future active members; Portal administrators are the designated organization-secret curators. Administration does not confer read access to another person's personal secrets. Session sharing and federation do not confer vault rights.

Grants are attached to stable Portal user IDs. Login identities remain keyed by the provider and immutable provider subject, so linked Google/GitHub logins and renamed GitHub accounts do not move grants. Select recipients using a Portal UUID, `google:matt@cosmicfrontier.org`, a verified unambiguous email, or `meawoppl@github` / `github:meawoppl`. Display names are not accepted as selectors. GitHub handles are checked against GitHub's current immutable ID before resolution; stale, reassigned, ambiguous and unregistered selections are refused. Resolution never creates or links an account.

Browser member discovery (`GET /api/secrets/members` and `POST /api/secrets/members/resolve`) requires `ALLOWED_EMAIL_DOMAIN` and a verified provider email in that domain for the caller. Open-signup portals, email-only allowlists, unverified/legacy profile addresses, and out-of-domain allowlist exceptions receive 403. Exact lookup has the same restriction so it cannot enumerate accounts. Administrators retain their separate admin-only user-management endpoint. Direct grants to an already-known Portal UUID remain available; this restriction does not change secret access rights.

Legacy provider assertions have no fabricated verification timestamp. Sign in again to refresh a provider-qualified email/handle; the Portal UUID remains available for existing users. The browser directory labels provider identities and marks unverified addresses.

## Human access: web UI only

Open **Settings → Secrets** after signing in through the Portal's configured
OAuth provider. Manage names, versions, sharing, deletion and restoration there. Explicit Copy and approval operations additionally
require the browser-bound Portal account verification and same-origin CSRF
checks. Verification checks the current OAuth-backed Portal session; it is not
a separate computer approval, fresh provider login, or MFA challenge.

There is no human CLI enrollment, local vault-token file, or approved-computer
list. General vault endpoints require the browser cookie and reject all bearer
credentials, including old `aps_secret_` tokens and normal Portal CLI tokens.
The old `/api/secrets/authorize/*` endpoints and `/api/secrets/{id}/read` return
410 Gone. Deployment revokes historical CLI tokens and pending enrollments;
revocation is not undone on migration rollback. Old local token files are no
longer read and require no per-computer cleanup to block access.

OAuth authenticates the acting account; the approval card records consent and
scope. A process that controls that browser or OS account remains within the
same trust boundary. Approval UI does not provide process isolation.

Read/use, write and manage remain independent rights. Approval is scoped to the
specific secret versions, requesting session/process, environment variables and
lifetime shown in the card. A reader cannot re-share through Portal without
manage permission. Revocation does not retract previously delivered values or
rotate the provider credential.

## Versions and deletion

In Settings → Secrets, click **Check Portal access** before the inventory loads. Secrets initially sort by name across Personal and Organization scopes. Click any column heading to sort in either direction; Reference and Shared with are inventory columns. Dates appear in the expanded version history. Select a name to expand its details and actions directly below that row; select it again to collapse. **Show deleted secrets** adds deleted entries to the same list.

Every replacement appends an encrypted immutable version and advances the current pointer. Older encrypted versions remain retained; metadata is available in the web UI and agent requests can explicitly pin a version. The web page shows version metadata only. Select an active secret and use **Copy secret** after verifying the Portal session to copy its displayed current version. The server requires read permission, the browser-bound verification cookie and same-origin CSRF headers, and rechecks authorization after asynchronous decryption and reader registration. Copy never renders the value in the page or stores it in component state; success appears only after the clipboard write completes. Browser clipboard permission is also required. Clipboard contents may persist or be synchronized by the operating system; revocation cannot recall a copied value. The retired CLI read endpoint grants no access to any caller.


Deletion uses the same two-click confirmation as closing sessions: click **Delete secret**, then **Click again to confirm**. Closing or switching the expanded secret cancels confirmation. A failed deletion requires both clicks again.

Delete adds the exact reserved `deleted` system tag, records the actor/time, and invalidates access bindings. It removes no version and reserves the name. Deleted keys appear alongside active entries when **Show deleted secrets** is checked under the same authorization policy. Normal and historical value reads are denied while deleted. Browser sharing and restore also require the verified OAuth-backed browser session, since they can delegate access to retained values. Restore preserves identity/history, requires explicit confirmation of its sharing audience, and never revives old tickets or process approvals. Creating a previously deleted name fails until it is restored or another name is chosen.

## Approved session execution

Agents can ask for access directly in the session thread:

```sh
agent-portal secret request personal/DEPLOY_TOKEN --env DEPLOY_TOKEN --expires-in 900 --wait
# After the owner approves, the metadata response includes binding_id:
agent-portal secret run --binding BINDING_UUID -- deploy-tool
```

Request several credentials in **one approval card** with repeated `--credential` mappings:

```bash
agent-portal secret request \
  --credential DEPLOY_TOKEN=personal/DEPLOY_TOKEN \
  --credential API_KEY=org/API_KEY \
  --expires-in 900 --wait
# Use the binding_id of each item in the metadata response:
agent-portal secret run --binding FIRST_UUID --binding SECOND_UUID -- deploy-tool
```

The card lists every credential, pinned version, environment variable and lifetime.
One owner approval commits the entire set; a failing member rolls back all new
bindings. Individual members cannot be approved separately. The request accepts
1–16 credentials and rejects duplicate environment variables (case-insensitively).
A retry must use the same `--request-id`, item order and arguments. The group
response contains `items`, each with its own binding ID; it never contains values.
Execution redeems every binding before starting the child, and starts nothing
if a redemption fails or environments collide. This is atomic command startup,
not a guarantee that credentials cannot be revoked after redemption.


The request uses only the injected session credential; it cannot fall back to any general vault credential. It creates a durable **Approve / Deny** card in that session. The card fetches authenticated server metadata rather than trusting transcript text, and shows the exact secret/version, owner, target session/launcher, environment variable and requested duration. It never retrieves a saved value. Only the local browser-authenticated session owner can decide; approval also requires the explicit Portal session verification. Session viewers, editors, linked users, launchers and the requesting agent cannot approve themselves.

Click **Approve** once: the card verifies the Portal session server-side, submits the decision, then confirms its status. Inline progress shows verification and approval without navigation or a separate verification button. Failed verification sends no approval. Background polling cannot consume the click or replace the result with stale data, and an uncertain decision is checked rather than automatically submitted again.

Requests expire after ten minutes. Approval creates one binding pinned to the requesting process, secret version and ACL generation; approval does not expose a value to the browser. A rotation, deletion, revoked credential, replaced process or changed access cannot silently revive a stale request. Denial and expiry grant nothing. Repeated requests with the same request UUID replay the original outcome without creating duplicate cards or bindings. Use `--request-id UUID` when retrying a command whose response was lost. Archive and linked-portal views show inert history; reconnecting live threads fetch current server state before enabling controls.

Approving lets the agent access the value in its process environment: it is not a promise that an agent controlling a child process cannot print or copy it. The requested approval lasts at most one hour and can be revoked through the existing binding controls.

The OAuth-authenticated owner with read/use rights approves the request in the session thread.

Approved access stays pinned to the selected secret version, ACL generation and
active agent process. It lasts at most one hour. Session credentials can request
access but cannot approve themselves.

This path uses only the bound session context, without any general vault credential. It mints a 30-second ticket, redeems it once, and injects the value directly into the child. Tickets are hashed in PostgreSQL and never printed or placed in transcript/pending-input records. Ticket consumption, process/session/launcher identity, expiry, disabled users, rights and deletion are checked before and after decryption. Relaunching with a different proxy credential requires a new human approval. Rotation or delete/restore invalidates the old generation; it does not silently switch a pinned execution to a new value.

## Redaction and archive cleanup

Owner-scoped rules protect personal values. Merely sharing a value does not let its owner alter another person's records: a recipient becomes a redaction subject only after an explicit read/use. Organization secrets and explicitly curator-approved global cleanup rules apply organization-wide. Historical values continue to participate in their approved scope.

Matching covers raw values, ordinary JSON string escaping, base64 and bounded URL decoding. Streaming text/tool-argument delta payloads are withheld while applicable rules exist; completed records are sanitized. Provider/control identifiers remain intact, and explicit untrusted tool inputs/results receive semantic JSON redaction. Short or common values may mask unrelated text in the approved scope. Arbitrary transformations, screenshots and model/provider logs are not certified by matching.

Archive cleanup controls are not exposed in the normal **Settings → Secrets** inventory. A separate administrator interface is planned. Existing cleanup receipt links remain readable.


The default cleans the requester's own sessions and archives. Cross-user cleanup requires an organization curator who also has management access to the key. Its approval captures the exact retained version IDs; a later personal rotation cannot expand the destructive scope without fresh curator approval. Global policy tombstones also protect send-time replay buffers, rather than relying only on rewriting SQL rows.

Expunge never deletes encrypted vault history. It redacts content-bearing hot/queued/derived records, streams affected compressed NDJSON transcripts, updates manifest text metadata, removes contaminated media in full, and deletes superseded S3 versions rather than relying on delete markers. Per-session database fencing coordinates archive publishers, cleanup and late media write-through across backend processes. Required input/output persistence commits before delivery/ACK; failed secure persistence is explicitly rejected or left unacknowledged for retry.

Legacy archives remain readable as v1. Cleanup uses bounded records, a JSON node/depth budget and disk-spooled compression, without a whole-transcript JSON tree. Coverage is keyed by target secret/version policy and a fingerprint of the full manifest, so unchanged retries skip archive bodies without confusing one secret with another. Coverage fingerprints use an ephemeral HMAC key; a backend restart intentionally makes the inventory cold. Media scanning is bounded and checks overlap across read chunks. Current archive readers have two concurrent streaming slots; cleanup has one worker per backend and a durable job lease. Oversized, malformed, locked or inaccessible targets are reported blocked rather than declared erased.

A receipt distinguishes active-storage work, blocked targets and backup expiry. PostgreSQL WAL/dead tuples, old database backups, object locks/replicas, unknown transformations, rendered text inside opaque images/video, provider/native histories and previously downloaded copies require separate retention or remediation. An update/VACUUM or current-object overwrite is not proof of physical historical erasure. Revoke/rotate a leaked provider credential independently of archive cleanup.

## Operations and validation

Watch `SECRETS_REDACTION_REFRESH_FAILED`, `SECRET_REDACTION_FAILED` and `SECRET_EXPUNGE_FAILED` alongside the existing archive markers. A stale matcher causes protected reads/writes to fail closed until refresh, rather than logging or publishing raw content. Mutation concurrency, request-body buffers, key loading, matcher sources, JSON trees, archive spools and S3 pagination/deletion are bounded. Metadata responses use `Cache-Control: no-store`; sensitive API errors do not echo request/response bodies.

Tests use isolated PostgreSQL schemas and synthetic local encryption keys. Coverage includes ciphertext authentication/identity substitution, immutable versions, namespace reservation, deletion/restore, provider resolution, scoped permissions, retired enrollment/token rejection, browser-only access, idempotency, binding replay/process replacement/expiry, reader consent, rollback/ACK fencing, target-specific archive coverage, source timestamps/order, media resurrection and unsafe-record blocking. S3 request tests use local HTTP fixtures, including paginated version IDs, null versions, races and HTTP-200 per-object delete failures; they do not modify production storage.

## Inject secret references into a file

Keep references in a UTF-8 template and materialize its values locally:

```text
DB_PASSWORD={{ secret://org/database-password }}
API_TOKEN={{ secret://personal/api-token }}
STABLE_KEY={{ secret://id/01234567-89ab-4cde-8012-3456789abcde }}
```


`secret://personal/NAME`, `secret://org/NAME`, and `secret://id/UUID` use the same
identities as ordinary secret references. IDs are UUIDs, not sequential numbers.
The URI prefix also works in agent secret requests. Template names
are literal (including spaces); percent-encoding, URI query strings, and field
selectors are not interpreted. Only `{{ secret://... }}` placeholders expand;
other template expressions and bare URIs remain unchanged.

First request the credentials together, then supply the resulting
**group request ID** (not a member binding ID):

```bash
agent-portal secret request \
  --credential DB_PASSWORD=secret://org/database-password \
  --credential API_TOKEN=secret://personal/api-token \
  --credential STABLE_KEY=secret://id/01234567-89ab-4cde-8012-3456789abcde --wait
agent-portal secret inject --approval GROUP_REQUEST_UUID \
  --in-file config.env.tpl --out-file config.env
```

Every template reference must match exactly one credential in that approved
group. The existing process-bound tickets, pinned versions and expiry checks
still apply. The environment-variable labels used during approval are checked
against the redeemed bindings; the template selects secrets by reference.
Injection always requires an active bound agent context and the explicitly
approved group. There is no human-token or unapproved fallback.

Substitution is literal: no shell expansion, recursive interpolation, or
JSON/YAML/INI escaping occurs. Choose the template syntax appropriately for the
secret's value. Repeated identical references are resolved once. Templates are
limited to 1 MiB, 16 unique secrets and 4096 placeholders; rendered output is
limited to 8 MiB. Values and rendered content are never printed.

All lookups and rendering complete before a temporary file is written beside
the destination and atomically published. Existing files require `--force`;
input and output must be different files, and output symlinks/special files are
rejected. Failed resolution leaves an existing output unchanged. Files use mode
0600 on Unix; Windows uses the destination directory's ACL. The rendered file
contains plaintext and persists after binding expiry or revocation: keep it out
of version control and remove it when no longer needed.

## Agent-proposed secret deposits

An agent holding a user-provided credential can request creation or replacement of secrets without
getting general vault access. The owner reviews one combined card, edits names
or descriptions, optionally removes recipients, and clicks **Approve** once.
The card verifies Portal access server-side, shows progress, then saves the
whole batch atomically. A name conflict, disabled recipient, or expired request
changes nothing. Existing names append an immutable version after explicit owner confirmation; no read binding is
created for the agent. Updates pin the current version and permission generation, so concurrent changes reject the entire batch. Existing sharing is preserved, and update targets cannot be renamed in the card. Organization proposals require a curator and retain the
normal organization-wide read/use audience.

Single value, read from a local file (or use `--stdin`):

```sh
agent-portal secret set personal/AWS_ACCESS_KEY_ID \
  --value-file /secure/path/access-key-id \
  --description 'AWS automation access key ID' \
  --share google:jazz@cosmicfrontier.org --wait
```

For several values, provide a JSON array through `--batch-file` or
`--batch-stdin`. Each object has `namespace` (`personal` or `organization`),
`name`, `value`, optional `description`, and optional `share` selectors. For
example, a locally prepared file can contain both an AWS access key ID and its
secret access key. Do not paste the file contents into a prompt or tool output.

```sh
agent-portal secret set --batch-file /secure/path/credentials.json --wait
agent-portal secret set --status REQUEST_UUID
```

The old `propose` spelling remains a compatibility alias. Omitted descriptions
preserve the current description on updates.

New selected-user grants provide only **read/use**, never write/manage. Existing
grants retain their permissions and expiry; changing those belongs in Settings. Selectors
accept `google:EMAIL`, a verified email, or an existing Portal user UUID, using
the same verified organization-directory boundary as the web UI. GitHub handles
must first be resolved to a stable Portal UUID in the web UI. Sharing metadata
is pinned to the resolved user and displayed before approval. An owner may
remove recipients during review; adding recipients requires a new proposal.

Batches contain 1–16 values, each 1–65536 UTF-8 bytes, at most 256 KiB total and
400000 bytes of JSON input. Values are preserved exactly, including newlines.
There are at most four pending batches per owner, 128 globally, and 30 new
batches per owner per hour. Proposals expire after ten minutes. Denial clears
the staged ciphertext immediately; a 30-second sweeper clears expired staging.
Database backups can retain earlier encrypted records under their retention
policy. Approval moves the encrypted values into immutable vault versions and
clears staging in the same transaction.

The agent receives only proposal metadata. The transcript stores an opaque
proposal ID; secret values are excluded from approval cards, receipts, and
error messages. Only the submitting session/process can query its receipt, and
only the owning browser can decide. A replaced/disconnected agent process
cannot get its pending proposal approved. Reusing `--request-id` returns the
original immutable proposal, never updates its contents. Use `--status` after
an uncertain network result rather than creating a new proposal blindly.
