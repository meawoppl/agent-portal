# Port Forwarding: Agent-Served HTTP Through the Portal

Status: **spec / not implemented**. This is the remaining half of issue
[#689](https://github.com/meawoppl/agent-portal/issues/689) — the download half
shipped as [PORTAL_FILE_DOWNLOADS.md](PORTAL_FILE_DOWNLOADS.md).

## Goal

An agent stands up an HTTP service on the machine its proxy runs on (a dev
server, Jupyter, a dashboard it built) and makes it reachable from the user's
browser through the portal:

```console
$ agent-portal forward 8080
Forwarding http://8080--550e8400e29b41d4a716446655440000.localhost:3000/ → localhost:8080
```

The agent pastes the printed URL into its reply; the portal auto-links it; the
user clicks it and uses the service — including WebSockets and SSE — for as
long as the session is alive.

## Design summary

- **Per-forward subdomains**, not path prefixes. Every forward gets its own
  origin: `{port}--{session}.{forward domain}`. Agent-built apps work
  unmodified (absolute asset paths, cookies, service workers), and forwarded
  apps are origin-isolated from the portal and from each other.
- **One listener.** The backend keeps its single HTTP listener and routes by
  `Host` header. No per-forward port allocation anywhere — which is why the
  CLI takes only a local port, never a remote one.
- **A generic byte tunnel over the existing session WebSocket** is the
  transport. The backend's forward handler is an ordinary streaming reverse
  proxy (hyper client) whose connector "dials" `localhost:{port}` on the proxy
  host through the tunnel. Plain requests, streamed responses, SSE, and
  WebSocket upgrades all ride the same machinery. There is deliberately **no
  separate buffered request/response protocol** — that would be throwaway
  protocol surface.
- **Auth by token handoff**, since a subdomain is a different origin and the
  portal cookie does not follow. The portal origin mints a short-lived JWT and
  redirects; the forward origin exchanges it for its own scoped cookie.

```text
browser ──HTTP──▶ backend (Host-routed reverse proxy)
                     │  TunnelOpen/Data/Close over /ws/session
                     ▼
                  proxy ──TCP──▶ 127.0.0.1:{port} (agent's service)
```

## Naming scheme

```text
{port}--{session-uuid-simple}.{PORTAL_FORWARD_DOMAIN}
```

- `port`: decimal, 1–65535.
- `session-uuid-simple`: the 32-hex-char `Uuid::simple()` form. UUIDs contain
  single hyphens in canonical form, so the hyphenless form plus the `--`
  separator makes the label unambiguous; total label length is ≤ 41 chars,
  within the 63-char DNS limit.
- Parsing is strict: `^(\d{1,5})--([0-9a-f]{32})$` on the first DNS label, and
  the remaining labels must equal the configured forward domain exactly.
  Anything else falls through to the normal router.

### Forward domain configuration

| Setting | Behavior |
|---|---|
| `PORTAL_FORWARD_DOMAIN` set (e.g. `fwd.example.com`) | Forwarding enabled; public URLs are `{scheme from BASE_URL}://{label}.fwd.example.com/`. Production needs a wildcard DNS record and wildcard TLS cert at the terminator — the backend itself stays plain HTTP. |
| Unset, `--dev-mode` | Defaults to `localhost:{PORT}`. Chrome/Edge/Firefox resolve `*.localhost` to loopback with no DNS setup, and treat it as a secure context. |
| Unset, production | Forwarding disabled; the CLI and API return a clear "forwarding is not configured on this server" error. |

Keep the forward domain a *sibling* of the portal domain (`fwd.example.com`
next to `portal.example.com`), or rely on the portal auth cookie being
host-only (it is, by default) — the portal cookie must never be in scope for
forward origins. Safari's `*.localhost` handling is historically spotty; since
the domain is config, `lvh.me` or `sslip.io` is a one-line workaround.

## Protocol changes (`shared/src/endpoints/session.rs`)

The tunnel is a multiplexed virtual-TCP layer: streams identified by UUID,
credit-based flow control so one fat stream cannot starve the session
WebSocket.

```rust
// ServerToProxy additions
TunnelOpen(TunnelOpenFields),        // { stream_id: Uuid, port: u16 }
TunnelData(TunnelDataFields),        // { stream_id: Uuid, data_base64: String }
TunnelWindow(TunnelWindowFields),    // { stream_id: Uuid, add_bytes: u32 }
TunnelClose(TunnelCloseFields),      // { stream_id: Uuid, reason: Option<String> }
ForwardOpen(ForwardPortFields),      // { port: u16 }  — sync proxy allowlist
ForwardClose(ForwardPortFields),

// ProxyToServer additions
TunnelOpened(TunnelStreamFields),    // { stream_id: Uuid }
TunnelRefused(TunnelRefusedFields),  // { stream_id: Uuid, error: String }
TunnelData(TunnelDataFields),
TunnelWindow(TunnelWindowFields),
TunnelClose(TunnelCloseFields),
ForwardStatus(ForwardStatusFields), // { port: u16, listening: bool, error: Option<String> }
```

Semantics:

- `TunnelOpen`: proxy checks the port against its allowlist (below), dials
  `TcpStream::connect(("127.0.0.1", port))`, replies `TunnelOpened` or
  `TunnelRefused`. The proxy never dials anything but loopback — hard-coded,
  not configurable.
- `TunnelData`: raw bytes, base64 inside the existing JSON text frames, at
  most **16 KiB decoded per frame**. The ~33% base64 overhead is accepted for
  v1; binary WS frames are listed under future work.
- Flow control: each direction starts with a 256 KiB window per stream. A
  sender must not exceed its window; the receiver grants more via
  `TunnelWindow` as it drains bytes to the underlying socket.
- **Writer capacity, not just credit.** The session WS writer is an unbounded
  mpsc on both sides today, so stream credit alone does not bound memory or
  protect session traffic. Tunnel senders are pull-based: read from the TCP
  socket / HTTP body only while holding *both* stream credit *and* tunnel
  writer budget (a bounded per-connection cap, 64 queued `TunnelData` frames
  across all streams), never eagerly. Streams with data ready are serviced
  round-robin. Non-tunnel session messages (agent output, permissions,
  heartbeats) bypass the tunnel budget entirely — tunnel traffic can starve
  itself, never the session.
- `TunnelClose`: either side; half-close is not modeled — close tears down the
  stream. On session WS disconnect, both sides drop all live streams; the
  browser sees 502s and retries land after the proxy reconnects.
- Limits: at most 64 concurrent streams per session (a browser opens ~6 per
  origin; SSE and WS connections are long-lived, so leave headroom). Plain
  streams idle out (no bytes either direction) after 300 s; **upgraded
  (WebSocket) streams are exempt** from the idle timeout — a quiet Jupyter
  kernel socket must survive, and the stream cap plus session lifetime bound
  the resource. Quiet SSE streams whose server sends no keepalive comments
  will be dropped at the idle timeout; `EventSource` auto-reconnects, which is
  acceptable.

`ForwardOpen`/`ForwardClose` keep the proxy's allowed-port set in sync as
defense-in-depth; the authoritative allowlist is the backend DB (below). After
a proxy reconnect the backend replays `ForwardOpen` for every active forward
right after `RegisterAck`. The proxy answers every `ForwardOpen` with
`ForwardStatus`: it records the port in its allowlist, then probe-dials
`127.0.0.1:{port}` and reports `listening` (plus an `error` string on refusal)
so the registration API and CLI can warn when nothing is serving yet. A
missing `ForwardStatus` after a short wait means the proxy predates this
protocol — the API reports that cleanly ("proxy too old for forwarding")
rather than leaving the CLI hanging.

## Registration: DB, API, CLI

### Database

```sql
CREATE TABLE session_forwards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (session_id, port)
);
```

Rows are deleted with the session (cascade + the session reaper). Forward
lifetime is strictly session lifetime — nothing outlives the session.

### Agent-facing API (bearer token, same rails as `/api/agent/*`)

| Route | Effect |
|---|---|
| `POST /api/agent/sessions/{id}/forwards` `{ port }` | Insert row (idempotent on conflict), push `ForwardOpen` to the proxy, return `{ url }`. |
| `GET /api/agent/sessions/{id}/forwards` | List `{ port, url, created_at }`. |
| `DELETE /api/agent/sessions/{id}/forwards/{port}` | Delete row, push `ForwardClose`, drop live streams for that port. |

Request/response bodies are typed structs in `shared/src/api.rs` (house rule:
no `serde_json::json!`).

### Browser-facing API (cookie auth)

| Route | Access | Effect |
|---|---|---|
| `GET /api/sessions/{id}/forwards` | session read access | List forwards for UI. |
| `DELETE /api/sessions/{id}/forwards/{port}` | owner | Revoke. |
| `GET /api/sessions/{id}/forwards/{port}/open?next=/…` | session read access | Mint handoff token, `302` to the forward origin (see Auth). |

Access mirrors `files::pull_session_file`: read access (owner or
`session_members`) is sufficient to *use* a forward; only the owner can revoke.

### CLI (`launcher/src/forward.rs`, mirroring `launcher/src/message.rs`)

```console
agent-portal forward <port>       # register (idempotent), print the URL
agent-portal forward list         # active forwards for this session
agent-portal forward close <port> # revoke
```

- Auth: the launcher's stored token from `launcher.json`, exactly like
  `message`.
- Session identity: `sender_session_id()` (already handles
  `CLAUDE_CODE_SESSION_ID`, `PORTAL_SESSION_ID`, and the Codex thread-id
  reverse map). If no session identity is available, error with "run this from
  inside an agent session".
- On success prints exactly one URL line so agents can trivially relay it.
  A warning line is added when the proxy's `ForwardStatus` reply reports
  nothing listening on the port, or when no `ForwardStatus` arrives (proxy
  too old / not connected).

## Auth: token handoff to the forward origin

The forward origin cannot see the portal cookie. Flow:

1. Browser (on the portal origin) hits
   `GET /api/sessions/{id}/forwards/{port}/open?next=/some/path` —
   authenticated by the normal portal cookie.
2. Backend mints a JWT with the existing `jwt_secret`:
   claims `{ aud: "forward", session_id, port, exp: now + 60 s }`. Binding the
   port costs nothing (the redirect targets exactly one origin, and the
   host-only cookie is per-origin anyway) and prevents cross-forward reuse of
   a leaked token URL.
3. `302` → `{scheme}://{port}--{session}.{domain}/__portal/auth?token=…&next=/some/path`.
   `next` is validated before use — it must be an origin-relative path:
   starts with a single `/` (reject `//` and `/\`), no scheme/authority, no
   control characters, ≤ 2048 bytes; anything else is replaced with `/`.
   Without this, `/__portal/auth?next=…` is an open redirect on the forward
   origin. The same validation applies where the forward origin bounces an
   unauthenticated navigation back to the portal `open` endpoint.
4. The forward-origin router validates the token (including that the token's
   `port` matches the origin's), sets a `portal_fwd` cookie (HttpOnly,
   `SameSite=Lax`, `Secure` when the scheme is https, host-only — so it is
   scoped to that one forward origin) containing a `{ session_id, port }` JWT
   with a longer TTL (8 h), then `302` → `next`.
5. Every subsequent request on that origin — including WebSocket upgrades —
   authenticates via the `portal_fwd` cookie.

Cookie-missing/expired behavior on the forward origin: navigations
(`Sec-Fetch-Mode: navigate` / `Accept: text/html`) are redirected back to the
portal's `open` endpoint with `next` set, so a user who bookmarks a forward URL
bounces through transparently while logged in. Non-navigation requests (XHR,
WS) get a plain `401` — redirecting API calls causes confusing failures.

## The forward-origin router

An Axum middleware (or `Router::fallback`-level dispatch) inspects `Host`
before the normal router. The authority is normalized first — lowercase,
strip any `:port` suffix (dev requests arrive as
`8080--….localhost:3000`), strip a trailing dot — and only then is the first
DNS label matched against the strict regex:

- Host matches `{port}--{session}.{forward domain}` →
  - `/__portal/auth` → the handoff handler above.
  - everything else → require `portal_fwd` cookie, verify the `session_forwards`
    row still exists (revocation check, cheap and cacheable for a few
    seconds), then reverse-proxy through the tunnel.
- Otherwise → the existing router, untouched.

The reverse proxy is a hyper client whose connector opens a tunnel stream
(`TunnelOpen{port}` → `TunnelOpened`) and exposes it as `AsyncRead + AsyncWrite`.
Because hyper speaks real HTTP/1.1 over that virtual connection, streamed
bodies, chunked encoding, SSE, and `Connection: Upgrade` all work without
special cases; WebSocket upgrades use hyper's upgrade mechanism on both the
browser side and the tunnel side with a bidirectional byte copy.

Request rewriting, in the backend before bytes hit the tunnel:

- Strip the `portal_fwd` cookie from the `Cookie` header; pass other cookies
  through (they belong to the forwarded app, scoped to its origin).
- Never forward `Authorization` from the browser.
- Strip hop-by-hop headers (RFC 9110 §7.6.1); hyper manages
  `Transfer-Encoding`/`Connection` itself.
- Rewrite `Host` to `localhost:{port}` — this defuses dev-server host checks
  (Vite's `allowedHosts`, Django's `ALLOWED_HOSTS`).
- Set `X-Forwarded-For`, `X-Forwarded-Proto`, `X-Forwarded-Host`.

Response rewriting is limited to headers:

- Strip hop-by-hop headers.
- Rewrite `Location` (and `Content-Location`) values whose authority is
  `localhost:{port}`, `127.0.0.1:{port}`, or `[::1]:{port}` back to the
  forward origin — apps that saw `Host: localhost:{port}` emit absolute
  redirects there, which would otherwise send the browser off-origin.
- `Set-Cookie` passes through untouched; a `Domain=localhost` attribute is
  the app's own (mis)behavior and scoping it is not our job.

No HTML/URL body rewriting, ever — origin isolation makes it unnecessary.

## Security model

- The proxy dials loopback only; the port must be in its `ForwardOpen`-synced
  allowlist. The backend, authoritatively, only tunnels ports with a live
  `session_forwards` row. Browser users cannot probe undeclared ports.
- Forwarded apps run on their own origins: they cannot read the portal's DOM,
  cookies, or storage, and cannot reach each other.
- The portal auth cookie is host-only and never in scope for forward origins;
  the `portal_fwd` cookie is stripped before proxying, so the forwarded app
  never sees portal credentials of either kind.
- Handoff tokens live 60 s and are useless outside the forward-auth endpoint
  (`aud: "forward"`). Forward cookies expire after 8 h and re-bootstrap
  silently.
- Revocation (CLI `forward close`, UI, or session end) deletes the row, drops
  live tunnel streams, and invalidates future requests at the row check —
  outstanding cookies become harmless.
- Everything a forwarded app serves is whatever the *agent* chose to run;
  forwards are only reachable by users who already have read access to that
  session's transcript, which is the same trust boundary.

## Frontend

- Session view renders a chip per active forward (`:8080 ↗`), sourced from
  `GET /api/sessions/{id}/forwards`; click opens the `open` endpoint in a new
  tab; owners get a revoke `×`. A `ServerToClient::ForwardsChanged { session_id }`
  event triggers a refetch so chips appear the moment the agent registers.
- Bare forward URLs printed by the CLI already auto-link in transcripts; no
  new markdown scheme is needed (deliberate non-goal — `portal://port/…` would
  duplicate what the printed URL does).

## Agent hint (`claude-session-lib/portal_reminder.md`)

Ship in the same PR as the CLI — an unadvertised affordance is invisible to
agents:

```md
- **Port forwarding**: if you stand up an HTTP service the user should see
  (dev server, Jupyter, a web UI you built), run `agent-portal forward <port>`
  and share the printed URL — the user can open it in their browser while this
  session is alive. WebSockets and SSE work. Run `agent-portal forward close
  <port>` when the service is gone.
```

## Milestones

Protocol and schema changes land in milestone 1 in final form; later
milestones only add behavior. Each PR bumps the workspace version (the
protocol addition in M1 is a minor bump).

1. **Plumbing + plain HTTP.** Migration, tunnel protocol, proxy dial/copy with
   windowing, Host-routed reverse proxy, handoff auth, agent API + CLI,
   reminder update. Exit criteria: `python3 -m http.server 8080` browses
   correctly through `http://8080--{sid}.localhost:3000` in dev mode.
2. **Upgrades + streaming hardening.** WebSocket upgrade passthrough, SSE
   verification, idle timeouts, stream caps, reconnect replay of
   `ForwardOpen`. Exit criteria: Vite dev server with working HMR, and Jupyter
   with a live kernel, both through a forward.
3. **UI + polish.** Forward chips, revoke, `ForwardsChanged` push, probe-dial
   warning in the CLI, docs.

## Testing

- Unit: host-authority normalization + label parsing (valid/hostile), header
  filtering, `Location` rewrite, `next` validation (rejects `//`, absolute
  URLs, control chars), window accounting (sender blocks at 0, resumes on
  grant), token claims/expiry/port binding.
- Integration (backend + in-process fake proxy): end-to-end GET, streamed
  body, 502 on refused dial, revocation mid-stream, auth bounce redirect
  loop, WS upgrade echo.
- Manual checklist per milestone exit criteria above, in `--dev-mode` on
  `*.localhost`.

## Non-goals / future work

- **Raw TCP / non-HTTP protocols** — the tunnel could carry them, but naming,
  auth, and the browser story are all HTTP-shaped. Revisit if a concrete need
  appears.
- **Forwards that outlive the session**, and **public/unauthenticated share
  links** (ngrok-style). Both are auth-model expansions to design separately.
- **Path-prefix fallback mode** — superseded by subdomains; not worth
  maintaining two schemes.
- **Binary WebSocket frames** for tunnel data to drop the base64 overhead.
- **Port auto-discovery** (sniffing `LISTEN` sockets) — explicit declaration
  keeps the allowlist meaningful and the reminder teaches agents to declare.
