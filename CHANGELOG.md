# Changelog

## 2.5.77

- **Privilege-escalation fix: enforce editor/owner role on session-mutating actions (closes #781).** Static review surfaced that `verify_session_access` in both `backend/src/handlers/messages.rs` and `backend/src/handlers/websocket/auth.rs` accepted *any* `session_members` row regardless of role, and the downstream mutating call sites (`handle_web_input` / `handle_permission_response` / `Interrupt` / file-upload start+chunk in `web_client_socket.rs`, plus `stop_session` and `create_message` over REST) never re-checked the role — so a viewer-role member could send `ClaudeInput`, approve permission prompts, interrupt the in-flight turn, stream a file upload, POST `/api/sessions/{id}/messages`, or POST `/api/sessions/{id}/stop` against a session they were only supposed to read. New `backend/src/handlers/session_access.rs` module centralizes the layered rules: `can_mutate_role(role: &str) -> bool` is the pure role-string predicate (true for `"owner"` or `"editor"`, false for `"viewer"` and unknown roles, including the case-sensitivity check); `verify_session_mutator(conn, session_id, user_id) -> Result<Session, AppError>` is the REST-side check that returns the typed session row or `AppError::NotFound` (matches the existing handler error shape — we 404 rather than 403 to avoid leaking session existence to non-members); `is_session_mutator(app_state, session_id, user_id) -> bool` is the WebSocket-side check, re-queried on every mutating message so role revocations take effect immediately for already-connected viewers (rather than caching the role on the connection state and going stale). All three respect the owner-without-members-row invariant — `sessions.user_id == user_id` is checked first and short-circuits the membership join, so sessions that predate the `session_members` table or that lost their owner row for any reason continue to work. Gated sites: `messages.rs::create_message` (REST), `sessions.rs::stop_session` (REST), `sessions.rs::delete_session` (REST, now also accepts the owner-row fallback), `web_client_socket.rs::handle_web_input` (WS — sends a typed `ServerToClient::Error` back so the frontend can surface "permission denied"), `web_client_socket.rs` `FileUploadStart` / `FileUploadChunk` / `PermissionResponse` / `Interrupt` arms (WS — silently dropped with a warn-level log). Unit tests on `can_mutate_role` cover owner/editor/viewer plus the empty-string, unknown-role, and case-sensitivity branches.

## 2.5.76

- **Frontend WS sender: queued mpsc, eliminate drop-on-concurrent (closes #783).** `frontend/src/pages/dashboard/session_view/websocket.rs::send_message` previously wrapped the `ws_bridge` split sink in `Rc<RefCell<Option<Sender>>>` and ran a `take()` / `await sink.send()` / restore dance inside a per-call `spawn_local`. Two concurrent callers raced into the `take`-returned-`None` gap: caller B's `if let Some(_) = ...` arm silently fell through and the outgoing message was dropped without surfacing any error. The file-upload path in `component.rs` (`FilesSelected`) is the in-the-wild trigger — for every uploaded file it pushes a `FileUploadStart` envelope followed by a tight loop of base64-encoded `FileUploadChunk` envelopes through the same sender, with one of them additionally followed by a `ClaudeInput` summary frame; the original code could lose any of those chunks under realistic browser scheduling. The fix replaces the take/restore wrapper with the producer half of a `futures_channel::mpsc::unbounded::<ClientToServer>()`: `WsSender` becomes `UnboundedSender<ClientToServer>` (the public alias in `pages/dashboard/types.rs` changed, but is `Clone` like the prior `Rc` so all existing `Option<WsSender>::clone()` / `Some(ref sender)` call sites kept compiling unchanged); the `connect_websocket` task spawns one extra `spawn_local` that owns the underlying `ws_bridge` sink and pulls from the receiver in a loop, `await`ing each `sink.send()` to completion before pulling the next item. `send_message(&WsSender, ClientToServer)` collapses to a one-line `let _ = sender.unbounded_send(msg);` — synchronous, non-async, no `borrow_mut`, no drop window. Three new unit tests pin the queue semantics directly against `try_next()` (no executor required): `send_message_enqueues_all_messages_in_order` pushes 64 messages and asserts in-order receipt, `send_message_is_synchronous_push` asserts immediate visibility without an executor spin, and `concurrent_pushes_from_clones_lose_nothing` interleaves 100 pushes across two cloned `UnboundedSender`s and asserts zero loss — the exact pattern that broke under the old implementation.

## 2.5.75

- **Launcher registration: atomic duplicate check (closes #790).** `backend/src/handlers/websocket/launcher_socket.rs::handle_launcher_socket` previously did a check-then-insert pair across separate `SessionManager::find_duplicate_launcher(&hostname, user_id)` and `SessionManager::register_launcher(launcher_id, conn)` calls, with arbitrary `.await` points (the `LauncherRegisterAck` send) between them. Two simultaneous connections from the same `(user_id, hostname)` could both pass the duplicate check and both insert into the `launchers` `DashMap` (keyed by `launcher_id`), leaving the user double-registered and confusing downstream consumers (`get_launchers_for_user`, `stop_session_on_launcher`, the launcher list page). Fixed by introducing a private `launcher_dedup: Arc<DashMap<(Uuid, String), Uuid>>` index alongside the existing `launchers` map and replacing the racy pair with a single `try_register_launcher(launcher_id, connection) -> Result<(), String>` method: it `entry((user_id, hostname)).or_insert(launcher_id)` so the check-and-claim happens under one `DashMap` shard lock, returns `Err(existing_launcher_name)` if the slot was already claimed, and only mutates `launchers` after winning the reservation. `unregister_launcher` now does a paired `remove_if` on the dedup index, gated on the slot still pointing at us, so a newer same-(user, host) registration's reservation can't be evicted by a stale teardown. The max-launchers-per-user cap is intentionally left unchanged (it's a separate, less-impactful race outside this issue's scope). New regression test `concurrent_registrations_dedupe_to_single_entry` spawns 10 `tokio::spawn` tasks all trying to register the same `(user_id, hostname)` and asserts exactly one succeeds + the `launchers` map ends with exactly one entry; two further unit tests cover the serial duplicate-rejection path and the unregister-releases-slot invariant.

## 2.5.74

- **Bounded `ImageStore` with TTL + byte-cap LRU eviction (closes #787).** `backend/src/handlers/images.rs` was holding every uploaded / inlined image in a process-wide `Arc<DashMap<Uuid, StoredImage>>` with no eviction path. On long sessions with image-heavy output (Claude inlining base64 PNGs over the WebSocket, or proxies streaming chunked uploads via `/ws/session/upload`) the map grew without bound and would eventually OOM the backend. The store is now a `mini_moka::sync::Cache<Uuid, Arc<StoredImage>>` configured with a weigher that returns each entry's byte length so `max_capacity` is interpreted as a total-bytes budget, plus a `time_to_live` so un-fetched entries age out automatically; over the cap, the least-recently-used image is evicted first. Values are wrapped in `Arc` because `Cache::get` returns by clone — without the indirection every fetch would copy the raw image bytes. The chunked-upload accumulator (`pending: Arc<DashMap<Uuid, PendingUpload>>`) is unchanged; only the served-images map is bounded (in-flight uploads have their own per-message size cap via `PORTAL_MAX_IMAGE_MB`). Two new env vars plumb the caps through `AppState`: `PORTAL_IMAGE_STORE_MAX_MB` (total served-cache bytes, default **256**) and `PORTAL_IMAGE_STORE_TTL_SECS` (per-entry TTL, default **3600** = 1 h). Defaults live as `pub const`s on the `images` module so `main.rs` can fall back symbolically. Three new unit tests cover (a) inserting beyond the byte cap evicts entries so the surviving set fits under the cap, (b) entries fetched past the TTL return `None`, and (c) the base64 roundtrip path still works end-to-end; the eviction test uses `Cache::run_pending_tasks` to flush the asynchronous maintenance task synchronously. URL auth on `/api/images/{id}` is **not** changed by this PR — that's the separate issue #786 and a separate PR (they'd conflict on imports in the serve handler if combined).

## 2.5.73

- **Real auth-bypass fix: require `auth_token` validation on existing-session proxy reattach (closes #780).** Static review caught that `backend/src/handlers/websocket/registration.rs::register_or_update_session` reactivated *any* row matched by `params.claude_session_id` without verifying ownership — the existing-session branch only did the `verify_and_get_user` token resolution for the *new-session* path inside `create_new_session`, so a client that knew or guessed another session's UUID could attach as that proxy via `ProxyToServer::Register`, then read user input flowing into the session, forge `ClaudeOutput` / `SequencedOutput`, persist messages under the victim's `user_id`, and even mark unrelated sessions as `replaced` via the `replaces_session_id` path. Compounding the bug, `backend/src/handlers/websocket/proxy_socket.rs::handle_proxy_message` called `session_manager.register_session(key, tx)` *before* calling `register_or_update_session`, wiring the attacker's WebSocket into `SessionManager.sessions` (the routing table used by `send_to_session` for `SequencedInput`) prior to any DB check — so even with the auth check in place, a colliding-UUID attacker could briefly receive input messages destined for the legitimate proxy and the disconnect-cleanup path could clobber the legitimate registration's connection generation. Fix lands in three pieces:
  - **Token validation on every branch.** `register_or_update_session` now resolves `auth_token` → `user_id` (via the existing `proxy_tokens::verify_and_get_user`, with the same dev-mode `testing@testing.local` fallback the new-session path used) *before* either branch runs. A new `user_is_authorized_for_session(conn, session_id, user_id)` helper checks both the `sessions.user_id` owner column and the `session_members` sharing table (since historical owner rows aren't always mirrored into `session_members`), and the existing-session branch gates the reactivation update on that check. The `replaces_session_id` path is gated on the same check — without it, a valid-token user could mark *any* session as `replaced` purely by attaching `replaces_session_id` to their own registration. On authorization failure the response shape is identical to the not-found case (`SESSION_NOT_FOUND_ERROR = "Session not found or not authorized"`) so a UUID-guess probe can't distinguish "row doesn't exist" from "row exists but you don't own it", avoiding a session-ID oracle.
  - **Defer socket registration until after DB auth.** `handle_proxy_message`'s `Register` arm no longer calls `session_manager.register_session` up front; it only registers (and binds `session_key` / `connection_gen` / `db_session_id` for the cleanup path) on `result.success`. The send-side `tx` channel is unaffected by this reordering, so the `RegisterAck { success: false }` still flushes to the failing client through the same `send_task` that handles success-path replies.
  - **Clean close on auth failure.** `handle_proxy_message` now returns `ControlFlow<()>`; the `Register` arm returns `ControlFlow::Break(())` on failure, and the WS loop breaks out so no further `ProxyToServer` messages are accepted on a session that never authenticated. The cleanup path then `drop`s the owned `tx` handle and `await`s the `send_task` to natural completion, ensuring the failure `RegisterAck` flushes to the wire before the WebSocket closes (previously `send_task.abort()` could race the ack — fine for the success path because proxies keep the connection alive, race-prone for the immediate-close path this PR adds).
  - Two unit tests in `mod tests` of `registration.rs` pin the externally-observable contract: `unauthorized_reattach_error_shape_matches_not_found` asserts the unauthorized-reattach error is the same `SESSION_NOT_FOUND_ERROR` constant the not-found path returns (so the oracle stays closed even if a future refactor parametrizes either branch's error message), and `session_not_found_error_does_not_leak_existence` asserts the string contains none of `"token"`, `"forbid"`, `"auth"` to keep any well-meaning future edit from accidentally re-exposing existence info. End-to-end DB-backed coverage for the wrong-token / right-token reattach cases would require a Postgres test harness that doesn't yet exist in this crate; the helper extraction (`user_is_authorized_for_session`) keeps the authorization logic small and reviewable for the next person who adds that harness.

## 2.5.70

- **PR 4/4 of #758: group-level time-ago footer.** Closes the message-grouping roadmap. The three group renderers added by PR 2 (`render_portal_group`) and PR 3 (`render_user_group`, `render_codex_group`) now mirror the assistant-group treatment from PR 1: pull the **last** message's `_created_at` ISO via `extract_raw_iso(messages.last())` and render a `.message-footer` containing a live-updating `<TimeAgo iso={iso} />`. The existing `.claude-message .message-footer { justify-content: flex-end; }` puts the chip in the bottom-right corner exactly as the roadmap spec called for. Mechanical three-line change per renderer, gated on `if let Some(iso)` so pre-`_created_at` messages still render cleanly. All 19 `message_renderer` unit tests continue to pass — no new tests because the change is presence-only and the underlying `TimeAgo` component already has its own coverage.

## 2.5.69

- **PR 3/4 of #758: User + Codex grouping with explicit predicate ordering.** Extends the categorized `MessageGroup` from PR 1 and the Portal grouping from PR 2 with two new `GroupCategory` variants:
  - **`User`** (key prefix `"u"`, Tokyo Night orange `#e0af68` accent) collapses consecutive plain-text human prompts into a single `You × N` card. New `is_plain_text_user` predicate accepts either `UserMessage.content: Some(String)` (optimistic-send envelope) or `UserMessage.message.content: Some([Text { .. }, …])` (Claude echo shape) — and *only* if every nested block is `Text`. That all-Text guard is critical: a tool-result envelope is also user-shaped, but it belongs with the surrounding assistant turn. New `tool_result_user_envelope_stays_in_assistant_group` test pins that ordering invariant so a future predicate reshuffle can't silently re-route Read tool-results into User.
  - **`Codex`** (key prefix `"x"`, Tokyo Night purple `#bb9af7` accent) collapses consecutive `CodexEvent` events of any non-`Unknown` variant. The new `is_codex_event` predicate parses the wire JSON via `crate::components::codex_renderer::CodexEvent` and returns true on any successfully-recognized variant — silent streaming deltas (`PlanDelta`, `ReasoningTextDelta`, etc.) still get grouped semantically; they just render to empty cells inside the wrapper.
  - **Predicate ordering** in `classify` is now Assistant → Portal → User → Codex, with a `**Predicate ordering matters**` docblock that names *why* each rung sits where it does. Assistant runs first because user-shaped tool-result envelopes are user-typed but belong with the assistant turn; Portal next because portal frames have their own shape; User after Assistant has already claimed the tool-result envelopes; Codex last because its events are dispatched through a different enum.
  - New `render_user_group` and `render_codex_group` in `renderers.rs` mirror the PR 2 `render_portal_group` shape: one `.message-group.user-group` / `.message-group.codex-group` wrapper with a single badge + count header, with per-message rendering delegated back to `render_user_message` / `CodexMessageRenderer`. New CSS in `frontend/styles/messages.css` adds the accent borders, faint tinted backgrounds, and the same chrome-stripping overrides on nested `.user-message` / `.claude-message` so each run reads as a single visual unit — stylelint clean.
  - 7 new unit tests in `mod tests`: `plain_text_user_classifies_into_user_group`, `tool_result_user_envelope_stays_in_assistant_group` (predicate-ordering regression target), `serial_user_text_collapses_into_user_group`, `user_run_breaks_on_intervening_assistant`, `codex_event_classifies_into_codex_group`, `serial_codex_events_collapse_into_codex_group`, `codex_run_breaks_on_intervening_portal` (mixed-stream split coverage). 19 tests pass in `message_renderer` now (12 prior + 7 new). Full workspace clippy clean.

## 2.5.68

- **PR 2/4 of #758: portal-message grouping + UTC disconnect/reconnect stamps (closes #711).** Two pieces ship together:
  - **Portal grouping.** `GroupCategory` gains a `Portal` variant (key prefix `"p"`); `classify(json)` returns `Some(GroupCategory::Portal)` for any `ClaudeMessage::Portal(_)` after the Assistant check (so an assistant turn between portal frames correctly breaks the run). `MessageGroupRenderer` dispatches the `Portal` arm to a new `renderers::render_portal_group(messages, ts)`, which renders a single `claude-message message-group portal-group` block with one `Portal × N` header and a vertical body of trimmed per-message `render_portal_message(_, None)` cards. Two new tests in `mod tests` cover the happy path (`serial_portal_messages_collapse_into_one_group`) and the break case (`portal_run_breaks_on_intervening_assistant`). CSS in `frontend/styles/messages.css` adds `.portal-group` (teal `#7dcfff` left border + faint teal bg), `.portal-group-body` (column flex, small gap), and `.portal-group-body .portal-message` overrides (strip per-card border / bg / margin so the group looks unified) — stylelint clean.
  - **UTC disconnect/reconnect stamps.** `SessionState` in `claude-session-lib/src/proxy_session/mod.rs` gains a `disconnected_at_utc: Option<chrono::DateTime<chrono::Utc>>` field paired with the existing monotonic `disconnected_at: Option<Instant>` — captured at the same instant in both `ConnectionResult::Disconnected` and `ConnectionResult::ServerShutdown` arms, and cleared together on the successful-reconnect path. The reconnect portal text now appends two indented UTC lines below the existing "**Proxy reconnected** after Xm Ys (reason)" header: `disconnected at YYYY-MM-DDTHH:MM:SSZ (UTC)` and `reconnected at YYYY-MM-DDTHH:MM:SSZ (UTC)`. The wall-clock stamps make it possible to align reconnect events against external logs (server crashes, networking blips) instead of only knowing relative durations — particularly useful when reconnect runs span multiple events and the user is reading them after the fact in the grouped view from this PR's first piece.

## 2.5.67

- **Typed-dispatch `is_codex_terminal_event` in `frontend/src/components/codex_renderer.rs` (closes #757).** Follow-up to #737 (typed codex delta renderers) and #730 (typed codex dispatch). The terminal-event probe used to parse the wire line into a `serde_json::Value`, poke `val.get("type")?.as_str()?`, and string-match against `"turn.completed"` / `"turn.failed"` / `"item.started"` / etc. Now it deserializes into the typed `CodexEvent` enum (defined in the same file since #737) and `matches!` on the terminal variants directly: `TurnCompleted` / `TurnFailed` → `Some(true)`; `ItemStarted` / `ItemUpdated` / `ItemCompleted` / `TurnStarted` / `ThreadStarted` → `Some(false)`; everything else (`Error`, the six streaming-delta variants, and the `#[serde(other)] Unknown` catch-all) → `None`, preserving prior behavior bytewise. Eliminates the last stringly-typed dispatch in the codex frontend path — per CLAUDE.md's typed-interfaces rule, the next codex SDK rename here is now a compile error instead of a silent `Some(false)`/`None` regression. Existing unit tests (`terminal_event_turn_completed`, `terminal_event_turn_failed`, `terminal_event_item_completed_is_not_terminal`, `terminal_event_unknown_returns_none`) cover the round trip and continue to pass unchanged.

## 2.5.66

- **Typed `ResultMessage.modelUsage` map in the result-message renderer (closes #756).** `frontend/src/components/message_renderer/renderers.rs:1284-1293` used to `model_usage.as_object()`-iterate a `serde_json::Value` and re-poke each entry's `costUSD` via `.as_f64()`; the local `frontend::message_renderer::types::ResultMessage::model_usage` field was also typed as `Option<Value>` and (latently broken) lacked a `#[serde(rename = "modelUsage")]`, so the wire camelCase key the upstream `claude-codes::ResultMessage` re-serializes was silently never matched. Now `shared::api::ModelUsageEntry` (camelCase-renamed `#[derive(Serialize, Deserialize)]` with `inputTokens` / `outputTokens` / `cacheReadInputTokens` / `cacheCreationInputTokens` / `costUSD` / `webSearchRequests`, all `#[serde(default)]`) and `shared::api::ModelUsage` (a `BTreeMap<String, ModelUsageEntry>` alias) carry the typed shape, the local `ResultMessage::model_usage` is `Option<ModelUsage>` with the `modelUsage` rename, and the renderer iterates the typed map directly (`entry.cost_usd`). New roundtrip tests in `shared/src/api.rs` cover the per-entry and full-map wire shapes lifted from claude-codes' own `test_result_with_new_fields`. The typed mirror lives in `shared` rather than upstream because `claude-codes::ResultMessage.model_usage` is still `Option<serde_json::Value>` in 2.1.141; SDK issue [meawoppl/rust-code-agent-sdks#140](https://github.com/meawoppl/rust-code-agent-sdks/issues/140) tracks the typed adoption upstream, and the local alias is tagged `TODO(SDK #140)` so it's trivial to rip out once that lands.

## 2.5.65

- **Typed `ContentBlock` dispatch in `render_structured_block` (closes #755).** `frontend/src/components/message_renderer/renderers.rs::render_structured_block` previously took `&serde_json::Value` and poked `block.get("type").as_str()` then `block.get("text").as_str()` to dispatch text/image — same JSON-poking footgun the #729 (shim) and #733 (session_view) refactors removed: any next SDK rename silently breaks the renderer with no compile error. The function now takes `&shared::ContentBlock` (re-exported from `claude_codes::io::ContentBlock`, already present in `shared::lib.rs`) and dispatches via `match` on typed variants — `Text(t) => ExpandableText { t.text }`, `Image(_) => "[image]" pill`, and a typed catch-all that pretty-prints the variant's serialized form for any other block type. The caller in `render_content_blocks` (the `ToolResultContent::Structured(blocks)` arm) deserializes each `serde_json::Value` from the SDK's `Vec<Value>` into `shared::ContentBlock` at the boundary via `serde_json::from_value`, with a JSON pretty-print fallback if the wire shape isn't recognized — preserves prior behavior for unknown/malformed blocks while making the text/image render paths compile-time-checked against the SDK shape. `ToolResultContent::Structured` upstream is still typed as `Vec<Value>` in `claude-codes` 2.1.141, so the boundary parse is the closest typed shape available without an upstream change.

## 2.5.64

- **Typed `Citation` struct for content-block citations (closes #754).** `frontend/src/components/message_renderer/renderers.rs::render_citations` was iterating a `Vec<serde_json::Value>` and per-entry JSON-poking `cite.get("url")`, `cite.get("title")`, and `cite.get("cited_text")` — the last typed-interface gap left on the assistant text-block render path. The `citations` field on `ContentBlock::Text` in `frontend/src/components/message_renderer/types.rs` is now `Vec<shared::Citation>`, a new `#[derive(Serialize, Deserialize)] struct Citation { url, title, cited_text }` in `shared::api` with all-optional `#[serde(default)]` fields. `claude-codes` 2.1.141 also stores citations as `Vec<serde_json::Value>` on `TextBlock` (`src/io/content_blocks.rs:168`), so the local definition stays for now and SDK issue [meawoppl/rust-code-agent-sdks#142](https://github.com/meawoppl/rust-code-agent-sdks/issues/142) has been filed proposing the typed shape upstream — once it lands we can drop `shared::Citation` and re-export from `claude-codes` per the typed-interfaces rule. Wire shape is unchanged: unknown fields like `type: "web_search_result_location"` are silently dropped (regression-tested), older messages that omit `citations` entirely keep parsing via `#[serde(default)]`, and empty citations still serialize to nothing via `skip_serializing_if = "Option::is_none"`. The renderer's URL/title fallback logic (`title → cited_text → "source"`, `url → "#"`) is preserved exactly. Unit tests cover the full round-trip, the unknown-field-ignored path, and the empty-frame default.

## 2.5.63

- **Fix #758 (PR 1/4): Categorized `MessageGroup` + Assistant grouping bug fix.** Two things landed together:
  - Refactored `MessageGroup` from the binary `Single | AssistantGroup(Vec)` shape into `Single | Grouped { category: GroupCategory, messages }` with a single `classify(&str) -> Option<GroupCategory>` entry point that decides which run a message belongs to. `GroupCategory` is `Assistant`-only in this PR; `Portal` / `User` / `Codex` come in subsequent roadmap PRs without further enum churn. `group_messages` is one pass; categories accumulate as long as `classify` returns the same variant. `Single` stays a distinct variant so the common one-message case avoids the group-wrapper render path and keeps its Yew key stable independent of category.
  - Fixed the long-standing "serial Read tool uses don't roll into one block on Claude" regression. The previous `should_group_with_assistant` predicate bailed early on `msg.content.is_some()` — but the top-level `content` field is the optimistic-send envelope shape and can leak onto real Claude echoes through the cross-process wire wrapping (the exact production path that did this isn't pinned, but the regression test reproduces). The predicate now looks only at the **nested** `message.content` blocks; the stale top-level field is ignored.
  - Four new unit tests in `frontend/src/components/message_renderer/mod.rs::tests` cover (a) canonical user-tool-result grouping, (b) serial Read tool uses collapsing into one group, (c) the fragile `content` early-bail (regression target), and (d) plain-text user messages staying ungrouped.

## 2.5.62

- **Typed `ephemeral_1h` / `ephemeral_5m` cache-creation token fields in the frontend message renderer (closes #753).** `frontend/src/components/message_renderer/renderers.rs::extract_ephemeral_cache` previously read `usage.cache_creation: Option<serde_json::Value>` and JSON-poked `.get("ephemeral_1h_input_tokens").and_then(|v| v.as_u64())` / `.get("ephemeral_5m_input_tokens").and_then(|v| v.as_u64())` — silent regression bait the next time the SDK renames a field. The `claude-codes` SDK already exposes a typed `CacheCreationDetails` struct with `ephemeral_1h_input_tokens: u32` and `ephemeral_5m_input_tokens: u32` (no upstream change needed). Now: `shared::lib.rs` re-exports `CacheCreationDetails` from `claude_codes`, the local lenient `UsageInfo` in `frontend/src/components/message_renderer/types.rs` types `cache_creation` as `Option<CacheCreationDetails>` instead of `Option<serde_json::Value>`, and `extract_ephemeral_cache` reads the typed `u32` fields directly (widening to `u64` for the tooltip arithmetic). No wire-shape change — the JSON field names and types are bytewise identical to what the prior `serde_json::Value` parse accepted.

## 2.5.61

- **Typed dispatch for `SystemMessage.extra` per subtype (closes #752).** Continues the typed-interface push from #722 / #723 / #724 / #735 / #736 / #743. The three system-message renderers in `frontend/src/components/message_renderer/renderers.rs` (`render_init_bar`, `render_compaction_completed`, `render_task_notification`) used to JSON-poke `msg.extra.as_ref().and_then(|v| v.get("fast_mode_state").as_str())` / `v.get("summary")` / `v.get("status")` / `v.get("usage")` etc. — silent regression bait if the wire renames a field, exactly the pattern PR #743's `CodexPermissionInput` envelope retired on the codex side. Each renderer now `serde_json::from_value::<T>(extra.clone())` into a typed struct once per branch and reads named fields off it. The init bar uses a new `shared::InitExtra { fast_mode_state }` (mirroring the SDK's already-typed `InitMessage::fast_mode_state`); the task notification uses a new `shared::TaskNotificationExtra { status, task_id, usage }` (a narrow mirror of `claude_codes::TaskNotificationMessage`'s renderable subset — the SDK type's required `session_id` / `summary` are already consumed by the lenient `SystemMessage`'s typed top-level fields and never appear in the flattened `extra`); the compaction renderer uses a new `shared::CompactionExtra { summary, leaf_message_count, message_count, duration_ms, content, text }` with helper methods `summary_text()` and `message_count()` that mirror the historical `summary` → `content` → `text` and `leaf_message_count` → `message_count` fallback chains. The compaction extra is necessary because `claude_codes::CompactBoundaryMessage` only exposes `compact_metadata { pre_tokens, trigger }` and not the per-compaction summary stats the UI surfaces — filed upstream as [`rust-code-agent-sdks#141`](https://github.com/meawoppl/rust-code-agent-sdks/issues/141) with a `TODO(SDK rust-code-agent-sdks#141)` marker on `CompactionExtra` so the local mirror can be deleted (and the renderer switched to `CCSystemMessage::as_compact_boundary()`) when upstream lands. **Wire shape is unchanged bytewise** — `extra` stays a `#[serde(flatten)] Option<serde_json::Value>` on the lenient `SystemMessage`, the new typed structs are deserialize-only views over the same bytes, and `#[serde(default)]` on every field of every mirror means any frame that omits them still parses (yielding `None`).

## 2.5.60

- **Typed `ToolInput::ExitPlanMode` access in `frontend/src/components/tool_renderers/interactive.rs` (closes #751).** Renderer-side parity to #740's permission-dialog refactor. `render_exitplanmode_tool` was reading `input.get("allowedPrompts").and_then(|v| v.as_array())` and then per-entry `.get("tool").as_str()` / `.get("prompt").as_str()` — the same silent-regression bait #740 fixed on the permission-card side. Replaced with `serde_json::from_value::<ToolInput>(input.clone())` matching `ToolInput::ExitPlanMode(epm)` and reading `epm.allowed_prompts: Option<Vec<AllowedPrompt>>` typed; iteration uses typed `p.tool` / `p.prompt` directly. `AllowedPrompt`, `ToolInput`, and `ExitPlanModeInput` are already re-exported from `shared` (landed in #740) — no shared/upstream changes needed. Render layout, CSS classes, and the empty-list fallback are unchanged. The sibling `render_askuserquestion_tool` JSON-poke is tracked separately in #750.

## 2.5.59

- **Typed `ToolInput::AskUserQuestion` dispatch in the AskUserQuestion renderer (closes #750).** `frontend/src/components/tool_renderers/interactive.rs::render_askuserquestion_tool` no longer pokes `input.get("questions").as_array()` and per-entry `.get("header") / .get("question") / .get("multiSelect") / .get("options") / .get("label") / .get("description")` on raw `serde_json::Value`. It now deserializes the renderer's `Value` into `claude_codes::tool_inputs::ToolInput`, matches `ToolInput::AskUserQuestion(AskUserQuestionInput)`, and reads typed `questions: Vec<Question>` with typed `header: String`, `question: String`, `multi_select: bool`, and `options: Vec<QuestionOption>` (label + optional description). The `answers: Option<HashMap<String, String>>` lookup is also typed now — no more `.as_object()` / `.as_str()` JSON peeking. Same icons, CSS classes, multi-select badge, comma-split answer-matching, and empty-list fallback on deserialization failure. `shared` re-exports `AskUserQuestionInput`, `Question`, and `QuestionOption` from `claude_codes::tool_inputs` so the frontend stays free of a direct `claude-codes` dep. The ExitPlanMode branch in the same file is unchanged — that one is tracked by #751.

## 2.5.58

- **`render_task_tool` uses typed `ToolInput::Task` instead of JSON-poking (closes #749).** Follow-up to 2.5.48 (#735) / 2.5.49 (#736): the Task subagent renderer in `frontend/src/components/tool_renderers/task.rs` was the third claude-side renderer still reading `input.get("description") / input.get("subagent_type") / input.get("run_in_background")` against `serde_json::Value`. It now deserializes its `serde_json::Value` input into `claude_codes::tool_inputs::ToolInput`, matches `ToolInput::Task(TaskInput)`, and reads the typed `description: String`, `subagent_type: SubagentType` (via `.as_str()`, which handles `Unknown(_)` for forward-compat), and `run_in_background: Option<bool>` fields directly. Same icon, same CSS classes, same `"?"` / `"agent"` / `false` fallbacks when deserialization fails — just no more silent-null bait the next time the SDK renames a field. `shared` now re-exports `TaskInput` alongside the existing `TodoItem` / `TodoStatus` / `TodoWriteInput` / `ToolInput` re-exports.

## 2.5.57

- **`render_webfetch_tool` / `render_websearch_tool` use typed `ToolInput::WebFetch` / `ToolInput::WebSearch` instead of JSON-poking (closes #748).** Both renderers in `frontend/src/components/tool_renderers/search.rs` now deserialize their `serde_json::Value` input into `claude_codes::tool_inputs::ToolInput`, match the relevant variant, and read typed `WebFetchInput { url: String, prompt: String }` / `WebSearchInput { query: String, … }` fields directly — no more `input.get("url")` / `input.get("prompt")` / `input.get("query")`. Same icons, same CSS classes, same `?` fallback when deserialization fails. `shared` now re-exports `WebFetchInput` and `WebSearchInput` so the frontend can use them without a direct `claude-codes` dep. The Glob/Grep portions of the same file remain JSON-poked under #747.

## 2.5.56

- **Typed `ToolInput::Glob` / `ToolInput::Grep` dispatch in `frontend/src/components/tool_renderers/search.rs` (closes #747).** `render_glob_tool` and `render_grep_tool` previously pulled fields out of their `serde_json::Value` input by string name: `input.get("pattern")`, `input.get("path")`, `input.get("glob")`, `input.get("type")`, and the suspicious `input.get("-i")`. Both renderers now deserialize the value into `claude_codes::tool_inputs::ToolInput` once and match the appropriate variant (`ToolInput::Glob(GlobInput)` / `ToolInput::Grep(GrepInput)`), reading typed fields directly — `pattern: String`, `path: Option<String>`, `glob: Option<String>`, `file_type: Option<String>` (the SDK already handles the `#[serde(rename = "type")]`), and `case_insensitive: Option<bool>` (the SDK already handles the `#[serde(rename = "-i")]`, so what looked like a JSON-poke smell on the consumer side turned out to be fully typed upstream — no SDK gap). Same icons, same CSS classes, same `"?"` fallback when deserialization fails or the variant doesn't match; just no more string-name field probing. `shared::lib` now re-exports `GlobInput` and `GrepInput` alongside the existing `ToolInput` / `TodoWriteInput` / `TodoItem` / `TodoStatus` re-exports so the frontend doesn't need a direct `claude-codes` dependency. The WebFetch and WebSearch renderers in the same file remain untouched — they're tracked separately in #748.

## 2.5.55

- **Typed `ToolInput::Read` dispatch in `render_read_tool` (closes #746).** The Read tool renderer in `frontend/src/components/tool_renderers/mod.rs` was poking `input.get("file_path").as_str()` / `input.get("offset").as_i64()` / `input.get("limit").as_i64()` against `serde_json::Value`. Replaced with `serde_json::from_value::<ToolInput>(input.clone())` matching `ToolInput::Read(ReadInput { file_path, offset, limit })`, then reading typed fields (`String` and `Option<i64>`) directly. `shared` now re-exports `ReadInput` so the renderer doesn't take a direct `claude-codes` dep. Range-info formatting, CSS classes, the `"?"` fallback path, and the icon are unchanged.

## 2.5.54

- **`render_edit_tool` / `render_write_tool` use typed `ToolInput::Edit` / `ToolInput::Write` instead of JSON-poking (closes #745).** Follows #735 (TodoWrite) and #736 (ExitPlanMode). The two renderers in `frontend/src/components/tool_renderers/edit.rs` previously did `input.get("file_path").and_then(|v| v.as_str()).unwrap_or("unknown file")` / `get("old_string")` / `get("new_string")` / `get("replace_all").as_bool()` / `get("content").as_str()` against the raw `serde_json::Value` envelope — same silent-regression bait the prior typed-dispatch PRs killed elsewhere (next wire rename returns `None`, the renderer silently displays defaults). Both functions now `serde_json::from_value::<ToolInput>(input.clone())`, match `ToolInput::Edit(EditInput)` / `ToolInput::Write(WriteInput)`, and read typed `file_path: String`, `old_string: String`, `new_string: String`, `replace_all: Option<bool>` (Edit) and `file_path: String`, `content: String` (Write). Same icons, CSS classes, layout, and the same "unknown file" / empty-content fallback when the input doesn't deserialize as the expected variant (preserves behavior for malformed in-flight frames). `shared` now also re-exports `EditInput` and `WriteInput` so the frontend uses them without taking a direct `claude-codes` dependency, matching the `TodoItem` / `TodoStatus` re-export pattern from #735.

## 2.5.53

- **`render_bash_tool` uses typed `ToolInput::Bash` dispatch instead of JSON-poking (closes #744).** The frontend Bash tool renderer in `frontend/src/components/tool_renderers/bash.rs` was reading `input.get("command")`, `input.get("description")`, `input.get("timeout")`, and `input.get("run_in_background")` against a raw `serde_json::Value` — silent-regression bait if `claude-codes` renames a field. Now `render_bash_tool` deserializes its `serde_json::Value` into `claude_codes::tool_inputs::ToolInput`, matches the `Bash(BashInput)` variant, and reads typed fields directly: `bash.command: String`, `bash.description: Option<String>`, `bash.timeout: Option<u64>`, `bash.run_in_background: Option<bool>`. Same rendered output, same CSS classes, same empty-command fallback when the wire shape doesn't deserialize. `shared` now re-exports `BashInput` alongside the existing `ToolInput` re-export so the frontend doesn't need a direct `claude-codes` dependency. Follows the same typed-dispatch pattern as `render_todowrite_tool` (#735, 2.5.48) and the ExitPlanMode permission dialog (#736, 2.5.49).
## 2.5.52

- **Shared typed `CodexPermissionInput` envelope (closes #725 and #731).** Builds on 2.5.45's typed-dispatch refactor (#723 / #730) by typing the *other* half of the wire boundary: the `IoEvent::CodexPermissionRequest.input` IPC payload that the codex app-server bridge in `codex-session-lib/src/handler.rs` sends and the frontend's `format_permission_input` in `frontend/src/pages/dashboard/types.rs` consumes. Before 2.5.45 both ends JSON-poked the same field names from opposite sides of the wire; 2.5.45 typed the proxy *read* (typed-match on `ServerRequest::*Approval(p)`) but the cross-process *write* still serialized into a stringly-typed `serde_json::Value` blob via `serde_json::json!({...})`, and the frontend's consumer still picked it apart with `input.get("itemId").and_then(|v| v.as_str())`. Now: `shared::CodexPermissionInput` is a `#[serde(tag = "tool", rename_all = "camelCase")]` enum with one variant per codex approval type (`FileChange`/`ApplyPatch`/`Bash`/`ExecCommand`/`Permissions`/`McpElicitation`/`AskUserQuestion`), `IoEvent::CodexPermissionRequest` drops the redundant `tool_name: String` field (the variant is now the discriminant — call `input.tool_name()` for the human-readable key the cross-agent `SessionEvent::PermissionRequest` envelope still carries because the claude path comes through it too), `handler.rs` constructs each variant directly from the SDK's typed param struct (so the proxy-emission half is now compile-time-checked against the SDK shape — `serde_json::json!({…})` literals are gone), and the frontend's `format_permission_input` round-trips the wire `Value` into the typed enum before dispatching on variants (with the historical JSON-poke arms retained for claude-side tool inputs, which use a different IPC envelope from `claude-codes::tool_inputs::ToolInput` and are out of scope here — see #735/#736). The wire envelope on `ProxyToServer::PermissionRequest` / `ServerToClient::PermissionRequest` stays `serde_json::Value` — both sides now use a typed serde-derived round trip through that wire, which preserves backward compat with in-flight frames from older proxies while making the next codex SDK shape change a compile error rather than a silent `null`. New unit tests in `shared/src/api.rs` cover the round-trip and the optional-fields-omitted case the 0.129.3 wire frames hit.

## 2.5.51

- **Typed HTTP response structs for `/api/auth/me`, `/api/sessions`, and `/api/admin/users` (wave 1 of #734).** Added `shared::api::MeResponse`, `shared::api::SessionsResponse`, `shared::api::AdminUserEntry`, and `shared::api::AdminUsersResponse` so the four call sites that used to `serde_json::Value::get("…")` the response now deserialize against a typed struct shared with the backend handler. Backend `me` returns `MeResponse` directly (lifted from the old in-handler `UserResponse`); admin `list_users` returns `AdminUsersResponse` carrying `Vec<AdminUserEntry>`. Frontend pages (`dashboard/page.rs`, `hooks/use_sessions.rs`, `settings/sessions_panel.rs`, `admin/mod.rs`) now `.json::<TypedStruct>()` instead of poking field names. **Wire shape is unchanged bytewise** — fields keep their existing names and `#[serde(default)]` covers older partial responses; the sessions endpoint still serializes the full `Session` row + `my_role` flatten and the shared `SessionInfo` deserialize silently drops the extras it doesn't need. Subsequent waves (settings panels, voice config, admin sessions/stats, etc.) remain tracked in #734.

## 2.5.50

- **Typed-dispatch refactor in `frontend/src/pages/dashboard/session_view/component.rs` (closes #733).** Replaced five `serde_json::from_str::<Value>` + `.get("type")` / `.get("content")` JSON pokes with typed `ClaudeMessage` parses. The most-recent-meaningful-message filter, the history-replay `msg_type` classification, and the live-output fallback `msg_type` classification now derive the wire `type` tag from a small `message_type_tag(&ClaudeMessage) -> &'static str` helper instead of probing the raw JSON. The pending-send echo matching at the "user" branch now extracts the user-text payload via a typed `extract_user_text(&ClaudeMessage) -> Option<String>` helper that walks `UserMessage::content` (the optimistic-send + codex synthesized-echo shape) or, when absent, concatenates `ContentBlock::Text` blocks from `message.content` (the Claude `--replay-user-messages` shape) — which also fixes the latent bug where the bytewise `Value` compare silently never matched against Claude's nested-blocks echo. **No behavior change** to the downstream `msg_type` string contract or the message storage shape — the broader in-memory `Vec<String>` → `Vec<ClaudeMessage>` refactor flagged in the issue stays out of scope for this PR.

## 2.5.49

- **Typed `ToolInput::ExitPlanMode` access in `frontend/src/pages/dashboard/permission_dialog.rs` (closes #736).** The ExitPlanMode permission card was reading `perm.input.get("allowedPrompts").and_then(|v| v.as_array())` and then per-entry `.get("tool").as_str()` / `.get("prompt").as_str()` on the inner objects — silent regression bait if the wire renames a field. Replaced with `serde_json::from_value::<ToolInput>(perm.input.clone())` matching `ToolInput::ExitPlanMode(epm)` and reading `epm.allowed_prompts: Option<Vec<AllowedPrompt>>` typed; iteration uses typed `p.tool` / `p.prompt` directly. `ToolInput`, `ExitPlanModeInput`, and `AllowedPrompt` are now re-exported from `shared` so the frontend doesn't need a direct `claude-codes` dep. The `perm.input: serde_json::Value` envelope stays — per-tool inputs have different shapes; typed decoding happens at the dispatch site. Render layout, CSS classes, and other permission-tool branches (Bash, Edit, Write) are unchanged — they remain separate issues.

## 2.5.48

- **`render_todowrite_tool` uses typed `ToolInput::TodoWrite` instead of JSON-poking (closes #735).** The frontend renderer now deserializes its `serde_json::Value` input into `claude_codes::tool_inputs::ToolInput`, matches the `TodoWrite(TodoWriteInput)` variant, and reads each `TodoItem`'s typed `content: String` and `status: TodoStatus` enum (`Completed` / `InProgress` / `Pending` / `Unknown(_)`). Same icons, same CSS classes, same empty-list fallback when deserialization fails — just no more `.get("todos").as_array()` / `.get("status").as_str()`. `shared` now re-exports `TodoItem`, `TodoStatus`, `TodoWriteInput`, and `ToolInput` so the frontend can use them without taking a direct `claude-codes` dependency.

## 2.5.47

- **Typed query-param parse on the banned page (closes #732).** `frontend/src/pages/banned.rs` no longer pokes `web_sys::UrlSearchParams::get("reason")` against the URL; it uses `yew_router::use_location().query::<BannedQuery>()` with a `#[derive(Deserialize)] struct BannedQuery { reason: Option<String> }`, and `serde_urlencoded` handles URL decoding so the manual `js_sys::decode_uri_component` block is gone.

## 2.5.46

- **Typed frontend renderers for codex streaming-delta events.** `turn/diff/updated`, `item/fileChange/patchUpdated`, and `turn/plan/updated` previously fell through `CodexEvent`'s `#[serde(other)] Unknown` arm and got dumped as raw pretty-printed JSON in the transcript. Now `CodexEvent` carries typed variants for all six slash-named delta methods the proxy forwards (the three above plus `item/plan/delta`, `item/reasoning/summaryPartAdded`, `item/reasoning/textDelta`); the first three render as real cards (cumulative unified diff, per-file patches, numbered plan with status icons) and the latter three are typed no-op stubs that suppress the raw-JSON dump without aggregating delta state. The per-line HTML emission was extracted out of `render_diff_lines` into a private `render_diff_html` helper, and a new public `render_unified_diff(diff: &str)` parses unified-diff strings (skipping `--- `, `+++ `, `@@ ` headers and the `\\ No newline at end of file` marker, classifying body lines by leading char with a context fallback) and feeds the same helper. Both the claude `Edit` tool renderer and the new codex diff renderers go through `render_diff_html`, so styling stays in lockstep. Tests cover `parse_unified_diff` (single hunk, multi-hunk, blank context, no-newline marker, missing file headers, unprefixed lines) and round-trip each new `CodexEvent` variant through `serde_json::from_str`.

## 2.5.45

- **Closes #723: dispatch `handle_codex_server_message` on the typed `codex_codes::Notification` / `codex_codes::ServerRequest` enums instead of stringly-typed `match method.as_str()` + `params.get("…")` JSON-poking.** The previous shape lowered every typed param struct back to `serde_json::Value` just to re-read fields by string name — losing every compile-time guarantee the SDK provides (PR #721 was a concrete instance: codex 0.129.3 dropped `FileChangeRequestApprovalParams.changes`, the lookup silently returned `null`, the FileChange dialog rendered `{"changes": null}`). Now both halves of the handler match on the typed variants directly: `Notification::TurnCompleted(p) => p.turn`, `Notification::Error(p) => p.error.message`, `Notification::DeprecationNotice(p) => p.summary` (which actually carries the wire text — the old `params.get("notice")` always returned the `"(no message)"` default because `notice`/`message` never existed on the typed schema), `ServerRequest::FileChangeApproval(p) => { itemId: p.item_id, reason: p.reason, grantRoot: p.grant_root }`, `ServerRequest::ExecCommandApproval(p) => p.command.join(" ")`, and so on for every variant the SDK exposes. The intermediate `let (method, params) = notif.into_envelope()` and `let params: Value = match &request { … to_value(p) … }` re-serialize preambles are gone. Frontend-facing JSON shape (`IoEvent::CodexPermissionRequest::input` field names and types) is preserved verbatim — `format_permission_input` keeps working unchanged. New unit tests in `codex-session-lib/src/handler.rs` cover the FileChange / ExecCommand / Bash / ApplyPatch / TurnCompleted / Error / ItemStarted / McpElicitation dispatch paths.

## 2.5.44

- **Typed-dispatch refactor in `proxy/src/shim.rs` (closes #724).** The shim's stdout and stdin readers used to route lines by JSON-poking the wire `type` field — `value.get("type").and_then(|t| t.as_str())` for the stdout user-echo filter, the stdout msg_type dispatch, and the stdin `control_response` detect, plus `block.get("type")` inside `extract_user_text`. Each site now parses the line into a typed `claude_codes::ClaudeOutput` up front and dispatches on enum variants: `Some(ClaudeOutput::User(user))` triggers user-echo filtering (with `extract_user_text` walking `user.message.content` typed and matching `ContentBlock::Text` blocks), `Some(ClaudeOutput::ControlResponse(_))` is dropped from the portal stream, `Some(ClaudeOutput::ControlRequest(req))` runs the existing `ControlRequestPayload::CanUseTool` permission branch, and any other typed variant is buffered for portal delivery. The stdin reader reads the `request_id` from `ControlResponsePayload::{Success, Error}` instead of poking a top-level field. **No wire/protocol change** — every line still passes through to VS Code bytewise when `forward_to_vscode == true`, and lines that don't match `ClaudeOutput` (non-JSON, or valid JSON like `stream_event` that the typed enum doesn't model) fall back to a raw `Value` parse so they still reach the portal (with the prior `stream_event` portal-filter preserved). The next claude-codes wire-shape change that adds or renames a variant now gets caught at compile time on this codepath instead of silently falling through the dispatch.

## 2.5.43

- **Bump `claude-codes` 2.1.140 → 2.1.141.** Pure addition upstream — adds `ToolPermissionRequest::answer_questions(answers, request_id)`, a typed helper for replying to `AskUserQuestion` approvals that preserves the original `questions` array in the `updatedInput` payload (manual `{answers: {...}}` responses drop it, which crashes downstream viewers that call `tool_use_result.questions.map(...)`). Not yet used by agent-portal — existing AskUserQuestion approval path can switch to the helper in a follow-up.

## 2.5.42

- **Tag `agent_type` per-message at the proxy emission boundary instead of capturing it from registration.** The 2.5.40/2.5.41 design captured `agent_type` from the proxy's `Register` and threaded it through `proxy_socket.rs` into every insert site. That was functionally correct today (one connection = one io_task = one agent) but wrong shape for any future multi-agent / sub-agent session: every insert site would silently mis-attribute the day a single `/ws/session` connection started ferrying messages from more than one agent. Now `ProxyToServer::SequencedOutput` and `ProxyToServer::ClaudeOutput` carry `agent_type: AgentType` on each message, the proxy reads `config.agent_type` at every emission site (output forwarder, wiggum portal messages, replay loop, codex raw output, connection-portal banner, shim mode), and `proxy_socket.rs` trusts the per-message tag — the `session_agent_type` capture is gone, as is the error-and-drop guard for pre-Register output. The live broadcast `ServerToClient::ClaudeOutput` also grew an `agent_type` field so the frontend's in-flight messages can be tagged the same way as the historical-read path. **Wire compat caveat:** pre-2.5.42 proxies don't send `agent_type` on output messages; `#[serde(default)]` makes them parse as `AgentType::Claude`, which is a presumed misattribution for any in-flight codex session running an older proxy until the proxy upgrades.

## 2.5.41

- **Drop the SQL DEFAULT and the orphan fallback from the messages `agent_type` migration; require every callsite to specify it explicitly.** The 2.5.40 migration set `DEFAULT 'claude'` on the column and filled any null rows with `'claude'` before the NOT NULL alter — both were silent claude-fallbacks that would mistag codex sessions if any future out-of-band INSERT path forgot the field. Now: no column DEFAULT (out-of-band inserts that omit `agent_type` fail with a NOT NULL violation, which is the point), and no orphan fallback (the join-backfill is the only source of truth; if the NOT NULL alter fails, you have orphan messages whose session got deleted without cascading — fix that before re-running). On the Rust side, `proxy_socket.rs`'s `session_agent_type` is now `Option<AgentType>` initialized to `None`; any `ClaudeOutput` / `SequencedOutput` arriving before `Register` is logged at `error!` and dropped rather than silently tagged as claude.

## 2.5.40

- **Add `agent_type` column to the `messages` table** (`VARCHAR(16) NOT NULL`) so readers know which agent's wire format each message's `content` JSON came from instead of guessing by JSON-poking the `type` field. Migration backfills from the parent session's `agent_type` for every existing row via a join. All three insert sites are now populated: `handle_claude_output` in `backend/src/handlers/websocket/message_handlers.rs` takes the agent type as a new parameter threaded from the proxy's `Register` message (captured into `session_agent_type` in `proxy_socket.rs`); the slash-command portal insert in `web_client_socket.rs` and the `create_message` HTTP handler both read it from the parent `Session` row. The new field is exposed to the frontend automatically via `MessageWithSender`'s `#[serde(flatten)] message: Message` and a matching `#[serde(default)] pub agent_type: String` on `MessageData` in `frontend/src/pages/dashboard/types.rs`. Frontend dispatcher gating is deferred — issue #723 will refactor codex dispatch separately, at which point consumers can branch on `agent_type` instead of probing `content.get("type")`.

## 2.5.39

- **Decommingle `claude-session-lib` into three crates.** `claude-session-lib` previously housed both `claude_io_task` and `codex_io_task` along with codex-specific helpers and an `IoEvent::CodexPermissionRequest` variant — the crate name lied. Split:
  - New **`session-lib`** owns the agent-agnostic core: `Session<A: Agent>` generic, `trait Agent` with `spawn_io_task`, the unified `IoCommand` / `IoEvent` / `SessionEvent` enums, `SessionConfig` / `SessionSnapshot` / `PendingPermission`, `OutputBuffer`, `PendingOutputBuffer`, `HeartbeatTracker`, `probe_all_agents`, `SessionError`.
  - **`claude-session-lib`** now contains only Claude-specific bits: `pub struct ClaudeAgent` with `impl session_lib::Agent`, `claude_io_task` (incl. the upstream-429 rate-limit retry state machine), `spawn_claude`, and `proxy_session/*` (wiggum mode, portal reminder, image upload, ws_reader). The `proxy_session` connection loop stays here and is now generic over `A: Agent`, so both Claude- and Codex-backed sessions can use it; splitting it is deferred to a future PR.
  - New **`codex-session-lib`** contains `pub struct CodexAgent`, `codex_io_task`, and `handle_codex_server_message` (the existing stringly-typed `match method.as_str()` dispatch is preserved verbatim — issue #723 will refactor to typed `ServerRequest` matching in a follow-up PR).
- Launcher's `process_manager.rs` adds an `AnySession` enum (`Claude(Session<ClaudeAgent>)` / `Codex(Session<CodexAgent>)`) so a single in-process map can hold mixed-agent sessions; the dispatch happens at `match config.agent_type` in `AnySession::new`.
- Proxy keeps using `Session<ClaudeAgent>` directly — the proxy binary never spawns Codex.
- `SessionError::ClaudeError(claude_codes::Error)` is gone — per-agent crates now collapse SDK errors into `SessionError::Agent(String)` so `session-lib`'s error surface stays SDK-free.

## 2.5.38

- CLAUDE.md: codify typed-interface preference + upstream-first rule, lifted from the codex `item/fileChange/requestApproval` regression in 2.5.38 / PR #721.

## 2.5.37

- **Bump `codex-codes` 0.129.2 → 0.129.3 and absorb the SDK rewrite.** Upstream 0.129.3 (PR #138) regenerated every wire type from the schema and shipped [SDK #134](https://github.com/meawoppl/rust-code-agent-sdks/issues/134)'s structured `ParseError`. Three concrete consequences for agent-portal:
  - **`codex_io_task` rewrite** in `claude-session-lib/src/session.rs` against the new API: `ThreadStartParams` lost its `Default` impl (15 Option fields, all `skip_serializing_if`), so we now build it via `serde_json::from_value(json!({}))` matching the SDK's own usage example; `ThreadStartResponse::thread_id()` is gone — use `resp.thread.id`; `TurnInterruptParams` now requires both `thread_id` and `turn_id`, so we track the active turn id from each `turn/started` notification and pass it on interrupt; `TurnStartParams::reasoning_effort` is now `effort` and the struct gained more Option fields, so we build it the same way; `UserInput::Text` gained a required `text_elements: Option<...>` we set to `None`.
  - **`CodexFrameCaptureLayer` retired** (the tracing-snooping workaround from 2.5.30 — which didn't actually capture in production anyway). The `Error::Json` arm in `codex_io_task` becomes `Error::Deserialization(ParseError)` and reads `raw_line` / `raw_json` / `method` / `error_message` directly off the struct. The portal message gets a real fenced ```json block with the offending frame, complete with the JSON-RPC `method` name. Supersedes the long-pending #708 revert PR.
  - **Cleanup:** removed `claude-session-lib/src/codex_frame_capture.rs`, its module export from `lib.rs`, the install lines in `launcher/src/main.rs` and `proxy/src/main.rs`, and the `tracing-subscriber` dep from `claude-session-lib/Cargo.toml`. Per-layer EnvFilter rewiring reverted to the original registry-level EnvFilter.
- **Net diff** is a small set of one-line API shape changes + one architectural pull-out, exactly proportional to a major SDK refactor.

## 2.5.36

- Session pill agent-type watermark (Anthropic asterisk / OpenAI knot) is now 10% smaller — 88×88 → 80×80 — with the left offset adjusted from −28px to −24px so the icon's horizontal center stays at the same x=16 in pill coords. Vertical center is unchanged.

## 2.5.35

- **Rate-limit retry: bump cap from 4 attempts/30s to 30 attempts/60s.** The original 2.5.29 settings gave up after ~1 minute of sustained throttling, which was too short for the rate-limit windows Anthropic actually applies (multi-minute holds are common). With 30 attempts at full-jitter exponential backoff capped at 60s, the upper bound on retry wall time is ~30 minutes of patience before we surface the "send your message again" portal note — and any earlier success short-circuits, so well-behaved limits unblock as soon as the window opens. Counter still resets on every fresh user input and every successful turn.

## 2.5.34

- **Bump `codex-codes` 0.129.1 → 0.129.2** and wire it through `codex_io_task`. The dep bump picks up [SDK #135](https://github.com/meawoppl/rust-code-agent-sdks/issues/135) (`AppServerBuilder::config_override` + `extra_args`). The call-site change parses each codex session's `SessionConfig::extra_args` (the existing "Extra args" launch-dialog text input — already wired through to the launcher) into two buckets:
  - tokens of the form `-c key=value` / `--config key=value` become `builder.config_override(k, v)` (rendered as `-c k=v` **before** the `app-server` subcommand, since `-c` is a global codex flag);
  - everything else becomes `builder.extra_args(...)` (appended **after** `--listen stdio://`).
  - Lets a user type e.g. `-c sandbox_mode=workspace-write -c approval_policy=on-request --strict-config` in the launch dialog and have each token land in the right slot on the spawned codex command line. No structured UI for sandbox/approval pickers yet — that's a follow-up.

## 2.5.33

- **Route the codex 0.130+ message types to user-facing UI instead of letting them fall through to "Unknown Codex request".** Followup to 2.5.32 which got the typing in. Each new message type now has a purpose-built dispatch arm:
  - **Approval requests** wire into the existing `CodexPermissionRequest` event with sensible \`tool_name\`s and trimmed input payloads, so the user gets the existing approve/deny dialog for each:
    - \`execCommandApproval\` → \`tool_name: "ExecCommand"\` (command + cwd + parsedCmd)
    - \`applyPatchApproval\` → \`tool_name: "ApplyPatch"\` (file-change map + grant-root + reason)
    - \`item/permissions/requestApproval\` → \`tool_name: "Permissions"\` (cwd + permissions profile + reason)
    - \`item/tool/requestUserInput\` → \`tool_name: "AskUserQuestion"\` (reuses the Claude AskUserQuestion renderer — the Codex question shape is structurally compatible)
    - \`mcpServer/elicitation/request\` → \`tool_name: "McpElicitation"\` (server name)
  - **Internal/system requests** (no user action to take) emit a portal text message so the user sees what was requested without blocking the agent:
    - \`item/tool/call\`, \`account/chatgptAuthTokens/refresh\`, \`attestation/generate\`
  - **Notifications**:
    - \`deprecationNotice\` / \`guardianWarning\` → portal text (high-visibility)
    - \`item/plan/delta\`, \`turn/plan/updated\`, \`turn/diff/updated\`, \`item/reasoning/summaryPartAdded\`, \`item/reasoning/textDelta\`, \`item/fileChange/patchUpdated\` → typed structured events the frontend can opt into rendering (currently fall through to the existing raw-codex display but with a stable \`type\` tag for follow-up renderers)
    - Pure-status notifications (\`mcpServer/oauthLogin/completed\`, \`account/login/completed\`, etc.) stay silent (debug log)
- **Frontend**: extended \`format_permission_input\` in \`frontend/src/pages/dashboard/types.rs\` so the new tool_names (\`ExecCommand\`, \`ApplyPatch\`, \`Permissions\`, \`McpElicitation\`) get readable one-line summaries in the permission card instead of dumping a JSON blob.

## 2.5.32

- **Bump `codex-codes` 0.128.0 → 0.129.1.** Upstream added eight new `ServerRequest` variants and roughly ten new `Notification` variants modeling codex CLI 0.130+ protocol additions (tool-input prompts, MCP elicitations, generic permission requests, dynamic tool calls, ChatGPT auth-token refresh, attestation, apply-patch approval, exec-command approval, plus plan/diff/reasoning delta notifications). Extended the proxy's `ServerRequest` match in `claude-session-lib/src/session.rs` to cover all of them — each new variant serializes its typed param struct back to `Value` so the downstream string-dispatch keeps working. New approval-type methods (`ApplyPatchApproval`, `ExecCommandApproval`, `PermissionsRequestApproval`, etc.) currently fall through to the existing `_ => warn!("Unknown Codex request: {}", method)` arm, which surfaces them as raw codex frames in the transcript — purpose-built UIs for each new approval type are a follow-up. New notifications flow through `notif.into_envelope()` and hit the existing string-dispatch's `_ => debug!` arm without code changes.

## 2.5.31

- **Probe installed agent CLIs when the launch dialog opens.** Previously the only signal that a launcher was missing `codex` or `claude` was a session that spawned then vanished within a second with a misleading `exited normally (code 0)` log. The launch dialog now asks the selected launcher to scan its PATH the moment the user opens it (and again on every launcher dropdown change), and surfaces the result inline:
  - Agent dropdown labels show `Claude (not installed)` / `Codex (not installed)` when the binary isn't on the host's PATH.
  - A red inline warning sits under the agent picker when the selected agent is missing.
  - A grey "Checking installed agents..." note shows while the probe is in flight.
- Plumbing:
  - Extracted the `which` + `--version` probe into `claude_session_lib::probe` so it's reusable outside the spawn-time diagnostic.
  - New `shared::AgentInstall { agent_type, installed, resolved_path, version }`.
  - New WS request/response pair: `ServerToLauncher::ProbeAgents { request_id }` → `LauncherToServer::ProbeAgentsResult { request_id, agents }`.
  - Backend correlates responses via a parallel `pending_probe_requests` map (mirrors the existing directory-listing pattern).
  - New REST endpoint `GET /api/launchers/{id}/probe-agents` that triggers the round-trip and returns the install state with a 5 s timeout.

## 2.5.30

- **Attach the raw Codex frame to typed-decode-failure portal messages.** When a codex frame fails our bundled `codex-codes` schema (the canonical `missing field 'callId'` mismatch with codex CLI 0.130.0's approval requests, see #703), the proxy interrupted the turn but left the user without the offending frame's content — making upstream bug reports hard to file. A new `CodexFrameCaptureLayer` tracing layer watches codex-codes' `[CLIENT] Received: <raw>` DEBUG events at runtime and stashes the last 8 frames in a process-wide ring buffer. On `Error::Json` the codex I/O task drains the most-recent entry (overwhelmingly the offending frame, since capture happens microseconds before the typed-decode failure on the same thread) and emits it as a portal message with a fenced JSON block, ready to copy-paste. The layer declares per-callsite `Interest::always()` only for `codex_codes::client_async` DEBUG events, so it sees them even when `RUST_LOG=info,…` would otherwise suppress them — without flooding journald (the fmt-layer EnvFilter is now per-layer, not global). Installed in both `launcher/src/main.rs` and `proxy/src/main.rs`.

## 2.5.29

- **Auto-retry upstream-429 turns with full-jitter exponential backoff.** When Anthropic's API rate-limits a request, `claude --print` doesn't fail — it streams an assistant message starting with `API Error: Server is temporarily limiting requests (not your usage limit) · Rate limited` and emits a `Result` with `is_error: true`. From the user's POV the session just ate a turn and went quiet. `claude_io_task` now caches the last user input, watches each turn for that exact shape, and on a match auto-retries with `delay ∈ [0, min(30, 2^attempt)]` seconds (full-jitter prevents N concurrent stuck sessions from re-firing in lockstep when the limit window resets). Up to 4 retries per user input; each retry emits a portal message announcing the wait and attempt count; on max-out we surface a final portal note asking the user to resend. If a new user input arrives during the backoff, it cancels the retry and runs normally — the user can always override automation. Counter resets on every fresh user input and on every successful turn.

## 2.5.28

- **Stop rendering protocol-agnostic messages as "Codex Raw" blocks on Codex sessions.** The synthetic user-echo (PR #691, fixing the "Codex Raw" pile-up) and the backend's `Portal` envelope both have `type: "user"` / `type: "portal"` and parse cleanly as `ClaudeMessage` — but the old dispatch sent every message on a Codex session straight to the Codex renderer, which only knows codex-specific shapes (`item.*`, `turn.*`) and falls anything else through to the "raw JSON" catch-all. The user saw their own input echoed as a JSON dump. Dispatch now matches on the message shape first (any recognized `ClaudeMessage` variant renders the same on both agents) and only routes the remaining unknown-shape messages to the Codex renderer.

## 2.5.27

- **Agent-type watermark behind session pills.** Each pill now shows the actual brand mark of the agent backing it — Anthropic's stylized burst behind Claude sessions, OpenAI's hex-knot behind Codex sessions — anchored to the left edge of the pill, clipped to the rounded silhouette via the existing `overflow: hidden`, sized to overflow the corner a bit. Light grey at 0.22 opacity, sits below the foreground text and indicators so legibility is unchanged. The old "Codex" text badge is removed since the watermark carries the same signal.
- New static assets: `frontend/assets/anthropic-mark.svg` (recolored to white from the source SVG so it tints down to the Tokyo Night palette cleanly) and `frontend/assets/openai-mark.png` (alpha-cut from the source PNG, RGB-negated so the logo reads light on the dark pill).

## 2.5.26

- **Fix #703 — codex sessions no longer wedge after a typed-decode failure.** The 2.5.22 patch for #695 kept the session alive after `codex-codes` failed to deserialize a frame, but silently dropped the frame. When the lost frame was a server→client approval request (the `callId`-missing variant in codex 0.130.0's `item/{commandExecution,fileChange}/requestApproval`), codex blocked the turn waiting for a reply it would never get; the proxy stayed in `turn_active = true` and rejected every subsequent user prompt with `Received input while Codex turn is active`. From the user's POV the session looked alive but ignored them. Now, on `codex_codes::Error::Json` during an active turn, `claude-session-lib` (a) emits a `turn.failed` event to the frontend so the hang is visible, and (b) sends `turn/interrupt` so codex unblocks and emits `turn/completed` (Interrupted), which clears the flag through the normal path. If `turn_interrupt` itself errors, the flag is force-cleared to avoid a permanent wedge.

## 2.5.25

- Awaiting pill border + indicator back to red (`--error`) instead of orange. The pulse animation stays gone (#684's complaint was the strobing, not the color).

## 2.5.24

- **Session pill colors: blue focus ring, no more pulsating red on awaiting pills.** The 2.5.11 "bright-red active pill indicator" was too loud; the awaiting-pulse animation made it worse for any tab the agent was still chewing on. Now:
  - **Focused pill**: accent-blue border + soft blue background tint + 2 px blue ring.
  - **Awaiting pill** (agent still working): subtle orange border, no animation. The existing in-pill indicator glyph still picks up an orange tint to flag the state without the page strobing.
  - **Focused + awaiting**: focus state dominates — keeps the blue ring/background; the indicator glyph alone signals "still working."
- Dropped the `@keyframes awaitingPulse` rule.

## 2.5.23

- **Fix #692 — portal reminder no longer bloats the user transcript.** Two parts:
  - Reflowed `claude-session-lib/portal_reminder.md` to single-line paragraphs and bullets. The previous 72-column hand-wrapping turned every continuation into a new visual line in the rendered preview; long-line markdown lets the renderer reflow to the available width.
  - Dropped the user-bound `PortalMessage::reminder` emission entirely. The collapsed "Portal features" block on the frontend was content the user already knew (they built the portal), and the actually-useful side — re-priming the agent's affordance knowledge after a fresh start / compaction — is the stdin injection, which stays. The `PortalContent::Reminder` enum variant and its frontend renderer are kept around so historical DB rows still deserialize cleanly.
- **Filter Claude's user-message echo of `<system-reminder>` wrappers.** When the proxy injects the reminder via stdin, Claude echoes it back on stdout as a `ClaudeOutput::User` whose content is the raw `<system-reminder>…</system-reminder>` text. Forwarding that to the frontend leaked the wrapper text into the transcript as if the user had typed it. The output forwarder now detects user-message echoes whose first text block starts with `<system-reminder>` and drops them before forwarding.

## 2.5.22

- **Fix #695 — codex sessions no longer die on typed-decode errors.** Codex CLI 0.130.0 emits at least one frame type (likely a notification with a `callId` field our bundled `codex-codes` 0.128.0 doesn't model) that fails strict deserialization. Previously the proxy turned that into `SessionError::CommunicationError` and killed the session; from the user's POV the agent just vanished mid-turn. Now `codex_io_task` matches on `codex_codes::Error::Json` specifically, logs a warning, and continues the loop so the rest of the session stays alive. Other error variants (I/O, protocol, server-closed) still terminate as before.

## 2.5.21

- **Fix #678 — macOS ARM64 CI no longer flakes on broken-shim runner images.** The 2.5.8 PATH workaround was based on a wrong theory: on affected `macos-14` images, the files at `~/.cargo/bin/{cargo,rustc}` are *both* actually `rustup-init`, not toolchain shims. `dtolnay/rust-toolchain@1.92.0` exits 0 on these images but leaves the install half-broken — `rustup run` couldn't help either, because cargo internally invokes `rustc -vV` and got `rustup-init 1.29.0` back. Replaced the dtolnay step on all four macOS jobs (`build-proxy-macos-arm64`, `build-launcher-macos-arm64`, `build-macos-arm64`, `build-macos-intel`) with an explicit `rm -rf ~/.cargo ~/.rustup` followed by a fresh `curl | sh -s -- -y --default-toolchain 1.92.0 --profile minimal` install. Plain `cargo build` works after that. Dropped the obsolete PATH workaround.

## 2.5.20

- **Add a triple-click "Update & Restart" button to the Launchers panel.** Lets an operator push a launcher to fetch the latest agent-portal release from GitHub and restart itself, straight from the dashboard. Three-stage confirmation prevents fat-finger accidents:
  - **Click 1**: gray "Update & Restart" → yellow "Wait, really?"
  - **Click 2**: yellow → red "Are you absolutely positively sure?"
  - **Click 3**: red → muted "Restarting…" (POSTs `/api/launchers/:id/update`)
- Backend translates the POST into a new `ServerToLauncher::UpdateAndRestart` WebSocket message; the launcher receives it, calls `portal_update::check_for_update` (the same path as the `agent-portal update` CLI subcommand), then `service::restart()` via systemctl/launchctl. If the launcher isn't running under a service manager it just exits and relies on an external supervisor to respawn.

## 2.5.19

- **Fix "Codex Raw" pending messages piling up at the bottom of the transcript.** Codex's app-server protocol doesn't echo user input back the way Claude's CLI does, so the frontend's optimistic-send pending entry never matched anything and accumulated on every send. The pending entries were rendered through the Codex renderer (which doesn't recognize the `{type: "user", _pending: true}` shape) and fell into the "Codex Raw" catch-all. Fix: `codex_io_task` now emits a synthetic user echo (parses as `ClaudeOutput::User` with a top-level `content` field) right before kicking off the Codex turn, so the frontend's existing content-match pending-clear path fires identically for both agents. Portal-reminder injections wrapped in `<system-reminder>` tags are filtered out of the echo path so they stay invisible to the user.

## 2.5.18

- **Bump `codex-codes` 0.101.1 → 0.128.0.** Upstream restructured `ServerMessage` from the loose `{ method, params }` envelope into typed enums (`Notification(Notification)` and `Request { id, request: ServerRequest }`). Adapted the proxy's Codex dispatcher in `claude-session-lib/src/session.rs` to:
  - Match `ServerMessage::Notification(notif)` and recover `(method, params)` via `notif.into_envelope()` — keeps the existing string-based downstream dispatch intact.
  - Match `ServerMessage::Request { id, request }`, pull the method via `request.method()`, and serialize the typed param struct (`CmdExecApproval`, `FileChangeApproval`, or `Unknown`) back to a `Value` so the rest of the approval-flow code is unchanged.
- Three new `Notification` variants are now modeled (`AccountRateLimitsUpdated`, `McpServerStartupStatusUpdated`, `RemoteControlStatusChanged`); they fall through to our existing `_ => Skip` arm via `notif.into_envelope()`, so they're handled implicitly without further code.

## 2.5.17

- **Inject a "portal features reminder" into the agent at session start and after each context compaction.** Reminds the agent which portal-specific features are available (auto-formatted links, KaTeX, image rendering, AskUserQuestion, session sharing, etc.) so it can actually use them after a fresh start or a compaction-induced amnesia. The reminder is wrapped in `<system-reminder>` tags on the agent side and emitted as a collapsed `PortalContent::Reminder` block on the frontend so it doesn't clutter the transcript.
- The reminder body lives in `claude-session-lib/portal_reminder.md` as a readable markdown file (baked into the binary via `include_str!`), and operators can override it at runtime by pointing `PORTAL_REMINDER_FILE` at a readable path. Missing/unreadable overrides log a warning and fall back to the bundled default. Documented in `Dockerfile`, `docs/DEPLOYING.md`, `docs/DOCKER.md`, and `CLAUDE.md`.
- Extracted the duplicated "is this a compaction-end marker?" predicate into `shared::is_compaction_boundary(&CCSystemMessage)`. The Claude CLI emits this boundary under several subtype spellings (`compact_boundary`, `compaction`, `context_compaction`, `summary`) and three call sites were inlining the disjunction — they now all go through the shared helper.

## 2.5.16

- **Fix #684 — KaTeX hasn't actually been rendering math on any page load.** The Subresource Integrity hash on the `<script>` tag for KaTeX's `auto-render.min.js` in `index.html` did not match the file the CDN actually serves. Browsers silently refuse to execute scripts whose SRI hash mismatches, so `window.renderMathInElement` never landed on the page, and every call to our `renderMathInNode` helper silently no-op'd. Verified by a standalone harness (`frontend/dev-tools/katex-isolated.html`) that loads the same scripts from the same CDN and reports "renderMathInElement not on window". Replaced the stale hash with the real `sha384-43gviWU0YVjaDtb/GhzOouOXtZMP/7XUzwPTstBeZFe/+rCMvRwr4yROQP43s0Xk`.
- Also hardened `katex-helper.js` with a queue-and-retry path for the unlikely cold-load case where the helper fires before the CDN scripts finish loading, and added diagnostic logging so future failures surface in the browser console instead of being silently swallowed.
- Added a regression test confirming math in messages mixing `<thinking>` HTML blocks + fenced ```latex``` code + display `$$…$$` survives the markdown placeholder round-trip into `Event::Text`. This disproved earlier hypotheses that the bug was in our markdown pipeline.

## 2.5.15

- Pills are now 180 px wide (was 150 px, +20%) and the vertical rail tray shrinks from 240 → 200 px to match — no more dead space on the right of the column.

## 2.5.14

- Surface speech-to-text errors instead of silently ending the recording. Previously, when the backend speech recognition task failed early (bad credentials, recognizer setup error, Google API rejecting the stream, …), the result channel just closed and the WebSocket handler blindly emitted `VoiceEnded`, so the user saw the mic button "return immediately" with no explanation. Now the recognition task reports its final status via a oneshot channel, and the handler emits `VoiceError` with the actual error string when something went wrong.

## 2.5.13

- Session pills now have an explicit **150 px** width in both horizontal and vertical rail modes — previous content-driven sizing kept getting pushed by long folder names hitting min-content widths inside the flex children. `.pill-name` is `flex: 1; min-width: 0` so it fills the remaining space and ellipsizes cleanly. Vertical-mode `width: 100%` override removed.
- Trim the gap between the last visible message and the input bar: dropped the 0.5 rem bottom padding I added in 2.5.11 and shrunk the input form's top padding from 0.5 → 0.3 rem.

## 2.5.12

- Actually shrink the session pill width — #680's `.pill-name` change only knocked ~14% off the outer pill since the surrounding chrome stayed the same size. Pill-name width 120 → 100 px, pill horizontal padding 0.75 → 0.55 rem, and inter-item gap 0.5 → 0.35 rem, for ~25% total outer-width reduction in horizontal mode.

## 2.5.11

- Trim message stream right/bottom padding to match the previously-halved left side (1.5 rem → 0.75 rem right; bottom 0 → 0.5 rem)
- Tighten the `>` input prompt on both sides (negative left margin to match the existing negative right margin)
- Bright-red active pill indicator (was accent blue) so the focused session is immediately obvious
- Pill text width shrunk by 20 % (150 → 120 px) and the `▼` menu toggle now sits above the text in stacking order (z-index 2) so it stays readable when folder/branch labels are long

## 2.5.10

- Fix #676 — LaTeX math no longer drops when a message mixes equations with markdown that contains `_` (e.g. `$\sigma_{1D}$`). Math regions (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`) are now extracted before markdown parsing so pulldown-cmark can't split equation text on emphasis-meta characters; the math is restored verbatim in `Event::Text` so KaTeX auto-render can find the delimiters in a single DOM text node. Skips inline-code, fenced-code, and `$5`-style dollar amounts.
- Fix #675 — Copy button icon swapped to the canonical Lucide two-overlapping-rectangles glyph so the copy affordance is immediately readable.
- Tighten horizontal spacing between the `>` prompt and the input textarea (negative margin-right of 0.35 rem on `.input-prompt`).

## 2.5.9

- Expand the rail position setting from horizontal/vertical to four choices: **Top**, **Bottom**, **Left**, **Right**. Top is unchanged (default); Bottom places the rail under the input bar; Left and Right place the rail as a 240 px column on either side. Existing `horizontal`/`vertical` localStorage values are auto-migrated to `top`/`left`.
- Halve the left padding on the message stream (1.5 rem → 0.75 rem) so message bodies sit closer to the left edge.

## 2.5.8

- CI: prepend `$HOME/.cargo/bin` to PATH on macOS jobs in `ci.yml` and `release.yml` to work around a `macos-14` runner image variant where a preinstalled `rustup-init` shadowed the rustup-managed `cargo` shim, causing intermittent `error: unexpected argument 'build' found` failures

## 2.5.7

- Bump `claude-codes` 2.1.117 → 2.1.140 — fixes a latent proxy data-loss bug where the structured `tool_use_result` field (e.g. AskUserQuestion's `{questions, answers}`, Bash's `{stdout, stderr, exit_code}`) was being dropped by the typed serde round-trip in the proxy on the way to the frontend
- Also picks up `UserMessage.timestamp` (CLI-emitted ISO-8601 timestamp echoed alongside tool results)

## 2.5.6

- Force pills to actually stack vertically in vertical-rail mode (the previous fix laid out the wrapper as a left column but pills inside the rail still flowed horizontally on some browsers). Adds `!important` on `flex-direction: column` and `overflow-x/y` for the vertical rail, plus `align-items: stretch` so pills take full column width.

## 2.5.4

- Fix vertical pill rail orientation: previous CSS selector targeted the wrong element (`.session-rail` is wrapped in `.session-rail-container`, so a direct-child combinator never matched); now styles the wrapper for the left-column layout

## 2.5.3

- Stabilize expand/collapse state of message components across new messages: bash command toggle, `ExpandableText` "... more chars", and image viewer no longer reset when later messages arrive
- Keys for message groups and grouped messages now derive from `_created_at` instead of position, so inserting a user message between assistant groups stops invalidating later groups' identity

## 2.5.2

- Pause auto-scroll when the user scrolls up to read older output; new messages no longer yank the view to the bottom
- Show a floating "Jump to live ↓" pill in the messages area when tailing is paused; click to resume

## 2.5.1

- Add Appearance tab in Settings with a horizontal-vs-vertical toggle for the session pill rail
- Preference persists in browser localStorage (`claude-portal-rail-orientation`)
- Vertical layout puts the pill rail down the left side; horizontal keeps the current top-bar layout

## 2.5.0

- Chunked image uploads over a new `/ws/session/upload` stream (#666)
- Large images (>= 1 MB on disk) now stream from proxy to backend as raw binary chunks, bypassing the WS frame size cap that previously stuck sessions in reconnect loops
- Avoids base64 encoding on the wire for large images — chunks are sent as native binary WebSocket frames
- Backend reassembles chunks in an in-memory upload buffer, enforces `PORTAL_MAX_IMAGE_MB`, then publishes the image at `/api/images/{uuid}` like before
- Proxy now rejects images over the cap locally (textual portal message) instead of queueing oversized payloads that fail forever on replay

## 2.4.37

- Move "time ago" label from message header to small unobtrusive footer at bottom-right of assistant messages

## 2.4.36

- Left-justify copy button in user/portal message headers (was pushed to far right)

## 2.4.35

- Show live-updating "X minutes ago" label on assistant message headers, with exact local time tooltip

## 2.4.34

- Place copy button next to model name in assistant headers (was at far right)

## 2.4.33

- Add clipboard copy button to message headers (assistant, user, portal) to copy raw markdown text

## 2.4.32

- Fix splash page not scrollable on mobile (overflow:hidden was on body instead of session container)

## 2.4.31

- Add stable keys to grouped assistant messages to preserve expanded state on new messages

## 2.4.30

- Disable Google STT single-utterance mode to prevent immediate session end on quiet/silent mic inputs (common on macOS)

## 2.4.29

- Clear pending send queue when assistant/result arrives (fixes stuck pending state for slash commands)
- Match pending sends by content on user echo so lost messages don't consume unrelated entries

## 2.4.28

- Up/down arrows in input now move cursor between lines; history navigation only triggers at the first/last line

## 2.4.27

- Render LaTeX math in messages via KaTeX (`$inline$`, `$$display$$`, `\(...\)`, `\[...\]`)

## 2.4.26

- Serve large images via HTTP instead of WebSocket (partially addresses #655)
- Backend extracts base64 images >64KB from portal messages, stores in memory, replaces with `/api/images/{uuid}` URLs
- Frontend renders both URL and base64 image sources
- Eliminates WS frame size issues for large images

## 2.4.25

- Add 5-second query timeout to retention cleanup tasks (#616)
- Cap retention deletes at 1000 messages per cycle to avoid long-running queries

## 2.4.24

- Limit launcher registrations to 10 per user (#617)

## 2.4.23

- Reduce spend broadcast interval from 5s to 30s and skip when no clients connected (#618, #614)
- Use single aggregate query instead of per-user queries for spend updates

## 2.4.22

- Fix stuck reconnect loop: drop pending messages after 5 consecutive replay failures (#650)

## 2.4.21

- Click truncated bash commands to expand full command text

## 2.4.20

- Fix false autolinks: angle-bracket Rust paths like `<crate::Type>` no longer render as clickable URLs

## 2.4.19

- Fix iOS keyboard gap: use position:fixed on mobile to track visual viewport

## 2.4.18

- Fix iOS Safari scroll bounce creating stuck dead space below messages

## 2.4.17

- Optimistic send: user messages appear instantly with a pending indicator, confirmed when server echoes back

## 2.4.16

- Linkify URLs in inline code spans, error messages, and file content previews

## 2.4.15

- Linkify URLs in code blocks, tool results, expandable text, and thinking blocks

## 2.4.14

- Show session init info bar with model, version, fast mode, MCP servers, and tool count

## 2.4.13

- Show truncation warning when assistant message hits max_tokens
- Add service tier and inference region to model name tooltip
- Add ephemeral cache details to usage tooltip

## 2.4.12

- Show cost, stop reason, fast mode, errors, and permission denials in result stats bar
- Add model usage breakdown to result timing tooltip

## 2.4.11

- Bump claude-codes to 2.1.117
- Render new content block types: ServerToolUse, WebSearchToolResult, McpToolUse, McpToolResult, CodeExecutionToolResult, ContainerUpload
- Render web search citations on text blocks
- Show unknown content blocks as collapsible JSON instead of silently dropping them

## 2.4.10

- Add token renewal button in settings credentials panel
- Add local-time tooltip on message headers

## 2.4.9

- Fix pill scroll-into-view when tabbing to off-screen sessions

## 2.4.8

- Purge expired device flow codes every 60 seconds (#615)

## 2.4.7

- Global thin scrollbar styling (6px, subtle, matches dark theme)

## 2.4.6

- Add sortable columns to admin users table (Email, Name, Status, Sessions, Spend, Created)

## 2.4.5

- Fix admin stats 500: cast SUM(bigint) to ::bigint in raw SQL queries (#627)
- Add raw SQL type safety guidance to CLAUDE.md

## 2.4.4

- Unify config directory: launcher now uses `~/.config/agent-portal/` (same as proxy)
- Migrate launcher config from TOML to JSON (`launcher.toml` -> `launcher.json`)
- Auto-migrate old `~/.config/claude-portal/launcher.toml` on startup
- Both proxy and launcher use `directories::ProjectDirs` for consistent paths across platforms
- Update install script to use new config path and JSON format

## 2.4.3

- Add GitHub link to minimal splash page footer

## 2.4.2

- Add `SPLASH_TEXT` env var for minimal login page (heading + sign-in button + version + bug link)
- When unset, the full marketing splash page is shown as before

## 2.4.1

- Replace rust-embed + startup brotli/gzip compression with memory-serve (build-time compression, zero startup cost)
- Remove rust-embed, brotli, flate2, mime_guess dependencies
- Closes #613

## 2.4.0

- Upgrade axum 0.7 to 0.8, ws-bridge 0.1 to 0.2, tokio-tungstenite 0.24 to 0.28
- Upgrade tower-cookies 0.10 to 0.11, tower_governor 0.4 to 0.8
- Migrate route paths from `:param` to `{param}` syntax

## 2.3.15

- Add `agent-portal service logs` command with `-n` (line count) and `-f` (follow) options

## 2.3.14

- Resume existing Claude sessions on launcher restart instead of creating new ones

## 2.3.13

- Fix admin stats page crashing on non-401/403 error responses

## 2.3.12

- Render each message in assistant groups as its own component so only new messages re-render
- Revert thread_local expanded state hack (no longer needed)

## 2.3.11

- Fix expanded "... more chars" content collapsing when new messages arrive

## 2.3.10

- Upload progress bar fills over 1.5s minimum and collapses with animation after completion

## 2.3.9

- Defer stale session cleanup on backend startup to give proxies time to reconnect, fixing sessions disappearing from the pills menu after a backend restart

## 2.3.8

- Add `agent-portal service start`, `stop`, and `restart` subcommands

## 2.3.7

- Fix awaiting-input detection to skip noise message types (portal, error, system, rate_limit_event)
- Extract shared `is_claude_awaiting` function used by both REST load and WebSocket paths

## 2.3.6

- Auto-delete completed cron sessions to prevent UI clutter (costs preserved)
- Hide cron sessions by default in the session rail

## 2.3.5

- Fix admin stats endpoint returning empty body due to SQL type mismatch (COALESCE returns numeric, Diesel expects float8)

## 2.3.4

- Fix send bar not clearing after sending with attachments

## 2.3.3

- Consolidate remaining duplicate user ID extraction into shared auth module
- Convert scheduled_tasks and proxy_tokens handlers to use AppError

## 2.3.2

- Add unified AppError type for backend handlers with structured error responses
- Consolidate duplicate auth extraction into shared `auth::extract_user_id`
- Convert sessions, messages, launchers, and sound_settings handlers to use AppError

## 2.3.1

- Remove stale cli-tools crate and dead API types
- Remove unused dependencies from backend, proxy, launcher, and shared

## 2.3.0

- Auto-renew launcher auth tokens over WebSocket when within 7 days of expiry
- Add token expiry warning icon on session pills for sessions from launchers with expiring tokens
- Add Launchers tab to Settings page with manual token renewal button
- Add POST /api/launchers/:id/renew-token endpoint for manual token renewal

## 2.2.3

- Fix Codex sessions failing to start from launcher: resolve binary path via `which` before spawning

## 2.2.2

- Add favicon and browser tab icon for link previews

## 2.0.4

- Fix multiline user input getting flattened when rendered in message history

## 2.0.3

- Bake user's PATH into systemd/launchd service so spawned agents can find `claude`

## 2.0.2

- Fix install script: `agent-portal install` → `agent-portal service install`

## 2.0.1

- Version bump to 2.0.1

## 1.3.49

- Add Linux aarch64 support (CI builds, auto-update, install script)

## 1.3.48

- Fix session reconnect race: old connection cleanup no longer overwrites newer connection's registration

## 1.3.47

- Fix oneshot drop race causing launcher sessions to not reconnect on server restart

## 1.3.46

- Show proxy version badge in session pill, color-coded by staleness

## 1.3.44

- Break up settings.rs into sub-components (TokensPanel, SessionsPanel, SoundsPanel)

## 1.3.43

- Update claude-codes to 2.1.51 (typed enums for message fields)
- Handle unparsable CLI messages gracefully instead of crashing sessions

## 1.3.42

- Detect subagent task completion via tool_result fallback when task_notification is missing (--print mode)

## 1.3.41

- Add repo URL to pill menu with 3-state display: PR link, repo link, or "No Repository Detected"
- Proxy detects GitHub repo URL via `gh repo view` and sends it alongside branch/PR info

## 1.3.40

- Break up admin.rs into sub-components per tab (overview, users, sessions, raw messages)
- Review and update all docs for accuracy across 15 files
- Fix subagent completion handling in history loading path to preserve task data
- Replace catch-all status mapping with explicit CCTaskStatus variant matching

## 1.3.38

- Add "Add machine" button to launch dialog for setting up new launchers
- Remove Service section from settings Credentials tab

## 1.3.37

- Move install under service subcommand (`agent-portal service install`)

## 1.3.36

- Add agent install setup under Service section in Credentials settings tab

## 1.3.35

- Add bash-style Tab completion to launch dialog path input

## 1.3.34

- Fix launch dialog bugs and refactor DirBrowser

## 1.3.33

- Prevent duplicate launchers per host-user and fix tilde expansion

## 1.3.32

- Launcher cleanup: fix task abort, URL dedup, send error logging, config path

## 1.3.31

- Show session details (name, host, directory, branch, agent) in portal message on connect/reconnect

## 1.3.30

- Unify admin and settings page layout styles

## 1.3.29

- Fix transparent admin/settings overlay background

## 1.3.28

- Move bug report to bottom-right link with bug emoji

## 1.3.26

- Fix Shift+Tab keyboard hint text

## 1.3.25

- Keep server shutdown banner until first message received from reconnected server

## 1.3.24

- Remove unused dead code methods from CommandHistory

## 1.3.23

- Fix result message duplicating previous assistant message text

## 1.3.22

- Fix launcher crash: install ring crypto provider for rustls 0.23

## 1.3.21

- Fix sparkline tick subpixel rendering artifacts via GPU compositing

## 1.3.20

- Fix tasks drawer pull-tab clipped by overflow:hidden on drawer container

## 1.3.19

- Add `agent-portal login` subcommand for explicit authentication
- Add `agent-portal install` subcommand to install as system service
- Add `agent-portal update` subcommand to update binary and restart service
- Install script no longer auto-installs system service
- Updated frontend setup instructions with 3-step flow (install, login, service)

## 1.3.18

- Differentiate task and portal message colors (tasks=purple, portal=teal)
- Sparkline tick colors now match their message type colors

## 1.3.17

- Rename agent-launcher to agent-portal
- Make launcher the default install path (install script downloads agent-portal, installs as service)
- Launch button available to all users (not just admin)
- Launch dialog shows install instructions when no launchers are connected
- agent-portal recommends service install when run interactively

## 1.3.16

- Backend sends max image size to proxies via RegisterAck; proxy uses it instead of local env var
- Remove frontend image size check (backend/proxy is authoritative)

## 1.3.15

- Fix stale subagent entries persisting across page reloads by clearing task state on history reload
- Tasks sidebar panel now slides in/out with the drawer instead of instantly appearing/disappearing

## 1.3.14

- Refactor sender attribution: store actual sender user_id in DB, reconstruct display info at query time

## 1.3.13

- Add user name attribution to messages in shared sessions

## 1.3.12

- Increase default image max size from 2 MB to 10 MB

## 1.3.11

- Update claude-codes to 2.1.49 (String-to-enum migration for subtype, stop_reason, status)
- Re-export `CCSystemSubtype` from shared

## 1.3.10

- Use typed claude-codes structs for task parsing instead of raw JSON field access
- Parse task_type, task_status, and task_usage via typed deserialization in both component logic and renderers

## 1.3.9

- Tasks sidebar: header bar toggles open/close (removed separate X button)
- Tasks sidebar: show running task count in title bar

## 1.3.8

- Add widget protocol specification (`docs/WIDGET_PROTOCOL.md`)

## 1.3.7 and earlier

- See git history for previous changes.
