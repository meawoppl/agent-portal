# Agent Portal Mobile

This is the Tauri 2 mobile shell for Agent Portal. The app is intentionally a
thin remote-URL WebView: server-deployed frontend changes ship without a store
release, and native code is reserved for mobile-only capabilities such as deep
links, system-browser auth handoff, push registration, and share targets.

## Prerequisites

- Rust stable with the Android and/or iOS targets installed:
  ```bash
  rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
  ```
- Node.js 20+.
- Tauri 2 mobile prerequisites:
  - Android: Android Studio, Android SDK, NDK, and a configured emulator or device.
  - iOS: macOS with Xcode (the full app, not just Command Line Tools) plus
    CocoaPods, and a simulator or device. If `xcode-select -p` reports
    `/Library/Developer/CommandLineTools`, point it at Xcode first:
    ```bash
    sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
    sudo xcodebuild -license accept
    ```

Install the local CLI dependencies from this directory:

```bash
npm install
```

The Tauri iOS subcommand is only compiled into the CLI on macOS. On Linux,
`npm run ios:init` / `cargo tauri ios ...` will report that `ios` is not a
recognized subcommand even when the same Tauri version is installed; use a Mac
with Xcode or the macOS CI lane below for the full iOS build.

## Remote URL

Two config keys are in play, and they do **different** jobs — conflating them
is the main way to get stuck here:

| Key | Job |
|---|---|
| `app.windows[].url` (`https://txcl.io`) | What the WebView actually loads. Applies to **both** dev and build — this window's URL is an explicit external URL, so Tauri uses it verbatim rather than resolving it against the dev server. |
| `build.devUrl` (`http://localhost:3000`) | A **readiness gate only**. `*:dev` polls it *from the host* and refuses to build until something answers. It does not change what the WebView loads. |

The gate is the surprising half: with nothing listening on `localhost:3000`,
`npm run ios:dev` never starts building and just loops on
`Waiting for your frontend dev server to start on http://localhost:3000/`. So
for any `*:dev` run, start the backend first:

```bash
./scripts/dev.sh start
```

With the backend up, point the WebView at it by overriding the window URL. The
iOS simulator shares the Mac's `localhost`; the Android emulator reaches host
loopback via `10.0.2.2` (the host-side gate still polls `localhost` either way):

```bash
npm run ios:dev -- --config '{"app":{"windows":[{"url":"http://localhost:3000"}]}}'
npm run android:dev -- --config '{"app":{"windows":[{"url":"http://10.0.2.2:3000"}]}}'
```

Use a LAN IP instead of `localhost` for physical devices.

Without those overrides a `*:dev` run still loads the **deployed** portal, since
that is what `app.windows[].url` says. If you want that *and* don't want to run
a local backend, point the gate at something that answers so it stops blocking:

```bash
npm run ios:dev -- --config '{"build":{"devUrl":"https://txcl.io"}}'
```

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

## Android Status Surfaces

Android builds include a persistent status notification and a home-screen
widget. Both use the stored mobile JWT to poll `GET /api/agent/sessions` while
the app process is alive, currently every 45 seconds, and clear themselves when
no active sessions are present. The notification runs as a `dataSync` foreground
service; the widget renders the same saved session payload, so it updates
whenever the notification poller receives fresh status.

The widget also schedules a native WorkManager refresh using the same stored
mobile JWT and status endpoint. Android enforces a 15-minute minimum interval
for periodic work; widget system updates also enqueue a one-time refresh when
network is available. The widget summary includes a compact freshness label,
switching to a stale marker after the expected refresh window so cached session
state is visible as cached state. Tapping the widget opens the dashboard deep
link, which brings the app process back and lets the poller/one-shot refresh
run again.

On Android 13+, the first status update requests `POST_NOTIFICATIONS`; if the
permission is not granted yet, the shell skips the update and retries on the
next poll. Dogfood should confirm that first-launch permission UX is clear.

If device/OEM battery policy reaps the app process despite the foreground
service, note the device and behavior here. The FCM data-message bridge below
is the durable refresh path that wakes the Android surfaces without relying
only on process-local polling.

## Android FCM Bridge

Android builds include the Firebase Messaging receive bridge, but it is
runtime-gated: without Firebase configuration, token registration is skipped
and CI still builds a debug APK. When Firebase is configured, the shell obtains
the FCM registration token after mobile auth and registers it with
`POST /api/push/subscriptions` as:

```json
{
  "platform": "fcm",
  "endpoint_or_token": "<FCM registration token>",
  "p256dh": null,
  "auth": null,
  "device_label": "Agent Portal Android"
}
```

The request uses the stored mobile JWT as `Authorization: Bearer ...`.
`onNewToken` re-posts the rotated token. When the shell rejects the stored JWT
and clears auth, it also deletes the saved backend subscription id if one was
registered.

FCM data messages enqueue the same native status refresh used by the widget
WorkManager path; that refresh updates the widget and best-effort re-shows the
persistent status notification from the fetched session payload.

To enable FCM for a local or release Android build, Matt must provision:

- A Firebase project with an Android app for `io.txcl.agentportal`.
- The app's `google-services.json` in the generated Android app module after
  `npm run android:init` (`mobile/src-tauri/gen/android/app/google-services.json`).
- Conditional Google Services plugin application in the generated Android app
  Gradle file only when that JSON exists. Do not require it in CI.
- A Firebase service-account JSON on the backend host and
  `PORTAL_FCM_SERVICE_ACCOUNT_PATH=/path/to/service-account.json` so the
  existing backend FCM transport can send to `platform=fcm` rows.

The generated Android app Gradle file should apply Google Services only when
the Firebase file is present, for example:

```kotlin
plugins {
    id("com.google.gms.google-services") version "4.4.2" apply false
}

if (file("google-services.json").exists()) {
    apply(plugin = "com.google.gms.google-services")
}
```

## First-Time Native Project Generation

Generate the platform project before the first device run:

```bash
npm run android:init
npm run ios:init
```

The generated projects land in `src-tauri/gen/` and are **not** committed —
`src-tauri/.gitignore` ignores `/gen/`, and the Android CI lane regenerates
`gen/android` on every run. Treat them as local build output: re-run the init
for a platform whenever you need it back, and never hand-edit files under
`gen/` expecting the change to survive.

This is also why anything that must persist across regeneration lives outside
`gen/` — see the APNs bridge in [`ios/`](ios/) below.

## iOS push (APNs) bridge

The APNs registration bridge lives in [`ios/`](ios/) —
`PushRegistrationBridge.swift` plus its integration checklist
([`ios/README.md`](ios/README.md)). It is kept outside the gitignored
`gen/apple` tree so `ios:init` regeneration can't destroy it; add it to the
Xcode project when wiring push (requires the Push Notifications entitlement
and, in CI, the F2/F3 signing prerequisites).

`mobile/src-tauri/build.rs` injects the `aps-environment` entitlement during
iOS builds so it survives `gen/apple` regeneration. Local/debug builds default
to `development`; the signed release workflow sets
`PORTAL_IOS_APS_ENVIRONMENT=production`.

## Development

Start the backend first (see [Remote URL](#remote-url) — `*:dev` waits on
`build.devUrl` and will not proceed without it), then:

```bash
npm run android:dev    # Android emulator or device
npm run ios:dev        # iOS simulator or device
```

Append a device name to skip the interactive picker, e.g.
`npm run ios:dev -- "iPhone 17 Pro"`. `xcrun simctl list devices available`
lists the installed simulators; if that comes up empty, install an iOS runtime
from Xcode → Settings → Components.

Either command stays in the foreground after the app launches, watching
`src-tauri/` and `shared/` for changes and rebuilding on edit — leave it
running for hot reload.

The `Warn No code signing certificates found` line at startup is expected and
harmless for simulator/emulator runs; signing only matters for physical devices
and release builds.

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
unsigned debug APK. `.github/workflows/mobile-ios.yml` (job **iOS Simulator
App**) builds an unsigned iOS simulator app on a macOS runner. They run on:

- **Pull requests** that touch `mobile/**` or the workflow file itself.
- **Manual dispatch** (Actions tab → "Mobile Android" or "Mobile iOS" → "Run
  workflow").

Both lanes are **additive, non-required** lanes: they are deliberately kept out
of the `pr-to-main` branch-protection ruleset, so a mobile build failure never
blocks a merge (and, conversely, do not rename their jobs into a required lane —
see #1217).

What the Android lane does, in order:

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

What the iOS lane does, in order:

1. Runs on `macos-latest`, installs the pinned Rust toolchain plus iOS device
   and simulator targets, and uses the npm-provided Tauri CLI from
   `@tauri-apps/cli`.
2. Chooses the simulator target from the runner architecture: `aarch64-sim` on
   Apple Silicon, `x86_64` on Intel.
3. Runs `cargo check -p agent-portal-mobile --target <ios-simulator-triple>` as
   a fast-fail Rust/iOS gate.
4. Runs `npx tauri ios init --ci` to generate `gen/apple`.
5. Runs `npx tauri ios build --debug --target <simulator-target> --no-sign`.

The iOS lane intentionally stops at an unsigned simulator build. Device
installation, APNs entitlements, archive export, and TestFlight upload need the
signing/cert/profile work from the release track before they can run in CI.

## iOS release lane

`.github/workflows/mobile-ios-release.yml` (job **Signed iOS IPA**) is a manual
workflow for the signed device/App Store path. It does not run on PRs because it
requires Apple credentials and provisioning. It builds `aarch64-apple-ios`,
exports a signed IPA, and uploads that IPA as the
`agent-portal-ios-signed-ipa` workflow artifact. When `upload_testflight` is
enabled on the manual dispatch, it also uploads the IPA to App Store Connect for
TestFlight processing.

Required GitHub Actions secrets:

| Secret | Purpose |
|---|---|
| `APPLE_API_KEY` | App Store Connect API key id, passed to Tauri/Xcode as `APPLE_API_KEY`. |
| `APPLE_API_ISSUER` | App Store Connect issuer id, passed as `APPLE_API_ISSUER`. |
| `APPLE_API_KEY_P8` | Raw `.p8` App Store Connect API key contents; CI writes it to `APPLE_API_KEY_PATH`. |
| `APPLE_DEVELOPMENT_TEAM` | Apple Developer Team ID for generated Xcode signing settings. |
| `IOS_CERTIFICATE` | Base64 distribution signing certificate consumed by Tauri's iOS signing support. |
| `IOS_CERTIFICATE_PASSWORD` | Password for `IOS_CERTIFICATE`. |
| `IOS_MOBILE_PROVISION` | Base64 mobile provisioning profile for `io.txcl.agentportal`. |

Manual run inputs:

- `export_method`: `app-store-connect` for App Store/TestFlight export,
  `release-testing` for ad-hoc distribution, or `debugging` for development
  signing.
- `shell_url`: portal origin baked into the shell via `PORTAL_SHELL_URL`
  (defaults to `https://txcl.io`).
- `build_number`: optional `CFBundleVersion` suffix; when blank, CI uses the
  workflow run number.
- `upload_testflight`: when true, runs `xcrun altool --upload-app` after the
  signed IPA artifact is collected. Leave this off until the first signed IPA is
  known-good and the App Store Connect app record exists.

Before the first TestFlight upload, confirm in Apple Developer/App Store
Connect that:

1. Bundle ID `io.txcl.agentportal` exists.
2. The provisioning profile includes the Push Notifications and Associated
   Domains capabilities used by the mobile shell.
3. App Store Connect has an app record for the bundle ID.
4. Backend env is set for link verification and APNs delivery:
   `PORTAL_MOBILE_APPLE_TEAM_ID`, `PORTAL_MOBILE_BUNDLE_ID`,
   `PORTAL_APNS_KEY_P8_PATH`, `PORTAL_APNS_KEY_ID`, `PORTAL_APNS_TEAM_ID`, and
   `PORTAL_APNS_BUNDLE_ID`.

The APNs Swift bridge still has to be added to the generated Xcode project and
connected to the shell's mobile JWT before iOS push registration is live.
