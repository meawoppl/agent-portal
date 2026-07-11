# Agent Portal — mobile shell (Tauri 2)

A thin native shell that wraps the **deployed** Agent Portal web app in a
platform WebView (WKWebView on iOS, Android System WebView, WebKitGTK on
desktop). This is work item **E1** (scaffold only) of
[`docs/MOBILE_APPS_PLAN.md`](../docs/MOBILE_APPS_PLAN.md) — Track E, M3.

## Why this shape (decision context)

Two decisions from the plan's decision log (§13) drive this crate:

- **D2 — Tauri 2 over Capacitor.** The repo is pure Rust; a Tauri shell keeps
  the shell logic (token store, push registration relay, lifecycle nudges) in
  Rust rather than introducing an npm/TypeScript toolchain. The shell is
  swappable, so this is low-risk.
- **D3 — Remote-URL over bundled frontend.** The shell loads the deployed
  portal URL directly instead of bundling the Yew/WASM frontend. This preserves
  every same-origin assumption the web app relies on (cookies, relative API/WS
  URLs, embedded assets) and lets us ship frontend fixes by deploying the
  backend — no app-store review. See §5.B.2 for the full tradeoff.

Because it's a remote-URL shell, the local `../src/index.html` is only a splash
placeholder (Tauri requires a `frontendDist`); the real UI is whatever the
portal URL serves.

## What this scaffold includes (E1)

- `src-tauri/` — the Rust crate (`portal-mobile`), Tauri config, capabilities,
  placeholder icons.
- Remote-URL window config (`src-tauri/tauri.conf.json` → `app.windows[0].url`).
- Build-time portal-URL override via `PORTAL_SHELL_URL` (see below).

**Not** in this PR (later items, all depend on E1):

- **E2** — deep links / universal links (+ backend `assetlinks.json` /
  `apple-app-site-association`).
- **E3** — auth handoff: system-browser device flow → JWT in Keychain/Keystore
  → cookie exchange into the WebView.
- **E4 / E5** — APNs (Swift) and FCM (Kotlin) push registration bridges.
- **E6** — share extension / share target → upload → open session.
- **F1 / F2** — Android and iOS CI lanes (SDK/NDK, signing, store upload).

## Configuring the portal URL

The window loads `https://localhost:3000` by default (a placeholder in
`tauri.conf.json`). Override it at **build time** with `PORTAL_SHELL_URL`:

```bash
PORTAL_SHELL_URL="https://portal.example.com" cargo tauri dev
```

`src-tauri/build.rs` bakes the value as a compile-time env var; `src/lib.rs`
navigates the main window to it on startup. When the variable is unset, the
config default is used verbatim (no override, no double-load).

## Running

> Prerequisite: the Tauri 2 CLI — `cargo install tauri-cli --locked` (provides
> `cargo tauri`).

### Desktop (Linux/macOS/Windows — quickest local check)

Desktop Tauri needs system WebView libraries. On Debian/Ubuntu:

```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev \
    libayatana-appindicator3-dev librsvg2-dev build-essential
```

Then, from `mobile/src-tauri/`:

```bash
PORTAL_SHELL_URL="https://portal.example.com" cargo tauri dev
```

### Android

```bash
# One-time: install Android Studio, SDK, and NDK; set ANDROID_HOME + NDK_HOME.
# Add the Rust Android targets:
rustup target add aarch64-linux-android armv7-linux-androideabi \
    i686-linux-android x86_64-linux-android

# From mobile/src-tauri/ — generates gen/android/ (git-ignored here; committed
# in F1):
cargo tauri android init
PORTAL_SHELL_URL="https://portal.example.com" cargo tauri android dev
```

### iOS (macOS only)

```bash
# One-time: Xcode + command line tools.
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios

# From mobile/src-tauri/ — generates gen/apple/ (git-ignored here; committed
# in F2):
cargo tauri ios init
PORTAL_SHELL_URL="https://portal.example.com" cargo tauri ios dev
```

## Workspace membership (important)

Per plan §10 the mobile crate is *intended* to join the root Cargo workspace.
It is currently **excluded** from the workspace (root `Cargo.toml` →
`[workspace] exclude`) rather than added to `members`, for one concrete reason:

> The main CI runs `cargo clippy --workspace` and `cargo test --workspace` on a
> plain `ubuntu-latest` runner with **no** `webkit2gtk` and no mobile SDK/NDK.
> A workspace member is compiled by `--workspace` regardless of
> `default-members`, so making `portal-mobile` a member would break existing
> CI and any `cargo build --workspace` on a machine without the WebView libs.
> `exclude` is the only setting that keeps those invocations byte-for-byte
> unaffected.

As a result this crate manages its **own** `target/` and lockfile and is built
via the Tauri CLI from `mobile/src-tauri/`, independent of the workspace.

**Promotion path:** once F1/F2 land dedicated mobile CI lanes (which install
the WebView/SDK toolchain), the crate can be moved from `exclude` into
`members` — ideally alongside a `default-members` list that keeps
`portal-mobile` out of the default `cargo build`/`cargo test` so day-to-day
workspace commands stay fast on machines without the mobile toolchain.

## Icons

`src-tauri/icons/` holds placeholder icons. Regenerate a full, correctly-sized
set (including `.icns` for macOS) from a source PNG with:

```bash
cargo tauri icon path/to/logo.png
```
