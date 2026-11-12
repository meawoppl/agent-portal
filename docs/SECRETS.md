# Secrets service

Portal stores immutable, encrypted secret versions in PostgreSQL. The web page manages metadata, sharing, replacement and cleanup without retrieving saved values. Human CLI reads require a separately approved credential; ordinary session, launcher and linked-account credentials do not provide general vault access.

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

Member discovery (`agent-portal secret members`, `GET /api/secrets/members`, and `POST /api/secrets/members/resolve`) requires `ALLOWED_EMAIL_DOMAIN` and a verified provider email in that domain for the caller. Open-signup portals, email-only allowlists, unverified/legacy profile addresses, and out-of-domain allowlist exceptions receive 403. Exact lookup has the same restriction so it cannot enumerate accounts. Administrators retain their separate admin-only user-management endpoint. Direct grants to an already-known Portal UUID remain available; this restriction does not change secret access rights.

Legacy provider assertions have no fabricated verification timestamp. Sign in again to refresh a provider-qualified email/handle; the Portal UUID remains available for existing users. The browser directory labels provider identities and marks unverified addresses.

## Human CLI access

First log in normally, then authorize a dedicated secret credential:

```sh
agent-portal login
agent-portal secret authorize --label 'My development workstation'
```

Approve the displayed code on the Secrets authorization page. Approval requires the matching browser account and an OAuth sign-in within ten minutes. A long-lived Portal cookie or a token-login does not substitute for that recent sign-in. Dedicated credentials are backend-scoped, expire after 30 days, and are stored in an owner-only local file. The Secrets page lists and revokes these approvals without exposing credential hashes or tokens. Disabled accounts and revoked credentials lose subsequent access.

```sh
agent-portal secret set personal/DEPLOY_TOKEN
agent-portal secret set org/DEPLOY_TOKEN --stdin < token.txt
agent-portal secret members
agent-portal secret share personal/DEPLOY_TOKEN --user meawoppl@github --access read,use
agent-portal secret list
agent-portal secret info personal/DEPLOY_TOKEN
agent-portal secret versions personal/DEPLOY_TOKEN
agent-portal secret read personal/DEPLOY_TOKEN --version 1 --raw
agent-portal secret run --env DEPLOY_TOKEN=personal/DEPLOY_TOKEN -- deploy-tool
```

Interactive values use hidden input. Stdin preserves UTF-8 bytes, including newlines; `--trim-newline` is explicit. Values are never accepted in a `--value` argument. Read emits exactly the value bytes on stdout; diagnostics go to stderr. Prefer `run` to avoid putting a value in terminal scrollback or an agent tool result. Process injection uses literal environment variables without shell interpolation or secret values in argv. The child process and the local OS account can inspect those variables. The local OS account is a trust boundary: owner-only files do not isolate mutually hostile processes running under that same account.

Read/use, write and manage are independent rights. Arbitrary `run --env` requires both read and use, because a caller controlling the child can print its environment. A reader cannot re-share through Portal without manage permission. Revocation does not retract a previously delivered value or rotate a provider credential.

## Versions and deletion

Every replacement appends an encrypted immutable version and advances the current pointer. Older versions remain available for explicit authorized CLI reads. The web page shows version metadata only; no saved-value reveal, copy, prefix, suffix or length-dependent mask is provided.

```sh
agent-portal secret rotate personal/DEPLOY_TOKEN --stdin < replacement.txt
agent-portal secret delete personal/DEPLOY_TOKEN
agent-portal secret list --deleted
agent-portal secret info personal/DEPLOY_TOKEN --deleted
agent-portal secret restore personal/DEPLOY_TOKEN --confirm-sharing
```

Delete adds the exact reserved `deleted` system tag, records the actor/time, and invalidates access bindings. It removes no version and reserves the name. Deleted keys appear in a separate masked Deleted view under the same authorization policy. Normal and historical value reads are denied while deleted. Browser sharing and restore also require a recent OAuth sign-in, since they can delegate access to retained values. Restore preserves identity/history, requires explicit confirmation of its sharing audience, and never revives old tickets or process approvals. Creating a previously deleted name fails until it is restored or another name is chosen.

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


The request uses only the injected session credential; it cannot fall back to a human's CLI vault credential. It creates a durable **Approve / Deny** card in that session. The card fetches authenticated server metadata rather than trusting transcript text, and shows the exact secret/version, owner, target session/launcher, environment variable and requested duration. It never retrieves a saved value. Only the local browser-authenticated session owner can decide; approval also requires a recent sign-in. Session viewers, editors, linked users, launchers and the requesting agent cannot approve themselves.

Requests expire after ten minutes. Approval creates one binding pinned to the requesting process, secret version and ACL generation; approval does not expose a value to the browser. A rotation, deletion, revoked credential, replaced process or changed access cannot silently revive a stale request. Denial and expiry grant nothing. Repeated requests with the same request UUID replay the original outcome without creating duplicate cards or bindings. Use `--request-id UUID` when retrying a command whose response was lost. Archive and linked-portal views show inert history; reconnecting live threads fetch current server state before enabling controls.

Approving lets the agent access the value in its process environment: it is not a promise that an agent controlling a child process cannot print or copy it. The requested approval lasts at most one hour and can be revoked through the existing binding controls.

A human with read/use rights may approve an existing owned session and connected owned launcher:

```sh
agent-portal secret bind --secret personal/DEPLOY_TOKEN \
  --session SESSION_UUID --launcher LAUNCHER_UUID --env DEPLOY_TOKEN --expires-in 3600
agent-portal secret bindings
agent-portal secret unbind BINDING_UUID
```

The approval pins the secret version, ACL generation and active proxy credential. It lasts at most one hour. Ordinary session credentials cannot create approvals. Within the approved session:

```sh
agent-portal secret run --binding BINDING_UUID -- deploy-tool
```

This path uses only the bound session context, without falling back to a human vault credential. It mints a 30-second ticket, redeems it once, and injects the value directly into the child. Tickets are hashed in PostgreSQL and never printed or placed in transcript/pending-input records. Ticket consumption, process/session/launcher identity, expiry, disabled users, rights and deletion are checked before and after decryption. Relaunching with a different proxy credential requires a new human approval. Rotation or delete/restore invalidates the old generation; it does not silently switch a pinned execution to a new value.

## Redaction and archive cleanup

Owner-scoped rules protect personal values. Merely sharing a value does not let its owner alter another person's records: a recipient becomes a redaction subject only after an explicit read/use. Organization secrets and explicitly curator-approved global cleanup rules apply organization-wide. Historical values continue to participate in their approved scope.

Matching covers raw values, ordinary JSON string escaping, base64 and bounded URL decoding. Streaming text/tool-argument delta payloads are withheld while applicable rules exist; completed records are sanitized. Provider/control identifiers remain intact, and explicit untrusted tool inputs/results receive semantic JSON redaction. Short or common values may mask unrelated text in the approved scope. Arbitrary transformations, screenshots and model/provider logs are not certified by matching.

```sh
agent-portal secret expunge id/SECRET_UUID --all-versions --wait
agent-portal secret expunge id/SECRET_UUID --all-versions --global --wait
agent-portal secret job JOB_UUID
```

The default cleans the requester's own sessions and archives. Cross-user cleanup requires an organization curator who also has management access to the key. Its approval captures the exact retained version IDs; a later personal rotation cannot expand the destructive scope without fresh curator approval. Global policy tombstones also protect send-time replay buffers, rather than relying only on rewriting SQL rows.

Expunge never deletes encrypted vault history. It redacts content-bearing hot/queued/derived records, streams affected compressed NDJSON transcripts, updates manifest text metadata, removes contaminated media in full, and deletes superseded S3 versions rather than relying on delete markers. Per-session database fencing coordinates archive publishers, cleanup and late media write-through across backend processes. Required input/output persistence commits before delivery/ACK; failed secure persistence is explicitly rejected or left unacknowledged for retry.

Legacy archives remain readable as v1. Cleanup uses bounded records, a JSON node/depth budget and disk-spooled compression, without a whole-transcript JSON tree. Coverage is keyed by target secret/version policy and a fingerprint of the full manifest, so unchanged retries skip archive bodies without confusing one secret with another. Coverage fingerprints use an ephemeral HMAC key; a backend restart intentionally makes the inventory cold. Media scanning is bounded and checks overlap across read chunks. Current archive readers have two concurrent streaming slots; cleanup has one worker per backend and a durable job lease. Oversized, malformed, locked or inaccessible targets are reported blocked rather than declared erased.

A receipt distinguishes active-storage work, blocked targets and backup expiry. PostgreSQL WAL/dead tuples, old database backups, object locks/replicas, unknown transformations, rendered text inside opaque images/video, provider/native histories and previously downloaded copies require separate retention or remediation. An update/VACUUM or current-object overwrite is not proof of physical historical erasure. Revoke/rotate a leaked provider credential independently of archive cleanup.

## Operations and validation

Watch `SECRETS_REDACTION_REFRESH_FAILED`, `SECRET_REDACTION_FAILED` and `SECRET_EXPUNGE_FAILED` alongside the existing archive markers. A stale matcher causes protected reads/writes to fail closed until refresh, rather than logging or publishing raw content. Mutation concurrency, request-body buffers, key loading, matcher sources, JSON trees, archive spools and S3 pagination/deletion are bounded. Metadata responses use `Cache-Control: no-store`; sensitive API errors do not echo request/response bodies.

Tests use isolated PostgreSQL schemas and synthetic local encryption keys. Coverage includes ciphertext authentication/identity substitution, immutable versions, namespace reservation, deletion/restore, provider resolution, scoped permissions, fresh enrollment, credential revocation, idempotency, binding replay/process replacement/expiry, reader consent, rollback/ACK fencing, target-specific archive coverage, source timestamps/order, media resurrection and unsafe-record blocking. S3 request tests use local HTTP fixtures, including paginated version IDs, null versions, races and HTTP-200 per-object delete failures; they do not modify production storage.

## Inject secret references into a file

Keep references in a UTF-8 template and materialize its values locally:

```text
DB_PASSWORD={{ secret://org/database-password }}
API_TOKEN={{ secret://personal/api-token }}
STABLE_KEY={{ secret://id/01234567-89ab-4cde-8012-3456789abcde }}
```

```bash
agent-portal secret inject --in-file config.env.tpl --out-file config.env
```

`secret://personal/NAME`, `secret://org/NAME`, and `secret://id/UUID` use the same
identities as ordinary secret references. IDs are UUIDs, not sequential numbers.
The URI prefix also works in existing reference-taking commands. Template names
are literal (including spaces); percent-encoding, URI query strings, and field
selectors are not interpreted. Only `{{ secret://... }}` placeholders expand;
other template expressions and bare URIs remain unchanged.

For an agent, first request the credentials together, then supply the resulting
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
An agent context never falls back to a human CLI credential. Outside an agent
context, injection uses the separately authorized human CLI credential and
requires both read and use rights.

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
