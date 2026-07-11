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
