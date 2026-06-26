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
/// Portal-side presentation/provenance metadata for a rendered message.
/// Carried alongside the raw agent `content`, never merged into it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PortalMeta {
    /// Server-assigned persisted-row timestamp (ISO-8601 µs); reconnect watermark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Human sender attribution (user-role messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<MessageSender>,
    /// Inter-agent provenance (the "Message from Codex (…)" card).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
    /// Browser-assigned delivery-tracking id (optimistic row correlation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_msg_id: Option<Uuid>,
    /// Live delivery stage for an optimistic send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_stage: Option<InputDeliveryStage>,
    /// Failure detail (only when stage == Failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_message: Option<String>,
    /// True while an optimistic send hasn't reached AgentAccepted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageSender { pub user_id: String, pub name: String }
```

`MessageOrigin`, `InputDeliveryStage` already exist in `shared`.
`MessageSender` is promoted from the frontend (currently
`message_renderer::types::MessageSender`) into `shared`.

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

`HistoryBatch` is the harder one: today each entry is a bare
`serde_json::Value` (raw content with `_`-keys baked in). Replace with a typed
pair so history matches the live path:

```rust
#[derive(Serialize, Deserialize)]
pub struct HistoryEntry {
    pub content: serde_json::Value,      // RAW
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<PortalMeta>,
}
HistoryBatch { messages: Vec<HistoryEntry>, last_created_at: Option<String> }
```

Back-compat: keep accepting the old `Vec<Value>` shape during transition via an
untagged enum or a transitional second field — see §5 slicing.

### 3.3 Backend (`Claude`)

- **`Message::portal_meta()`** on `models.rs` — single constructor mapping the
  existing typed columns → `PortalMeta` (supersedes `Message::origin()`; keep
  `origin()` as a thin shim until callers migrate).
- Populate `meta` on all three emit paths from columns:
  `message_handlers.rs` (live), `replay.rs` (WS history), `messages.rs` (HTTP).
- **Stop mutating content.** `normalize_output_content` keeps deriving
  provenance for the columns but **leaves `content` as the raw
  `AgentMessage`** (no rewrite to `Text`). Display text is derived frontend-side
  from the typed `PortalContent::AgentMessage` + `meta.origin`.
  - ⚠️ Migration nuance: existing rows were already rewritten to `Text` with
    provenance in columns — those still render correctly via `meta.origin`, so
    no backfill needed. New rows keep the richer raw form.
- During transition, **also** keep the `_`-injection (`replay.rs`) and the
  deprecated flat fields on `AgentOutput`, so an un-updated frontend bundle
  still works. Remove in slice 5.

### 3.4 Frontend (`Codex`)

- A single typed `RenderedMessage { content, meta }` (or thread `meta` next to
  the content string the message buffer already holds).
- Renderer reads `meta.origin` / `meta.sender` / `meta.delivery_*` directly.
  `agent_message_event` prefers `meta.origin`, then the existing typed-content
  fallback (stale-proxy raw `AgentMessage`), and **drops all `_`-key reads**.
- Delete `inject_message_metadata` and the `_`-injection in `websocket.rs`
  once reading from `meta`.
- `message_renderer::types::PortalMessage` loses its `_origin` field; origin
  comes from `meta`.

## 4. Non-goals / explicitly out of scope

- **No DB migration.** Columns already hold the metadata.
- No change to the proxy ↔ backend protocol (`ProxyToServer`). The proxy still
  forwards the typed `AgentMessage`; normalization stays backend-side.
- No change to how the agent SDKs (`claude-codes` / `codex-codes`) are parsed.

## 5. Slicing (thin, independently mergeable, parallel where possible)

| # | Slice | Owner | Depends on |
|---|-------|-------|-----------|
| 1 | `shared`: add `PortalMeta`, `MessageSender`, `HistoryEntry`; add `meta` to `AgentOutput` (additive, defaults) | Claude | — |
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
  `meta.origin = InterAgent`; non-UUID `provenance_session_id` → `None` (not a
  panic). `normalize` leaves content raw (slice 4).
- Frontend (Codex): `AgentOutput { meta.origin: InterAgent }` renders the
  "Message from Codex" card; `meta` absent + legacy `_origin` still works
  (transition); optimistic delivery stages drive off `meta`.

## 7. Rollout & back-compat invariants

1. Every new field is `#[serde(default)]` — old↔new in both directions parse.
2. Transition keeps BOTH `meta` and the flat/`_`-injected fields populated, so
   a cached old frontend bundle and a new backend coexist (this is the exact
   failure we just debugged — make it impossible by construction).
3. Flat fields + `_`-injection are removed only in slice 5, after the frontend
   reads `meta` in production.
4. Each PR bumps `[workspace.package] version`; coordinate numbers in chat.
