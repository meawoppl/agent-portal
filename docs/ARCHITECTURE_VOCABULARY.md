# Architecture Vocabulary

This document names the major runtime pieces in agent-portal and how messages move between them. Use these terms when discussing code changes so issues, PRs, and code comments describe the same boundaries.

## Runtime Components

| Term | Location | Responsibility |
|------|----------|----------------|
| Backend | `backend/` | Axum HTTP server, PostgreSQL persistence, authentication, WebSocket coordination, static frontend serving |
| Frontend | `frontend/` | Yew WebAssembly UI for dashboards, sessions, messages, settings, and interactive browser workflows |
| Shared | `shared/` | Protocol, API, and data-transfer types used by backend, frontend, proxy, and launcher crates |
| Proxy | `proxy/` | Local CLI wrapper that runs an agent process and streams session traffic to the backend |
| Launcher | `launcher/` | Long-lived local daemon that registers a machine with the backend and starts agent sessions on request |
| Portal auth | `portal-auth/` | Reusable OAuth device-flow client logic for local binaries |
| Portal update | `portal-update/` | Shared release and update logic for local binaries |

## Core Concepts

| Term | Meaning |
|------|---------|
| Portal | The complete system: backend, frontend, shared protocol, and local binaries working together |
| Session | A persisted conversation/workflow record owned by a user and optionally shared with collaborators |
| Web client | A browser frontend connected to the backend over HTTP and WebSocket |
| Agent process | The local AI coding process managed by the proxy or launcher |
| Device flow | The browser-assisted login flow that grants a local binary a proxy token |
| Proxy token | A JWT-backed credential used by local binaries to authenticate back to the backend |
| Launcher token | A backend-issued token used to authorize launcher-specific machine/session operations |
| Shared protocol | The typed message and endpoint contracts that keep Rust backend, WASM frontend, and local binaries aligned |

## Message Flow

1. A user opens the frontend and authenticates with the backend.
2. The frontend lists sessions over HTTP and subscribes to live session updates over WebSocket.
3. A local proxy or launcher authenticates to the backend with device-flow credentials.
4. The backend coordinates session ownership, membership, replay, and fanout.
5. Agent output, tool activity, permissions, uploads, and status updates flow through shared protocol types.
6. The frontend renders persisted history plus live updates from the backend.

## Ownership Boundaries

- Keep authentication and route behavior in the backend; local binaries should not infer backend access rules.
- Keep transport contracts in `shared/` when more than one crate needs the same shape.
- Keep browser-only rendering state in `frontend/`; avoid leaking UI-only concepts into protocol types.
- Keep local process lifecycle concerns in `proxy/` or `launcher/`; the backend should coordinate, not manage local OS process details directly.
- Prefer small handler modules when a backend flow mixes persistence, authentication, protocol mapping, and response rendering.
