# Agent Portal Mobile

This is the Tauri 2 mobile shell for Agent Portal. The app is intentionally a
thin remote-URL WebView: server-deployed frontend changes ship without a store
release, and native code is reserved for mobile-only capabilities such as deep
links, system-browser auth handoff, push registration, and share targets.

## Prerequisites

- Rust stable with the Android and/or iOS targets installed.
- Node.js 20+.
- Tauri 2 mobile prerequisites:
  - Android: Android Studio, Android SDK, NDK, and a configured emulator or device.
  - iOS: macOS with Xcode and a simulator or device.

Install the local CLI dependencies from this directory:

```bash
npm install
```

## Remote URL

The checked-in default loads `https://txcl.io`. For local development against a
backend on your machine, pass a temporary Tauri config override:

```bash
npm run android:dev -- --config '{"app":{"windows":[{"url":"http://10.0.2.2:3000"}]}}'
```

For an iOS simulator talking to a local backend:

```bash
npm run ios:dev -- --config '{"app":{"windows":[{"url":"http://localhost:3000"}]}}'
```

Use a LAN IP instead of `localhost` for physical devices.

For a self-hosted or long-lived dev shell build, bake the target URL into the
native binary instead:

```bash
PORTAL_SHELL_URL="https://portal.example.com" npm run android:build
```

When unset, the app loads `https://txcl.io` directly with no startup redirect.

## Deep Links

The shell registers verified HTTPS app links / universal links for `txcl.io`.
Opened links are routed into the existing WebView, so URLs such as
`https://txcl.io/dashboard?session=<id>` land on the corresponding dashboard
session.

The backend serves the association documents at:

- `/.well-known/assetlinks.json`
- `/.well-known/apple-app-site-association`

Set `PORTAL_MOBILE_ANDROID_SHA256_CERT_FINGERPRINTS` and
`PORTAL_MOBILE_APPLE_TEAM_ID` on the backend before production verification.
`PORTAL_MOBILE_BUNDLE_ID` defaults to `io.txcl.agentportal`.

## Mobile Authentication

On first launch, the native shell requests a mobile device-flow code from the
configured portal origin with `client_type=mobile`, opens the pre-filled verify
page in the system browser, polls until the flow completes, and stores the
returned mobile JWT in the Tauri store. The app then runs
`POST /api/auth/token-login` from inside the WebView so the session cookie is
written into the WebView's cookie jar before navigating to the current portal
URL, preserving deep links such as `/dashboard?session=<id>`.

On startup and when the app returns to the foreground, the shell calls
`POST /api/auth/refresh` with the stored mobile JWT. If the backend returns a
replacement token, the shell saves it and repeats the WebView token-login step.

The initial token persistence uses `tauri-plugin-store`, which is app-private
persistent storage and keeps this first auth handoff small and portable across
Android/iOS. A later native-security pass can swap this for a platform keychain
bridge without changing the backend API contract.

## First-Time Native Project Generation

Generate the platform project before the first device run:

```bash
npm run android:init
npm run ios:init
```

The generated Android and iOS project files should be committed by the PR that
first runs each platform init. This scaffold keeps E1 small and gives follow-up
PRs a clean place to add deep links, auth handoff, push bridges, and share
targets.

## iOS push (APNs) bridge

The APNs registration bridge lives in [`ios/`](ios/) —
`PushRegistrationBridge.swift` plus its integration checklist
([`ios/README.md`](ios/README.md)). It is kept outside the gitignored
`gen/apple` tree so `ios:init` regeneration can't destroy it; add it to the
Xcode project when wiring push (requires the Push Notifications entitlement
and, in CI, the F2/F3 signing prerequisites).

## Development

Run on Android:

```bash
npm run android:dev
```

Run on iOS:

```bash
npm run ios:dev
```

The fallback `mobile/www/index.html` is only a splash screen for build tooling;
normal app navigation uses the configured remote URL.

## Icons

`src-tauri/icons/` holds the canonical Tauri icon set (generated with
`cargo tauri icon <source.png>`), including the Android `mipmap-*` foreground /
launcher variants. `tauri::generate_context!` embeds a default window icon, so
these must exist for the crate to even compile for a mobile target — regenerate
the whole set from a single source PNG rather than hand-editing individual sizes.

## CI

`.github/workflows/mobile-android.yml` (job **Android Debug APK**) builds an
unsigned debug APK. It runs on:

- **Pull requests** that touch `mobile/**` or the workflow file itself.
- **Manual dispatch** (Actions tab → "Mobile Android" → "Run workflow").

It is an **additive, non-required** lane: it is deliberately kept out of the
`pr-to-main` branch-protection ruleset, so a mobile build failure never blocks a
merge (and, conversely, do not rename its job into a required lane — see #1217).

What the lane does, in order:

1. Installs the pinned Rust toolchain with the `aarch64-linux-android` target,
   Java 17 (Temurin), and the Android SDK cmdline-tools + a pinned NDK (26.x).
2. Runs `cargo clippy -p agent-portal-mobile --target aarch64-linux-android -- -D warnings`
   as a fast-fail gate before the slow build.
3. Runs `cargo tauri android init --ci` to generate `gen/android` (the native
   project is not committed; it is regenerated every run).
4. Runs `cargo tauri android build --debug --target aarch64` to produce the APK.

**Downloading the debug APK:** open the workflow run (Actions tab or the PR's
"Checks" view), scroll to the **Artifacts** section, and download
`agent-portal-android-debug`. It contains the unsigned `*-debug.apk`, installable
on a device or emulator with `adb install <file>.apk` (developer mode / unknown
sources enabled).
