# Mobile Apps Plan — iOS & Android access to Agent Portal

Status: **proposal / planning document** (no code changes implied by merging this).
Author: planning session 2026-07-11. Reviewed by: _pending_.

---

## 1. Executive summary

The cheapest credible path to "Agent Portal on my phone, in my pocket, with
notifications" is **not** a from-scratch native app. The existing stack is
unusually well positioned for mobile:

- The Yew/WASM frontend is already responsive and runs on mobile browsers today.
- The typed WS protocol lives in `shared/` (WASM-clean, compiles for any target)
  and `ws-bridge` already has **three faces**: axum server, browser client, and
  a rustls-based native client (`ws-bridge::native_client`) used by the proxy,
  launcher, and test harness.
- The connection-lifecycle work (#1256/#1236/#939) already solved the hard
  mobile problems: reconnect with replay watermarks (`replay_after`), input
  idempotency (`client_msg_id` + outbox resend), bounded queues, liveness
  sweeps. Mobile backgrounding is just an aggressive flavor of the flaky-proxy
  problem we already engineered for.
- Device-flow auth already mints JWTs for out-of-browser clients.

What is **genuinely missing** for mobile is exactly three things:

1. **Push notifications** (turn complete, permission request) — net-new backend
   epic, and the single highest-value mobile feature.
2. **Token auth for `/ws/client`** — it is cookie-only today
   (`handle_web_client_websocket` → `extract_user_id(&cookies)`); an app shell
   or PWA-adjacent client needs a bearer path. Device flow gets us 90% there.
3. **Store presence / app shell** — installability, deep links, share sheet,
   native push registration.

**Recommendation (staged):**

- **M0 — PWA hardening** (days): manifest + service worker + icons. Installable
  on Android and iOS immediately; zero new distribution process.
- **M1 — Mobile token auth** (small): accept portal JWTs on `/ws/client` and
  the REST surface; same-device device-flow login via system browser.
- **M2 — Push** (the real work): `push_subscriptions` table, Web Push (VAPID)
  first — it serves the installed PWA on **both** platforms today — with
  APNs/FCM native push as a follow-on inside the app shell.
- **M3 — App shell via Tauri 2 mobile** (Rust-coherent wrapper): remote-URL
  WebView shell + native push + deep links + share target.
- **M4 — Store packaging & CI lanes** (TestFlight / Play internal → prod).
- **M5 (contingent) — native escalation** (Dioxus or UniFFI core + SwiftUI/
  Compose) only if WebView quality or App Store review forces it.

Total to a store-published v1 (M0–M4): roughly **6–9 engineer-weeks** at our
PR cadence, of which push (M2) is ~40%.

---

## 2. Goals and non-goals

### Goals

- Read + drive sessions from a phone: view streaming output, send input,
  answer permission requests, launch sessions on connected launchers.
- **Be interruptible**: a permission prompt or finished turn reaches the user's
  lock screen within seconds.
- Reuse the existing frontend and protocol; keep one source of truth for UI.
- Fit the existing tooling: Rust workspace, GH Actions, squash-merge train,
  commit-count versioning, Codex cross-review.

### Non-goals (v1)

- Offline authoring / queueing input while fully offline (the outbox makes
  brief drops safe; multi-hour offline queues are out of scope).
- Feature parity for admin surfaces (usable via responsive web view is enough).
- Tablet-optimized layouts (they inherit the responsive web layout).
- Running agents **on** the phone. The phone is a remote control, never a host.

---

## 3. Inventory: what we already have (and what it buys us)

| Asset | Where | Mobile relevance |
|---|---|---|
| Typed WS protocol | `shared/src/endpoints.rs` | Compiles to any target incl. `aarch64-apple-ios` / `aarch64-linux-android`; a native core reuses it verbatim |
| `ws-bridge` native client | `ws-bridge::native_client` (tokio-tungstenite + **rustls-webpki-roots**) | No OpenSSL cross-compile pain on mobile; same client the proxy/launcher/harness use |
| Reconnect + replay | `RegisterFields.replay_after`, reconnect backoff, replay watermarks | Foreground-after-background = reconnect + catch-up, already server-supported |
| Input idempotency | `client_msg_id` + outbox resend-all (#1236) | Send-then-suspend on mobile can't double-deliver |
| Device flow auth | `/api/auth/device/{code,poll,verify,approve,deny}` → JWT | The out-of-browser login primitive already exists and is rate-limited (#1047) |
| Token rotation precedent | Launcher `TokenRefresh`/`TokenRefreshAck` (#1237) | The pattern for long-lived mobile tokens is already designed |
| Transactional uploads | temp + rename, `FileUploadResult` (#939) | Camera/share-sheet image sends ride the existing upload path |
| Responsive frontend | Yew app; portal officially supports mobile browsers | M0 is hardening, not a rewrite |
| Voice input | `frontend/src/components/voice_input.rs` (Web Speech API) | Works in Safari/Chrome; **not** in WKWebView (see §8.4) |
| Frontend embedded in backend | build-time asset embedding | PWA assets deploy with a normal backend release; no new hosting |
| Commit-count versioning | `shared::VERSION` (major.minor.commit-count) | Monotonic integer — drops straight into Android `versionCode` and pairs with `CFBundleVersion` |

---

## 4. The mobile-specific problem set

Every option below is scored against these five problems; they are the
whole difference between "the website on a phone" and "an app".

- **P1 Push**: permission requests and turn completions must reach a pocketed
  phone. Requires server-side push infra + a client registration surface.
- **P2 Auth without cookies**: WS + REST need a bearer-token path; login must
  work without a same-origin browser session.
- **P3 Lifecycle**: OS suspends the app ⇒ WS dies silently. Need fast
  foreground reconnect + replay, and push to cover the gap while suspended.
- **P4 Input affordances**: camera, share sheet ("share this file/screenshot
  to a session"), paste-image, dictation.
- **P5 Distribution**: App Store / Play Store presence, signing, review risk,
  update cadence.

---

## 5. Options analysis

### Option A — PWA (harden what exists)

Add `manifest.webmanifest`, icons, a service worker (network-first for the
app shell, cache-first for static assets), iOS meta tags. Trunk supports
copying extra assets; the backend already serves everything same-origin.

| Problem | How A handles it |
|---|---|
| P1 Push | **Web Push (VAPID)** — full support on Android (Chrome); iOS ≥16.4 **only when installed to home screen**, no install-prompt API, Apple may throttle background delivery |
| P2 Auth | Unchanged (cookies work — it *is* the browser) |
| P3 Lifecycle | Browser tab lifecycle; reconnect logic already exists in `use_client_websocket` |
| P4 Input | Web share target (Android only), `<input capture>` camera, keyboard dictation |
| P5 Distribution | None (that's the point) — or PWABuilder wrapping later |

- **Cost**: ~1 engineer-week including SW cache-invalidation care.
- **Ceiling**: iOS push UX is second-class (manual install ritual, no badge
  control, delivery throttling anecdotes), no share-sheet on iOS, no store
  presence.
- **Verdict**: do it regardless — it is the substrate for M2's Web Push and
  costs almost nothing. Not sufficient alone for the iOS experience we want.

### Option B — WebView app shell (Tauri 2 mobile vs Capacitor)

A native shell around the existing web UI. Two sub-decisions: **which shell
framework** and **remote vs bundled frontend** (the crux — see §5.B.2).

#### B.1 Shell framework

| | **Tauri 2 mobile** | **Capacitor** |
|---|---|---|
| Language/toolchain | Rust CLI, Rust plugin host — fits repo | Node/npm + TS config — new toolchain in a pure-Rust repo |
| Maturity (mobile) | Stable since 2.0 (Oct 2024); mobile is newer than desktop | Very mature, huge plugin ecosystem |
| Push plugin | Community/early (`tauri-plugin-push-notifications`); may need our own thin Swift/Kotlin bridge (~200 lines/platform) | First-party, battle-tested |
| Deep links, biometrics | First-party plugins | First-party plugins |
| Team fit | High — Rust across the board; our shell logic (token store, push registration relay) can be Rust | Low — introduces JS build + dependency surface we deliberately don't have |
| Risk | Younger mobile story; we self-insure on push bridge | npm supply-chain + toolchain drift |

**Choice: Tauri 2.** Rationale: repo coherence beats plugin convenience; the
one weak spot (push bridge) is small, well-bounded native code we'd want to
control anyway. **Reversible**: the web app doesn't know or care what wraps
it; swapping shells later doesn't touch the portal.

#### B.2 Remote-URL vs bundled frontend (the crux tradeoff)

- **Remote**: shell loads `https://portal.example.com` directly.
  - ✅ Zero frontend duplication; server-driven updates (ship a fix by
    deploying the backend, no store review); **same-origin assumptions all
    hold** — cookies, relative API/WS URLs, embedded assets.
  - ⚠️ Needs network for first paint (splash + cached shell mitigates);
    slightly higher App Store 4.2 "just a website" risk (mitigated by push,
    deep links, share extension — see §9).
- **Bundled**: frontend assets packed into the app, calling the backend
  cross-origin.
  - ✅ Instant first paint, review-proof asset story.
  - ❌ Breaks the same-origin assumption everywhere: the frontend derives API
    and WS URLs from `window.location`, auth is a same-origin cookie, assets
    are embedded in the backend. Bundling forces configurable base URLs,
    CORS on the whole API, token auth for **every** REST call, and a
    frontend-version-vs-backend-version skew matrix. That is a large,
    ongoing tax — not a one-time cost.

**Choice: remote-URL shell.** The bundled tax buys nothing our users need in
v1. Revisit only if review or first-paint metrics force it. (M1's token auth
is still required — for the *shell's* login handoff and for push registration
REST calls made outside the WebView cookie jar.)

| Problem | How B (Tauri, remote) handles it |
|---|---|
| P1 Push | Native APNs/FCM registration in the shell; token POSTed to backend; backend pushes natively. First-class lock-screen UX, badges, sounds |
| P2 Auth | Device-flow variant in system browser → JWT held by shell → injected into WebView session (§7) |
| P3 Lifecycle | Shell observes foreground events → nudges WebView reconnect immediately instead of waiting for timer |
| P4 Input | Share extension (iOS) / share target (Android) → upload path; camera via WebView permission; keyboard dictation |
| P5 Distribution | Full store presence; review risk analyzed in §9 |

- **Cost**: ~3–4 engineer-weeks including push bridges and store setup.
- **Verdict**: the recommended v1 app.

### Option C — Rust-native UI (Dioxus mobile / Slint)

Rewrite the client UI in a Rust framework with mobile renderers, reusing
`shared/` + `ws-bridge` natively.

- ✅ Maximum type-sharing; one language.
- ❌ **Second UI to maintain forever** — the dashboard is large (message
  renderers for two agent protocols, permission dialogs, uploads, voice,
  admin). Dioxus mobile is pre-1.0 in exactly the areas we'd stress
  (keyboards, IME, scroll performance, a11y).
- **Verdict: rejected for v1.** Escalation path only, and Option D is the
  better-understood escalation.

### Option D — Native shells + shared Rust core (UniFFI)

SwiftUI + Compose apps over a `mobile-core` Rust crate (session list, WS
client on `ws-bridge::native_client`, outbox, auth) exposed via UniFFI.

- ✅ Best possible UX ceiling; protocol logic stays typed Rust (no
  Swift/Kotlin protocol drift — honoring the "no JSON poking" rule).
- ❌ Two UIs + a bindings layer; roughly 3–4× Option B's cost; slowest
  iteration (every protocol addition = core + two UIs).
- **Verdict**: the *right* long-term architecture **if** the portal becomes a
  primary phone-first product. Not v1. The M1/M2 backend work is 100% shared
  with this future, so nothing in the recommended path is throwaway.

### Option E — Fully native, no shared core

- ❌ Reimplements the WS protocol twice in non-Rust; guaranteed drift; violates
  the repo's typed-interface ethos. **Rejected without further analysis.**

### Summary matrix

| | A PWA | B Tauri shell | C Dioxus | D UniFFI native | E Full native |
|---|---|---|---|---|---|
| Push quality (iOS) | ⚠️ installed-PWA only | ✅ APNs | ✅ APNs | ✅ APNs | ✅ APNs |
| UI reuse | ✅ 100% | ✅ 100% | ❌ rewrite | ❌ rewrite ×2 | ❌ rewrite ×2 |
| Protocol reuse | ✅ | ✅ | ✅ | ✅ (UniFFI) | ❌ |
| Store presence | ❌ | ✅ | ✅ | ✅ | ✅ |
| New toolchains | none | Xcode/AS + Tauri | Xcode/AS + Dioxus | Xcode/AS + UniFFI + Swift/Kotlin | Xcode/AS + Swift/Kotlin |
| Cost to v1 | ~1 wk | ~3–4 wk | ~8–10 wk | ~10–14 wk | ~14+ wk |
| Throwaway work if we later go D | none | shell only (~1 wk) | most of it | — | — |

---

## 6. Recommended architecture (M0–M4)

```
┌─ iPhone / Android ────────────────────────────────┐
│  Tauri 2 shell (Rust)                             │
│  ├─ WKWebView / Android WebView ──────────────────┼── https://portal (same-origin: cookies, WS, assets)
│  ├─ Push bridge (Swift/Kotlin, ~200 lines each) ──┼── APNs / FCM registration token
│  ├─ Deep links: https://portal/app/session/{id}   │
│  ├─ Share extension → POST upload → open session  │
│  └─ Auth handoff: system browser device-flow → JWT│
└───────────────────────────────────────────────────┘
                        │
┌─ backend ─────────────▼───────────────────────────┐
│  NEW  /api/push/subscriptions (register/list/del) │
│  NEW  push dispatcher (Web Push now, APNs/FCM M3) │
│  NEW  bearer-JWT auth on /ws/client + REST        │
│  hooks: permission request, turn Result, session  │
│         error → dispatch if user has no live      │
│         web client (presence via SessionManager)  │
└───────────────────────────────────────────────────┘
```

---

## 7. Auth design (M1)

**Today**: `/ws/client` extracts the user from the signed session cookie and
rejects otherwise; every REST handler uses `CurrentUserId` (cookie). Device
flow (`/api/auth/device/*`) already mints JWTs after browser approval, with
rate limiting.

**Plan**:

1. **Accept `Authorization: Bearer <jwt>`** as an alternative to the cookie in
   `extract_user` (one function; every handler and the WS upgrade inherit it).
   Token audience `mobile` distinct from proxy/launcher tokens so revocation
   can be per-class.
2. **Same-device login without code-typing**: app opens
   `https://portal/device?user_code=XXXX` (the existing verify page,
   pre-filled) in `ASWebAuthenticationSession` / Custom Tabs; the user does the
   normal Google login + approve; the app's poll on `/api/auth/device/poll`
   completes with a JWT. Zero new Google Console configuration, reuses the
   #1047-hardened endpoints. (The CLI types a code because it can't open a
   browser on the same screen — a phone can.)
3. **Rotation**: 30-day expiry with half-life refresh, mirroring launcher token
   rotation (#1237): refresh over the live WS (`TokenRefresh` variant on the
   client endpoint) or a `POST /api/auth/refresh`. Store in Keychain /
   Keystore via the shell.
4. **WebView session handoff**: the shell holds the JWT; the WebView still uses
   the cookie it gets from the device-flow completion redirect. Fallback: a
   `POST /api/auth/token-login` that exchanges a bearer JWT for a session
   cookie inside the WebView (small handler, same audience checks).

Backend delta: ~2 focused PRs (bearer path + refresh; token-login exchange).

## 8. Push architecture (M2 — the real epic)

### 8.1 Events worth a push (v1)

| Event | Source hook | Priority |
|---|---|---|
| Permission request pending | permission dispatch to web clients | **highest** — this is the "agent is blocked on you" interrupt |
| Turn complete (Result) | proxy `SequencedOutput` Result finalize | high; collapse per session |
| Session errored/disconnected unexpectedly | status transition | medium |
| Inter-agent message received | message delivery | low, off by default |

### 8.2 Delivery policy

- **Suppress when present**: if the user has a live web client
  (`SessionManager` user-client registry — the presence source already exists),
  deliver in-app only. Push is for the pocketed phone.
- **Collapse keys**: one visible notification per session, newest wins
  (`apns-collapse-id` / FCM `collapse_key` / Web Push `tag`).
- **Payload discipline**: session id + event kind + short preview only. The tap
  deep-links to the session; content stays server-side.
- Per-user, per-event-kind toggles in Settings (new `notification_prefs` on
  users or a small table).

### 8.3 Transport strategy

- **Stage 1 — Web Push (VAPID)** via the `web-push` crate:
  one integration that serves installed PWAs on Android **and** iOS ≥16.4 and
  desktop browsers. New table:

  ```sql
  push_subscriptions(id, user_id, platform,        -- 'webpush' | 'apns' | 'fcm'
                     endpoint_or_token, p256dh, auth,  -- webpush keys; NULL for native
                     device_label, created_at, last_success_at, disabled_at)
  ```

  Dead-endpoint pruning on 404/410 responses.
- **Stage 2 (M3) — native APNs + FCM** for the shell:
  direct APNs via the `a2` crate (HTTP/2, p8 key) and FCM v1 via HTTP + service
  account. **Decision: direct integrations, not FCM-for-iOS routing** — one
  fewer intermediary, no Firebase SDK inside the app, and `a2` keeps it Rust.
  The dispatcher fans out one event to every non-disabled subscription for the
  user, per policy above.
- Failure marker: `PUSH_DISPATCH_FAILED` (mirrors `SESSION_ARCHIVE_FAILED` /
  `PENDING_INPUT_PERSIST_FAILED` conventions for alerting).

### 8.4 Known capability gaps to accept in v1

- **Web Speech in WKWebView**: `SpeechRecognition` is unavailable inside
  WKWebView — the mic button should hide when unsupported (it already gates on
  API presence); keyboard dictation covers the need. Native speech plugin is a
  v2 nicety.
- **iOS PWA installs** can't register through the shell path; they use Web Push
  and live with its limits.

---

## 9. App Store / Play review risk

- **Precedent**: GitHub, Slack, Termius, Working Copy — "client for a developer
  service" is an accepted category. The agents run server-side; the app
  executes nothing (no 2.5.2 exposure).
- **Apple 4.2 (minimum functionality / repackaged website)** is the real risk
  for a remote-URL WebView app. Mitigations that historically satisfy review:
  native push, deep links / universal links, share extension, native settings
  entry points, app-specific login handoff. All are in M3 scope. Residual risk:
  a strict reviewer → contingency is bundling the frontend (§5.B.2 tax) or
  adding a native session-list screen (first slice of Option D).
- **Sign-in-with-Google-only** apps on iOS trip guideline 4.8 (must offer a
  comparable privacy-preserving option — in practice, Sign in with Apple).
  **Open question O3**: add Apple OAuth to the backend (moderate: second
  provider in `auth.rs`, same allowlist gates) or gate store release on it.
- **Play**: low risk; data-safety form + privacy policy URL needed (we should
  publish one regardless).

## 10. CI/CD, signing, versioning

- **Android lane**: ubuntu runner — SDK + NDK, `tauri android build`, sign with
  a keystore in GH secrets, upload to Play internal track (fastlane supply or
  `gradle-play-publisher`).
- **iOS lane**: macOS runner — Xcode, certs/profiles via **fastlane match**
  (private certs repo) or App Store Connect API key in secrets,
  `tauri ios build`, upload via `altool`/fastlane pilot to TestFlight.
- **Versioning**: `shared::VERSION`'s commit-count patch is monotonic —
  `versionCode` = commit count, `CFBundleVersion` likewise;
  `CFBundleShortVersionString` / `versionName` = full `major.minor.patch`.
  No new human knobs, consistent with #1096.
- **Cadence**: store binaries only change when the *shell* changes (remote-URL
  frontend updates ship with backend deploys). Expect a shell release every
  few weeks, not per-merge. CI builds every PR touching `mobile/`; store upload
  on manual dispatch or tag.
- **Repo shape**: `mobile/` at workspace root (Tauri project; its Rust crate
  joins the workspace; Xcode/Gradle projects generated + committed).

## 11. Phased plan (PR-series shape, per repo process)

Each phase = a PR series, cross-reviewed by Codex, squash-merged serially.

**M0 — PWA baseline** (~1 wk): manifest + icons + iOS meta (1 PR); service
worker with cache-versioning tied to `shared::VERSION` (1 PR); install hint UI
(1 PR). *Exit*: installable on both platforms, Lighthouse PWA pass.

**M1 — Mobile token auth** (~1 wk): bearer-JWT alternative in `extract_user` +
audience + tests (1 PR); refresh + `token-login` cookie exchange (1 PR);
pre-filled `user_code` verify page polish (1 PR). *Exit*: WS + REST usable
with a device-flow JWT end to end.

**M2 — Push** (~2–2.5 wk): `push_subscriptions` migration + CRUD API (1 PR);
dispatcher + Web Push + event hooks + suppression policy (2 PRs); Settings
toggles + frontend subscribe flow (1 PR); prefs + pruning + `PUSH_DISPATCH_FAILED`
observability (1 PR). *Exit*: locked Android phone + installed PWA receives a
permission-request push that deep-links into the session.

**M3 — Tauri shell** (~2–3 wk): scaffold + remote-URL config + CI debug builds
(1 PR); auth handoff via system browser (1 PR); APNs bridge + `a2` dispatch
(1–2 PRs); FCM bridge + dispatch (1 PR); deep links + share extension → upload
(1–2 PRs). *Exit*: TestFlight/internal-track builds with native push.

**M4 — Store launch** (~1 wk + review latency): assets, privacy policy, data
safety, review notes; Apple sign-in decision (O3) resolved; staged rollout.

**M5 — contingent**: only on explicit demand signals (review rejection ⇒
bundle/native-first-screen; WebView jank ⇒ Option D spike starting with the
session list).

## 12. Risks

| Risk | L | I | Mitigation |
|---|---|---|---|
| Apple rejects WebView shell (4.2) | M | H | M3 native surface area; contingency: bundle or native first screen |
| Tauri mobile push plugin immature | H | M | Own thin bridges (~200 lines/platform), planned not discovered |
| iOS Web Push unreliability (M2 gap era) | M | M | Positioned as interim; APNs lands in M3 |
| SW caching serves stale WASM | M | M | Version-keyed cache from `shared::VERSION`; network-first for `index.html` |
| Google-only login blocks iOS launch (4.8) | M | H | Decide O3 early — it's backend work with lead time |
| WS battery drain in foreground | L | L | Existing heartbeats are modest; shell releases socket on background |
| Cert/signing ops on solo project | M | M | fastlane match + documented runbook in `mobile/README` |

## 13. Decision log

| # | Decision | Rationale | Reversibility |
|---|---|---|---|
| D1 | PWA first, regardless of shell | Substrate for Web Push; near-zero cost | n/a |
| D2 | Tauri 2 over Capacitor | Rust coherence; push bridge self-insured | High — shell is swappable |
| D3 | Remote-URL over bundled frontend | Preserves same-origin assumptions; avoids CORS/base-URL/version-skew tax | Medium — bundling is additive later |
| D4 | Device-flow (pre-filled code) over new OAuth PKCE | Reuses hardened endpoints; zero Google Console changes | High |
| D5 | Direct APNs (`a2`) + FCM v1, not FCM-for-everything | Fewer intermediaries; no Firebase SDK on iOS; Rust-native | Medium |
| D6 | Web Push before native push | One integration serves both platforms + desktop immediately | n/a (kept either way) |
| D7 | No Dioxus/UniFFI in v1 | Second UI is the most expensive artifact we could create | Path preserved: M1/M2 fully shared with Option D |

## 14. Open questions for Matt

- **O1**: iPhone or Android first for dogfooding? (Determines which push
  bridge lands first in M3.)
- **O2**: Store presence a hard requirement, or is installed-PWA + push (M0–M2)
  possibly sufficient? M2 is the natural checkpoint to reassess.
- **O3**: Add Sign in with Apple to the backend (Apple 4.8), or defer store
  launch until we're ready to do it?
- **O4**: Push preview content — session name only, or include a text snippet
  of the permission/turn? (Privacy vs. usefulness on the lock screen.)
- **O5**: Should launchers be drivable from the phone in v1 (launch new
  sessions), or is driving existing sessions enough?
