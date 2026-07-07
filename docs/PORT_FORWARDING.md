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

- **One forward per session, on a per-session subdomain**, not path prefixes.
  Each session gets its own origin: `{label}.{forward domain}`, where `label`
  is a short opaque hash of the session (not the port). Agent-built apps work
  unmodified (absolute asset paths, cookies, service workers), and forwarded
  apps are origin-isolated from the portal and from each other. A session
  forwards a single port at a time; an agent that needs several services
  fronts them behind its own reverse proxy on that one port. Because the
  subdomain identifies the *session*, re-pointing the forward to a different
  local port keeps the same URL.
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
{label}.{PORTAL_FORWARD_DOMAIN}
```

- `label`: an 8-lowercase-hex subdomain (32 bits) allocated per session in the
  `forward_subdomains` lookup table. It is derived from
  `sha256(session_id)[..8]`, re-derived with an incrementing counter on the
  (rare) collision, and stored so the same session always resolves to the same
  label — stable across close/reopen and independent of which port is
  forwarded. It does not encode the port or leak the raw session UUID.
- Parsing is strict: `^[0-9a-f]{8}$` on the first DNS label (after
  lowercasing / stripping any `:port` and trailing dot), and the remaining
  labels must equal the configured forward domain exactly. Anything else falls
  through to the normal router. The label is then resolved to a session via
  the LUT; an unknown label is a 404.
- Collisions are handled by the LUT + counter, so 32 bits stays unambiguous;
  the table's `UNIQUE(label)` is the backstop.

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
-- The session's single forwarded port.
CREATE TABLE session_forwards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (session_id)              -- at most one forward per session
);

-- Subdomain label ↔ session lookup. A short 8-hex label is allocated on first
-- forward (sha256(session_id)[..8], counter-bumped on collision) and kept
-- across close/reopen so the URL stays stable.
CREATE TABLE forward_subdomains (
    label TEXT PRIMARY KEY,
    session_id UUID NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Both tables cascade-delete with the session. A `session_forwards` row is
deleted when the forward is revoked (its `forward_subdomains` label persists so
a re-forward reuses the same URL); everything dies with the session.

### Agent-facing API (bearer token or session cookie, same rails as `/api/agent/*`)

| Route | Effect |
|---|---|
| `POST /api/agent/sessions/{id}/forwards` `{ port }` | Upsert the session's single forward to `port`, push `ForwardClose(old)` + `ForwardOpen(new)` on a port change, return `{ forward, replaced_port, listening }`. |
| `GET /api/agent/sessions/{id}/forwards` | The session's forward (0 or 1). |
| `DELETE /api/agent/sessions/{id}/forwards` | Delete the forward row, push `ForwardClose`, drop live streams. |

Request/response bodies are typed structs in `shared/src/api/forwards.rs`
(house rule: no `serde_json::json!`). `replaced_port` is set when a `POST`
moved an existing forward off its previous port, so the CLI can tell the
agent.

### Browser-facing API (cookie auth)

| Route | Access | Effect |
|---|---|---|
| `GET /api/sessions/{id}/forwards` | session read access | The session's forward, for the UI. |
| `POST /api/sessions/{id}/forwards` | owner | Set/replace (same handler as the agent route). |
| `DELETE /api/sessions/{id}/forwards` | owner | Revoke. |
| `GET /api/sessions/{id}/forwards/open?next=/…` | session read access | Mint handoff token, `302` to the forward origin (see Auth). |

Access: read access (owner or `session_members`) is sufficient to *use* the
forward (open it in the browser) and to see it; **setting or revoking the
forward is owner-only** on both route sets — it exposes/tears down a loopback
port on the proxy host, a strictly tighter capability than reading the
transcript. The CLI clears the owner gate because the launcher's bearer token
resolves to the session owner.

### CLI (`launcher/src/forward.rs`, mirroring `launcher/src/message.rs`)

```console
agent-portal forward <port>   # set the forward (replacing any current one), print the URL
agent-portal forward list     # show the current forward
agent-portal forward close    # revoke it
```

- Auth: the launcher's stored token from `launcher.json`, exactly like
  `message`.
- Session identity: `sender_session_id()` (already handles
  `CLAUDE_CODE_SESSION_ID`, `PORTAL_SESSION_ID`, and the Codex thread-id
  reverse map). If no session identity is available, error with "run this from
  inside an agent session".
- On success prints exactly one URL line so agents can trivially relay it.
  A note line is added when the call replaced an existing forward on a
  different port (`replaced_port`), and a warning line when the proxy's
  `ForwardStatus` reply reports nothing listening on the port, or when no
  `ForwardStatus` arrives (proxy too old / not connected).

## Auth: token handoff to the forward origin

The forward origin cannot see the portal cookie. Flow:

1. Browser (on the portal origin) hits
   `GET /api/sessions/{id}/forwards/open?next=/some/path` — authenticated by
   the normal portal cookie.
2. Backend mints a JWT with the existing `jwt_secret`:
   claims `{ aud: "forward", session_id, exp: now + 60 s }`. The forward port
   isn't in the token — a session has at most one, looked up per request so
   revocation and re-pointing take effect immediately.
3. `302` → `{scheme}://{label}.{domain}/__portal/auth?token=…&next=/some/path`.
   `next` is validated before use — it must be an origin-relative path:
   starts with a single `/` (reject `//` and `/\`), no scheme/authority, no
   control characters, ≤ 2048 bytes; anything else is replaced with `/`.
   Without this, `/__portal/auth?next=…` is an open redirect on the forward
   origin. The same validation applies where the forward origin bounces an
   unauthenticated navigation back to the portal `open` endpoint.
4. The forward-origin router validates the token (`aud` + `exp` required, and
   the token's `session_id` must match the label's session), sets a
   `portal_fwd` cookie (HttpOnly, `SameSite=Lax`, `Secure` when the scheme is
   https, host-only — so it is scoped to that one forward origin) containing a
   `{ session_id }` JWT with a longer TTL (8 h), then `302` → `next`.
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
strip any `:port` suffix (dev requests arrive as `a3f9c2e1.localhost:3000`),
strip a trailing dot — and only then is the first DNS label matched against
the strict `^[0-9a-f]{8}$`:

- Host matches `{label}.{forward domain}` → resolve the label to a session via
  the LUT (unknown label → 404), then:
  - `/__portal/auth` → the handoff handler above.
  - everything else → require `portal_fwd` cookie, look up the session's
    currently-forwarded port (missing → 404, which doubles as the revocation
    check), then reverse-proxy through the tunnel to that port.
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
  allowlist. The backend, authoritatively, only tunnels the port in the
  session's live `session_forwards` row. Browser users cannot probe undeclared
  ports.
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
  by default forwards are only reachable by users who already have read access
  to that session's transcript, which is the same trust boundary.
- **Public forwards** are an owner opt-in (`session_forwards.public`, toggled
  from Settings ▸ Forwarding). A public forward skips the token handoff and
  serves the subdomain to anyone with the URL. The dispatch checks `public`
  *before* the cookie gate, but for a private-or-absent forward it still runs
  the cookie gate before distinguishing present-from-absent, so an
  unauthenticated caller can't probe whether a private forward is active — it
  always gets the same auth bounce. Only the session owner can flip the flag
  (`PATCH /api/sessions/{id}/forwards/public`).

## Frontend

- Session view renders a chip for the session's forward (`:8080 ↗`), sourced
  from `GET /api/sessions/{id}/forwards`; click opens the `open` endpoint in a
  new tab; owners get a revoke `×`. A
  `ServerToClient::ForwardsChanged { session_id }` event triggers a refetch so
  the chip appears/updates the moment the agent registers or re-points.
- **Settings ▸ Forwarding** lists the caller's active forwards across their
  sessions (`GET /api/forwards`, owner-scoped) with a per-forward public/private
  toggle (`PATCH …/forwards/public`), so the owner can opt a forward into
  unauthenticated public access.
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
  session is alive. WebSockets and SSE work. A session forwards one port at a
  time; run `forward <port>` again to move it (front multiple services behind
  your own reverse proxy). Run `agent-portal forward close` when done.
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
