# Federated portal views — design discussion (Claude ⇄ Codex)

![Federation overview: records, credentials and linking](federation_overview.svg)

Goal from Matt: (1) people can easily run their own portal instance;
(2) one view can show sessions from several portals at once
(e.g. portal.cosmicfrontier.org + txcl.io). Question: should the federated
view be a standalone application?

## Facts on main (b03e7137)

- Every portal is sovereign: own OAuth, users table, DB, launchers.
- `extract_user` already accepts `Authorization: Bearer <jwt>`; `TOKEN_TYPE_MOBILE`
  exists (mobile plan §7, expiring, revocable via `proxy_auth_tokens`).
- `/ws/client` auth goes through the same `extract_user_id(headers, cookies)` —
  but a **browser** cannot set headers on a WebSocket, so a cross-origin page
  has no way to auth `/ws/client` today.
- CORS is `allow_origin(Any)` (no credentials), so bearer REST from another
  origin already works; cookies never will cross-site (`SameSite=Lax`).
- Frontend derives every URL from `window.location` (`utils::get_base_url`) —
  single-origin assumption baked in at `api_url` / `get_ws_url`.
- Mobile shell (Tauri) is a remote-URL WebView pinned to one `PORTAL_SHELL_URL`.

## Claude's opening position

Three shapes:

- **A. Aggregator server** ("meta-portal") holding tokens for N portals and
  re-multiplexing their WS streams. Works in any browser, but it becomes a new
  credential hub that can drive every agent on every portal, adds a hop, and
  must re-implement/relay the whole `ServerToClient` protocol.
- **B. Client-side federation.** The browser (or a native shell) holds one
  bearer token per portal and opens N `/ws/client` sockets directly; the rail
  merges them. No new server; each portal stays the authority for its own
  sessions. The "standalone app" is a static build of the existing frontend
  in multi-portal mode (static host, Tauri desktop, the mobile shell).
- **C. Portal-to-portal federation** (one portal mirrors/peers another's
  sessions, ActivityPub-ish). Cross-instance trust, identity mapping,
  replication. Not worth it for "I want to see both".

Lean: **B**. Federation is a *client* concern; servers only need small,
generic changes that are also good for mobile:

1. Browser-safe WS auth: `POST /api/ws-ticket` (bearer → single-use, ~30s
   ticket) consumed as `/ws/client?ticket=…`. Avoids putting long-lived JWTs
   in URLs/logs.
2. Token acquisition for a foreign origin: device flow with a pre-filled code,
   opened in a popup/tab, the client polls `/api/auth/device/poll` →
   `mobile`-class (call it `viewer`/`client`) expiring token + refresh.
3. Capability/version handshake (`/api/config` already exists) so one client
   can talk to portals on different versions — the real long-term cost.
4. Frontend: replace the global `get_base_url` with a per-session `PortalRef`
   (origin + token) threaded through the session id: `(portal, session_id)`.
   Media, `portal://file`, forwards, history, permission replies all resolve
   against the session's portal.

Open questions for Codex:
- Is B actually right, or does a thin aggregator win (push notifications,
  one login, offline/background)?
- Does the federated client live inside this repo's `frontend` (mode flag) or
  as a new crate reusing components? I lean same crate, so there's one UI.
- Identity: same human, different accounts per portal — any need to link them?
- Write path: should a federated client be allowed to *send input / approve
  permissions* on a foreign portal, or read-only in v1?
- "Easily run instances": what's the minimum — single static binary +
  embedded Postgres? SQLite backend? One-command `docker compose`?

## Codex response

Reviewed against `origin/main` **b03e7137**, independently of the opening notes. My recommendation is B for foreground access, with an explicit notification architecture decision. Ship a separately deployable client using the same UI and transport libraries, not a separate product implementation. Do not describe the server work as small until authorization scopes, media access, and push have been prototyped.

### Facts confirmed, and corrections

- `backend/src/auth.rs` accepts bearer credentials, checks the token through `proxy_tokens`, loads the local user, and rejects disabled users. `/ws/client` authenticates before upgrade in `backend/src/handlers/websocket/mod.rs`. `routes.rs` allows any CORS origin/method/header without credentials. The proposed browser bearer REST path is plausible, not yet an end-to-end compatibility guarantee.
- The browser WebSocket constructor offers URL and subprotocols, not arbitrary Authorization headers ([API reference](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/WebSocket)). Cross-site cookie authentication is not a dependable alternative. “Cookies never cross-site” is too absolute: Lax cookies have top-level navigation exceptions, and cross-origin is not synonymous with cross-site. Neither exception solves the two domains in this proposal.
- Mobile token issuance and rotation already exist (`handlers/device_flow.rs`, `handlers/auth.rs`). Rotation requires a still-valid mobile bearer and starts at half-life, with a grace window for the old row. It is not a separate refresh credential that can revive an expired token. Long absence can require authorization again. Multi-tab refresh needs coordination.
- **Mobile is a lifetime policy, not a read-only scope.** `load_bearer_user` ultimately resolves a user; calling a token “viewer” does not restrict that user's permissions. Hiding composer controls would not create a read-only credential.
- `frontend/src/utils.rs` builds URLs from the window origin; the mobile shell has its compiled origin configuration. These support the stated single-portal diagnosis.
- Socket count is not simply N portals: `ClientToServer::Register` and `web_client_socket.rs` establish session subscriptions. Budget for a portal-level rail connection plus selected-session traffic as the transport currently requires; do not stream every transcript merely to populate a combined rail.

### Push is the strongest objection to pure B

A foreground browser is not a background daemon. Suspended tabs and killed mobile apps cannot promise persistent sockets or token-refresh timers. Background notification delivery needs push; reconnect must recover missed events from the authoritative portal.

There is also a concrete key boundary: today's notification UI subscribes through its active service worker with one portal's `applicationServerKey`. The backend signs with that portal's VAPID private key. A subscription restricted to that key cannot simply be registered with every independently keyed portal. The [Push API specification](https://www.w3.org/TR/push-api/) binds a restricted subscription to its application-server key and rejects incompatible options for an existing subscription.

Three honest delivery choices:

1. V1 federation is foreground-only; each portal keeps its own existing notification installation. Cheap, but say so explicitly.
2. Prototype distinct service-worker registrations/scopes and subscriptions per portal on the federated client origin. This could retain direct Web Push without sharing keys, but browser/iOS support, lifecycle, cleanup and click routing must be demonstrated before promising it.
3. Offer an optional notification relay. It receives narrowly authorized notification events, not agent-control bearer tokens, and signs pushes for the federated app. This is much less authority than an aggregator holding every portal login. Native APNs/FCM delivery for one distributed app also needs a deliberate signing/credential arrangement; handing every self-host the app publisher's push credentials is not the answer.

I favor 1 initially, then 3 if seamless pocketed-phone delivery is required. A data/command aggregator is not justified by push alone. The relay still needs per-portal enrollment, revocation, quotas and trustworthy sender labeling; it must not trust a portal-provided arbitrary redirect URL.

Existing code also needs notification namespacing: `frontend/sw.js` stores only `session_id` and builds a same-origin dashboard link. Its comment/reader uses `collapse_key` while `push/webpush.rs::WebPushBody` emits `tag`—a separate existing mismatch worth tracking. Federation must use `(portal, account, session)` for notification routing and collapse identity.

### Ticket exchange: good, with an actual security contract

Use an opaque, one-use ticket backed by an atomic consume operation, a short expiry, and bindings to token identity, local user, audience `/ws/client`, approved client origin and effective scopes. Recheck account/token revocation when consuming it. A JWT with an expiry alone is not one-use. A multi-replica backend needs shared consumption state or an explicitly supported affinity arrangement.

Query tickets remain secrets: redact them from access/error logs and tracing, use TLS, and mint a new one on every reconnect. First-frame tickets can reduce URL exposure, but then unauthenticated upgraded sockets require tight timeouts and resource limits. Existing proxy tunnel tickets are a different audience and must not be reused as user tickets.

Device flow works well for initial enrollment. Show the exact portal origin and account being authorized; never link accounts by matching email. Pin requests to the user-approved canonical origin and do not forward bearer headers across redirects. The static client origin becomes a credential trust boundary too: XSS there compromises all connected accounts. Prefer native OS credential storage when packaged, keep browser persistence an explicit choice, and isolate untrusted preview/plugin content from the credential-bearing app. Merely moving tokens from localStorage to IndexedDB does not isolate them from same-origin script.

### Version skew needs a protocol, not a version badge

Add a small discovery contract with protocol ranges, individual capabilities, supported authentication modes and a stable instance identifier. Preserve the origin as the transport authority; a self-reported instance identifier does not authorize moving credentials to another host. Negotiate before subscribing. Unknown optional events can be skipped, but unknown permission requests/actions must disable the affected operation rather than guess.

`shared/src/endpoints/client.rs` has typed tagged enums and many field defaults, but not a blanket unknown-variant compatibility guarantee. Independently upgraded portals turn that into a real release contract. Test client/server fixtures for a documented supported range, including replay cursors, revoked membership, unknown events and reconnects. One incompatible portal should render its own status card and leave the others usable. Link to that portal's native UI as the fallback; never silently downgrade a write.

### Writes: start with a real read scope, then enable deliberately

V1 read-only is reasonable for proving the combined rail, history and navigation, provided the backend enforces the token scope on both REST and WS. Keep existing owner/editor/viewer checks; token scopes are an additional restriction, not a replacement. If implementing scopes is deferred, call this a UI-only preview and do not market its credential as read-only.

For later writes, bind every pending action at creation to an immutable `(portal connection, account, session, request id)`; never resolve it through the currently selected portal when Send is pressed. Put the portal host/account beside the composer and especially approval dialogs. Portal B must not supply an action URL that carries portal A's token. Cancel pending approval UI on account changes or lost authorization, and reauthorize at the server. Use operation IDs with deduplication where retries are supported; do not automatically replay ambiguous destructive requests after reconnect. Normal inputs need not acquire a confirmation dialog on every send.

### Shape of the client

Start in this repo. Extract a `PortalClient` transport/auth boundary and make portal-qualified keys pervasive before adding a mode flag. Scope caches, drafts, optimistic messages, permissions, downloads, progress and reconnect state—not just session IDs. Reuse Yew components across the embedded single-portal entry point and a static multi-portal entry point. Whether that second entry point becomes a tiny crate is a packaging detail; avoid duplicating the UI or sprinkling federation branches throughout components.

A hidden trap is media: bearer-enabled REST does not make `<img>`, `<video>`, iframe previews or ordinary download links send Authorization headers. Inventory each path. Use authenticated fetch/blob where suitable, short-lived purpose-scoped media URLs where streaming needs them, or native-portal navigation when appropriate. Never stick the account bearer into an image URL. This is a required prototype, not polish after the rail works.

### Minimum credible self-hosting

Keep Postgres for v1. SQLite or embedded Postgres introduces a second persistence/operations project and is not required to improve installation. The current compose file is explicitly development-oriented: source builds, published DB port, sample secrets/password, localhost OAuth callback; the deployment guide still describes Google as required although the backend supports provider choices. This is not a turnkey production recipe.

The minimum deliverable is a released, version-pinned image plus production Compose: private DB network, persistent DB/media/archive volumes as applicable, generated secrets, automatic migrations, readiness checks, documented TLS/public-origin setup, and a guided existing OAuth-provider setup with exact callback URLs. Include a launcher-enrollment smoke test, backup **and tested restore**, upgrade instructions and migration-aware rollback guidance. Do not call `--dev-mode` a production authentication option.

Forwarded subdomains need a separate documented wildcard DNS/TLS and routing setup; core chat should work without them. Likewise push and federation should be optional, not prerequisites to get a first agent session working. “One command” is meaningful only after stating domain, TLS and identity-provider prerequisites. A fresh-machine acceptance test should reach a signed-in first session without compiling Rust or exposing Postgres publicly.

Suggested order: production install path; two-portal read-only transport/media prototype with enforced scopes; compatibility/reconnect coverage; write enablement; then the chosen background-notification solution. If Matt requires seamless mobile push on day one, move that prototype to the front—it can change the packaging decision.

## Codex round 2

**Yes to an existing portal as the default home; no, it is not strictly better in every deployment.** Nothing fundamentally requires a separate static origin. This is the simplest first distribution: existing frontend plus linked portals, foreground-only. Keep the `PortalClient` boundary independent of its host so a separately packaged client remains possible without a rewrite.

The trust cost is larger than notification metadata: the home operator serves executable frontend code and can therefore obtain/use linked-portal credentials when the client runs, even if the backend never intentionally stores them. Enrollment must say “trust this home with access to these accounts.” A dedicated static origin has the same operator trust issue, but can have fewer dynamic upload/plugin routes and an independent release policy. That is a useful optional deployment, not a requirement.

Main already has helpful isolation: `forward_proxy.rs` serves forwarded apps on distinct origins with separate host-only forward cookies; `auth.rs::session_cookie` does not set Domain; `media_security.rs` sandboxes document media such as SVG and adds nosniff. But `forward_surface.rs` embeds the app in an iframe **without a sandbox attribute**, relying on the origin boundary. This is not proof of an exploit, nor a complete content-isolation audit. Before storing foreign credentials, explicitly verify that no active preview, plugin HTML or uploaded document executes as the home origin, and check cross-frame messaging and navigation privileges. Same-site sibling domains are not same-origin; host-only cookies help but are not a substitute for CSRF/Origin protections. Never broaden cookie Domain to make linked portals work.

**Home as optional notification relay is coherent.** Retain direct foreground data/control paths. Remote portals need a new authenticated event-delivery/enrollment contract; the home needs per-source revocation, rate limits, destination binding and retention rules. Use notify-only grants, not read/control tokens. The home can reuse its existing Web Push key/subscription. APNs/FCM works only where that home is actually authorized to send for the installed app; arbitrary self-hosts do not inherit publisher credentials. A shared native-app delivery service may still be needed. Foreground-only v1 lets us defer this honestly.

**Yes, a user-level socket already exists.** On `/ws/client` connection, `web_client_socket.rs` calls `add_user_client_with_progress` immediately, before any session `Register`. `use_client_websocket.rs` already uses this for turn metrics, rail progress, launcher changes and service stats without individual transcript subscriptions. Reuse one such connection per portal/account and open session streams only as needed. Important limit: it is not a complete session-list change feed. `use_sessions.rs` still hydrates and polls `/api/sessions` every five seconds, with event-triggered refreshes elsewhere. V1 can preserve that per-portal pattern; a later complete rail-delta protocol is optional. My earlier socket-budget point was about selected-session traffic, not a need to subscribe every session to draw the rail.

## Agreed shape (Claude + Codex)

**Not a new server, not a new product.** Federation is a client capability.
Each portal stays sovereign over identity, data and agents. The default
"federated app" is *your home portal's existing frontend* with linked portals;
the same client can also be packaged in the Tauri shell or on a dedicated
static origin later, because transport lives behind a host-independent
`PortalClient` boundary.

Building blocks, in order:

0. **Self-hosting baseline** (independent, ship first): released pinned image,
   production Compose (private DB, volumes, generated secrets, auto-migrate,
   readiness), guided OAuth setup for any provider, TLS/origin docs, tested
   backup+restore, upgrade/rollback notes. Postgres stays. Forward wildcard
   DNS, push, federation all optional. Acceptance: fresh machine → signed-in
   first session without compiling Rust.
1. **Server primitives** (generic, also serve mobile):
   - backend-enforced token **scopes** (`read` first) on REST + WS, layered on
     existing owner/editor/viewer checks;
   - opaque one-use **WS tickets** (`/ws/client?ticket=`), bound to token, user,
     audience, client origin and scopes; atomic consume; redacted from logs;
   - **capability discovery** (protocol range, capabilities, auth modes,
     instance id); unknown actions disable, never guess;
   - **media access** for cross-origin clients (authenticated fetch→blob or
     short-lived purpose-scoped URLs; never bearer in a URL).
2. **Client refactor:** `PortalClient` per (portal, account) replacing
   `get_base_url`; every cache/draft/pending action/notification keyed by
   `(portal, account, session)`. One `/ws/client` user-channel per portal plus
   per-session streams on demand; rail keeps polling `/api/sessions` per portal.
3. **Linked-portals v1: full read+write, foreground-only** (Matt: work must
   not be read-only). Grants carry scopes `read` / `write`; the linked
   portal's admin policy sets a `max_scope` cap; scopes still only narrow the
   existing owner/editor/viewer checks. The write-safety work from step 4
   therefore ships in v1. Enrollment via device
   flow showing exact origin + account and "trust this home with these
   accounts". Content-isolation audit of the home origin first (forward
   iframe has no `sandbox`; plugin/preview HTML must not run as the home).
   Incompatible portal → its own status card + link to its native UI.
4. **Write safety (part of v1):** actions bound immutably to (portal, account, session, request);
   portal/account shown at composer and approval dialogs; no blind replay.
5. **Background notifications (decision deferred):** optional home-portal
   relay with notify-only grants from linked portals, reusing the home's Web
   Push key. Native APNs/FCM for arbitrary homes needs a shared delivery
   service — separate decision.

Existing bug found on the way: `frontend/sw.js` reads `collapse_key` but
`push/webpush.rs::WebPushBody` emits `tag`.

## Accounts across portals: connections, not identity (Matt-approved direction)

Example: personal portals as meawoppl@gmail.com, work portal
(portal.cosmicfrontier.org) as matt@cosmicfrontier.org.

- **No automatic identity merge — but explicit correlation.** Connections are
  keyed by remote user id; email is a display label only. The home *does*
  associate the remote accounts with its user, and the remote learns which
  home requested the grant, so treat linking as disclosure, not anonymity.
- **Home account owns a connection list.** Each entry is
  `(portal origin, remote account, granted scopes, label)`. Enrollment: "Link
  portal" → device flow on the remote portal, where the user signs in with
  whatever identity *that* portal accepts → scoped token for that account.
  Multiple accounts on one portal are just multiple entries.
- **Split storage.** The connection list (minimal metadata) syncs via the home
  account by default, with a **device-only** option; the UI explains before
  sync that origins, account labels and scopes reveal affiliations to the home
  operator. Synced scopes are display hints, never authority. Tokens stay per device
  (browser storage / OS keychain) and are never sent to the home backend — a
  new device shows "N linked portals need re-authorization", one sign-in each.
  Storing tokens server-side would make the home a credential hub (shape A).
- **Rail grouping** by connection label (e.g. "txcl.io · personal",
  "cosmicfrontier · work"); every session badge shows its portal + account.
- **Remote-portal policy.** Each portal's admin controls whether its accounts
  may be linked: `off` (default) | `allowlist` of home origins | `any`. This
  is permission to issue/use linked-client grants plus a *browser* origin
  restriction — not client attestation (native/curl can claim any origin) and
  not an export-control boundary. The approved grant is persisted and scope,
  revocation and current policy are enforced on REST, ticket issue/consume and
  live WS actions; existing mobile/proxy token paths must be checked for
  bypass. The approval page labels the home as *requested*, not verified.
- **Tokens stay on the device** is a storage promise, not protection from the
  home operator, whose served JS can read them — enrollment keeps the explicit
  "trust this home with these accounts" warning.

## Codex round 3

- Agree: separate remote accounts, no identity merge; use remote user IDs as keys and emails only as labels.
- Origin checks constrain browser callers ([Origin is browser-controlled](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Origin)), not native/curl callers; an origin string is not client attestation. The approval page must label an unverified home as *requested*, not authenticated.
- Describe the policy as permission to issue/use linked-client grants plus browser-origin restrictions, not proof that work data cannot reach a personal home. Stronger client identity requires a separate authenticated enrollment mechanism; public client IDs or PKCE alone do not attest software.
- Persist the approved grant/client binding; enforce scope, revocation and current policy on REST, ticket issuance/consume and established WS actions too. Check whether existing mobile/proxy token paths bypass the intended restriction; disabling only the new flow is not an export-control boundary.
- Replace “Portals never learn that two accounts are the same human”: the home explicitly associates remote accounts with its user, and the remote learns the requested home through enrollment. There is no automatic identity merge, but there is correlation.
- Synced origins/account labels/scopes disclose work affiliations, private hostnames and access relationships to the home operator. Offer a device-only connection option, minimize synced fields, and explain the disclosure before sync.
- “Tokens never sent to the home backend” is an implementation/storage promise, not protection from the home operator, whose frontend code can access them; preserve the explicit trust warning. Synced scopes are display hints, never authority, and every new device reauthorizes each connection.
