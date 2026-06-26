# Portal metadata sidecar — design & plan

**Status:** proposed (Claude + Codex co-design)
**Owners:** backend/shared → Claude · frontend → Codex
**Tracking versions:** TBD per slice (coordinate to avoid parallel-PR collisions)

## 1. Problem

Portal-specific message metadata (attribution, sender, server timestamp,
delivery tracking) is **flattened into the agent's own JSON blob** as
`_`-prefixed keys, on every delivery path, and read back on the frontend by
`#[serde(rename = "_origin")]`-style string keys.

The metadata already exists server-side as **typed `messages` columns**
(`provenance_kind` / `provenance_session_id` / `provenance_agent_type`,
`created_at`, `user_id`, `agent_type`, `role`). We then *re-flatten* those
columns into the content blob in **four independent places**:

| Path | Site | Injects |
|------|------|---------|
| Live broadcast | `frontend/.../session_view/websocket.rs:164/174/180` | `_sender`, `_created_at`, `_origin` |
| WS history replay | `backend/.../websocket/replay.rs:119/133/138` | `_sender`, `_created_at`, `_origin` |
| HTTP history load | `frontend/.../session_view/helpers.rs:320…` (`inject_message_metadata`) | `_sender`, `_created_at`, `_origin` |
| Optimistic / delivery | frontend `component.rs`, `agent_frame.rs` | `_client_msg_id`, `_delivery_stage`, `_delivery_message`, `_pending` |

The renderer (`frontend/src/components/message_renderer/types.rs`) then fishes
these back out by string key.

### Why this is the bug we keep hitting

`_origin` is a **convention, not a type**. The four injection sites must stay
byte-identical, forever, with no compiler enforcement. When one drifts — or a
browser runs a cached bundle whose injection predates a field — attribution
silently degrades to a generic "PORTAL" card (the inter-agent render gap, #1082
→ #1130). The data was correct in the DB the whole time; it was lost in the
flatten/re-parse round-trip.

Additional smell: `normalize_output_content` (`message_handlers.rs:349`)
**mutates the source-of-truth blob** — it rewrites a typed
`PortalContent::AgentMessage` down to plain `Text` and moves the attribution to
columns. The stored `content` is therefore no longer the raw agent message,
which complicates replay and makes the frontend depend on the re-injected
sidecar to reconstruct what it already had.

## 2. Goal

Bifurcate cleanly at the **wire + render boundary** (the DB is already a
raw-blob + sidecar-columns split, so no migration is required):

- `content` = the **raw agent JSON, untouched**.
- A **typed `PortalMeta` sidecar** carried as a *sibling* field on the wire,
  never merged into `content`.
- The frontend renders from **typed `meta` fields**; zero `_`-key string reads.

One mapping (columns → `PortalMeta`), one wire shape, one parse. Dropping a
field becomes a **compile error**, not a silent render gap.

## 3. Design

### 3.1 Shared types (`shared/src/endpoints/` or `shared/src/lib.rs`)

```rust
/// Who produced a message — a single sum type (human XOR agent XOR portal)
/// replacing the independent `sender` + `origin` optionals. `None` on
/// PortalMeta = this session's own agent output (no attribution chip needed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageSource {
    Human { account_id: Uuid, name: String },
    Agent { session_id: Uuid, agent_type: String },  // subsumes MessageOrigin::InterAgent
    Portal,                                           // continuations, reminders, system notices
}

/// Portal-side presentation/provenance metadata for a rendered message.
/// Carried alongside the raw agent `content`, never merged into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PortalMeta {
    /// Server-assigned persisted-row timestamp (ISO-8601 µs); reconnect watermark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Who produced the message (folds old sender + origin into one sum type).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<MessageSource>,
    /// Optimistic-send delivery tracking (frontend-owned; backend leaves None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryMeta>,
}

/// Grouped so "a tracked send always has a client_msg_id" is structural, not
/// four independently-Option fields that could drift into nonsense.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryMeta {
    pub client_msg_id: Uuid,                       // non-optional: tracked ⇒ has id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<InputDeliveryStage>,         // None = submitted, awaiting first ack
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,                   // Failed-only detail
}
// pending() is DERIVED from stage, not stored — can never disagree with it.
```

**On tightness (reviewed per Matt's two notes):**

- **`Option`-ness:** `created_at` and `source` stay `Option` — each is genuinely
  absent for most messages, and a non-optional sentinel (client-clock timestamp,
  a "no source" marker) would reintroduce exactly the is-this-real ambiguity
  this refactor removes. The delivery cluster (was
  `client_msg_id`/`delivery_stage`/`delivery_message`/`pending`) groups under one
  `Option<DeliveryMeta>`: cuts top-level optionality, makes `client_msg_id`
  non-optional inside, and **derives `pending` from the stage** (drops the bool).
- **`MessageSource` sum type:** replaces two independent optionals
  (`sender` human + `origin` inter-agent) — which could illegally both be set —
  with one tagged enum. Backend maps existing columns → source:
  `role=user` ⇒ `Human{user_id,name}`; `provenance_kind=inter_agent` ⇒
  `Agent{session_id,agent_type}`; portal-generated rows ⇒ `Portal`. No DB
  migration; `MessageOrigin` stays until slice 5 removes the deprecated path.

`MessageOrigin`, `InputDeliveryStage` already exist in `shared`. `MessageSource`
is new and supersedes both the frontend's `message_renderer::types::MessageSender`
and `MessageOrigin` (the latter kept until slice 5).

**Field ownership (important — `PortalMeta` is not "persisted delivery state"):**

| Field | Set by | Notes |
|-------|--------|-------|
| `created_at`, `source` | **backend** | derived from `messages` columns (`created_at`; `role`/`user_id`/`provenance_*` → `source`); the durable, server-authoritative part |
| `delivery` | **frontend** (local, for optimistic rows) | UI render state; the backend does **not** persist per-message delivery state. Live stage transitions arrive via `ServerToClient::InputProgress` and the frontend folds them into the matching optimistic row's `DeliveryMeta`. |

So the *same* struct serves both wire-from-backend (attribution/timestamp) and
local frontend optimistic state, but the backend only ever populates
`created_at` + `source`. One render model, without implying the DB tracks delivery.

### 3.2 Wire envelope changes (`shared/src/endpoints/client.rs`)

Add a single optional, typed `meta` field next to the raw content on every
message-bearing variant. All additive + `#[serde(default)]` → older clients and
backends stay parseable.

```rust
ServerToClient::AgentOutput {
    content: serde_json::Value,          // RAW agent JSON
    agent_type: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<PortalMeta>,            // NEW typed sidecar
    // DEPRECATED, kept during transition: sender_user_id, sender_name,
    // created_at, origin — populated AND mirrored into `meta` until the
    // frontend cuts over, then removed (slice 5).
}
```

`HistoryBatch` needs care: today each entry is a bare `serde_json::Value` (raw
content with `_`-keys baked in). **Replacing `messages` with a wrapper type is
NOT additive** — a cached old frontend expects raw content values and would
render the `{content, meta}` wrapper as the message body (the exact rollout
failure §1/§7 warn about). Instead, **keep `messages` byte-compatible and add a
parallel, index-aligned sidecar vector** (Codex's call, accepted):

```rust
HistoryBatch {
    messages: Vec<serde_json::Value>,            // UNCHANGED (raw, still _-injected during transition)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    message_meta: Vec<Option<PortalMeta>>,       // index-aligned with messages; None per entry w/o meta
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_created_at: Option<String>,
}
```

New frontends zip `messages` with `message_meta`; old frontends ignore the
extra field and keep working. Slice 5 may collapse this into a `HistoryEntry`
wrapper once no old bundles remain, or keep the parallel form if simpler.

### 3.3 Backend (`Claude`)

- **`Message::portal_meta()`** on `models.rs` — single constructor mapping the
  existing typed columns → `PortalMeta` (supersedes `Message::origin()`; keep
  `origin()` as a thin shim until callers migrate).
- Populate `meta` on all three emit paths from columns:
  `message_handlers.rs` (live `AgentOutput.meta`), `replay.rs` (WS history
  `message_meta`), `messages.rs` (HTTP). For HTTP, add `meta: Option<PortalMeta>`
  to the `MessageWithSender` response row **while keeping the existing top-level
  `origin`/`sender_name` fields** during transition, so the frontend uses one
  sidecar model across live WS, WS replay, and REST history.
- **Stop mutating content.** `normalize_output_content` keeps deriving
  provenance for the columns but **leaves `content` as the raw
  `AgentMessage`** (no rewrite to `Text`). Display text is derived frontend-side
  from the typed `PortalContent::AgentMessage` + `meta.source`.
  - ⚠️ Migration nuance: existing rows were already rewritten to `Text` with
    provenance in columns — those still render correctly via `meta.source`, so
    no backfill needed. New rows keep the richer raw form.
- During transition, **also** keep the `_`-injection (`replay.rs`) and the
  deprecated flat fields on `AgentOutput`, so an un-updated frontend bundle
  still works. Remove in slice 5.

### 3.4 Frontend (`Codex`)

- A single typed `RenderedMessage { content, meta }` (or thread `meta` next to
  the content string the message buffer already holds).
- Renderer reads `meta.source` / `meta.delivery` directly.
  `agent_message_event` prefers `meta.source` (the `Agent` variant), then the existing typed-content
  fallback (stale-proxy raw `AgentMessage`), and **drops all `_`-key reads**.
- Delete `inject_message_metadata` and the `_`-injection in `websocket.rs`
  once reading from `meta`.
- `message_renderer::types::PortalMessage` loses its `_origin` field; origin
  comes from `meta`.
- **Grouping simplification (Matt's note):** message grouping
  (`message_renderer/grouping.rs`) can match on the typed `meta.source` variant
  — group consecutive same-`source` runs, break on a `source` change — instead
  of sniffing `_origin`/role/`_sender` out of the content blob. Fold this into
  slice 3.

## 4. Non-goals / explicitly out of scope

- **No DB migration.** Columns already hold the metadata.
- No change to the proxy ↔ backend protocol (`ProxyToServer`). The proxy still
  forwards the typed `AgentMessage`; normalization stays backend-side.
- No change to how the agent SDKs (`claude-codes` / `codex-codes`) are parsed.

## 5. Slicing (thin, independently mergeable, parallel where possible)

| # | Slice | Owner | Depends on |
|---|-------|-------|-----------|
| 1 | `shared`: add `PortalMeta` + `MessageSource` + `DeliveryMeta`; add `meta` to `AgentOutput` and index-aligned `message_meta` to `HistoryBatch` (additive, defaults) | Claude | — |
| 2 | Backend: `Message::portal_meta()`; populate `meta` on live + HTTP + WS-replay paths (keep flat fields + `_`-injection) | Claude | 1 |
| 3 | Frontend: read from `meta` when present, fall back to `_`-keys; render unchanged | Codex | 1 |
| 4 | Backend: stop rewriting `content` in `normalize_output_content` (keep columns) | Claude | 3 deployed |
| 5 | Remove `_`-injection + deprecated flat fields once all clients read `meta` | Codex + Claude | 3, 4 |

Slices 1–3 are the bulk and can run in parallel after slice 1's shape lands.
Slices 4–5 are cleanup, gated on the frontend cutover being live.

**Critical sync point:** slice 1 (the `PortalMeta` + envelope shape) is the
contract. Land/agree it first; both sides build against it.

## 6. Test plan

- `shared`: round-trip `PortalMeta` (all-none default, full); `AgentOutput`
  with/without `meta` deserializes (old + new wire).
- Backend: `Message::portal_meta()` maps each column combo; inter-agent row →
  `meta.source = Agent`; user row → `Human`; portal row → `Portal`; non-UUID
  `provenance_session_id` → `None` (not a panic). `normalize` leaves content raw
  (slice 4).
- Frontend (Codex): `AgentOutput { meta.source: Agent }` renders the
  "Message from Codex" card; `meta` absent + legacy `_origin` still works
  (transition); optimistic delivery stages drive off `meta.delivery`.

## 7. Rollout & back-compat invariants

1. Every new field is `#[serde(default)]` — old↔new in both directions parse.
2. Transition keeps BOTH `meta` and the flat/`_`-injected fields populated, so
   a cached old frontend bundle and a new backend coexist (this is the exact
   failure we just debugged — make it impossible by construction).
3. Flat fields + `_`-injection are removed only in slice 5, after the frontend
   reads `meta` in production.
4. Each PR bumps `[workspace.package] version`; coordinate numbers in chat.
