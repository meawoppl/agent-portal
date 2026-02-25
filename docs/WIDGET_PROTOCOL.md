# Widget Protocol

This document specifies the protocol for integrating browser-based page interaction widgets with the portal. It enables Claude sessions to observe and interact with web pages rendered in the user's browser via a widget iframe.

## Overview

```
browser widget ──WS──► backend ◄──REST/WS── proxy (Claude)
     │                    │
     │  executes commands  │  bridges commands
     │  returns results    │  to widget WS
     │                    │
     ▼                    ▼
  page DOM           pending requests
```

A **widget** is a small script injected into (or iframe'd alongside) a target web page. It maintains a WebSocket connection to the portal backend, receives commands (screenshot, click, type, etc.), executes them against the page DOM, and returns results. The portal backend exposes these capabilities to Claude sessions through REST endpoints and/or WebSocket command forwarding.

## Concepts

### Session Binding

Each widget connection is bound to a portal session via `portal_session_id`. This allows the backend to route commands from a specific Claude session to the correct widget instance. A session may have zero or one widget connected at a time.

### Command-Response Pattern

All interactions follow a request-response pattern:

1. A command originates from the agent (via REST or WS)
2. The backend assigns a `request_id` and forwards the command to the widget over WebSocket
3. The widget executes the command against the page
4. The widget sends back a response with the matching `request_id`
5. The backend resolves the pending request and returns the result to the caller

Commands time out after **30 seconds** if the widget does not respond.

### Widget Lifecycle

```
1. User opens a page with the widget script loaded
2. Widget connects to /ws/widget
3. Widget sends WidgetRegister { portal_session_id, page_url }
4. Backend sends WidgetRegisterAck { success, widget_id }
5. Widget is now ready to receive commands
6. On page unload or navigation, widget sends WidgetPageChange { url }
7. On disconnect, backend marks the widget slot as empty
```

## WebSocket Endpoint

### `/ws/widget` — Widget Connection

```rust
pub struct WidgetEndpoint;

impl WsEndpoint for WidgetEndpoint {
    const PATH: &'static str = "/ws/widget";
    type ServerMsg = ServerToWidget;
    type ClientMsg = WidgetToServer;
}
```

## Message Types

### Widget → Server

#### `WidgetRegister`

Sent immediately after connecting. Associates this widget with a portal session.

```json
{
  "type": "WidgetRegister",
  "portal_session_id": "uuid",
  "page_url": "https://example.com/app",
  "viewport": {
    "width": 1280,
    "height": 720
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `portal_session_id` | UUID | The portal session this widget serves |
| `page_url` | string | Current URL of the page |
| `viewport` | object | Current viewport dimensions |

#### `CommandResponse`

Result of executing a command.

```json
{
  "type": "CommandResponse",
  "request_id": "uuid",
  "success": true,
  "data": { ... },
  "error": null
}
```

| Field | Type | Description |
|-------|------|-------------|
| `request_id` | UUID | Matches the originating command |
| `success` | bool | Whether the command succeeded |
| `data` | Value | Command-specific result payload |
| `error` | string? | Error message if `success` is false |

#### `WidgetPageChange`

Sent when the page navigates to a new URL.

```json
{
  "type": "WidgetPageChange",
  "page_url": "https://example.com/new-page",
  "viewport": {
    "width": 1280,
    "height": 720
  }
}
```

#### `WidgetHeartbeat`

Keepalive signal.

```json
{ "type": "WidgetHeartbeat" }
```

### Server → Widget

#### `WidgetRegisterAck`

Acknowledges widget registration.

```json
{
  "type": "WidgetRegisterAck",
  "success": true,
  "widget_id": "uuid",
  "error": null
}
```

#### `Command`

A command for the widget to execute against the page.

```json
{
  "type": "Command",
  "request_id": "uuid",
  "command": "screenshot",
  "params": {}
}
```

| Field | Type | Description |
|-------|------|-------------|
| `request_id` | UUID | Unique ID for correlating the response |
| `command` | string | Command name (see Command Catalog below) |
| `params` | Value | Command-specific parameters |

#### `ServerShutdown`

Reuses the existing portal shutdown message format.

```json
{
  "type": "ServerShutdown",
  "reason": "Server restarting",
  "reconnect_delay_ms": 5000
}
```

## Command Catalog

### Observation Commands

These commands read page state without modifying it.

#### `screenshot`

Captures a screenshot of the current viewport.

**Params:** `{}`

**Response data:**
```json
{
  "image": "data:image/png;base64,...",
  "width": 1280,
  "height": 720,
  "timestamp": 1708790400000
}
```

#### `getDom`

Returns a serialized snapshot of the page's DOM, optionally filtered by a CSS selector.

**Params:**
```json
{
  "selector": "body",
  "depth": 5,
  "include_styles": false
}
```

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `selector` | string | `"body"` | Root CSS selector |
| `depth` | int | `10` | Max DOM tree depth |
| `include_styles` | bool | `false` | Include computed styles |

**Response data:**
```json
{
  "html": "<body>...</body>",
  "node_count": 142
}
```

#### `getAccessibilityTree`

Returns a simplified accessibility tree for the page or a subtree.

**Params:**
```json
{
  "selector": "body",
  "depth": 5
}
```

**Response data:**
```json
{
  "tree": {
    "role": "main",
    "name": "Application",
    "children": [
      { "role": "button", "name": "Submit", "selector": "#submit-btn" }
    ]
  }
}
```

#### `getConsole`

Returns buffered console output since last call (or since widget connection).

**Params:** `{}`

**Response data:**
```json
{
  "entries": [
    { "level": "log", "message": "App loaded", "timestamp": 1708790400000 },
    { "level": "error", "message": "Failed to fetch", "timestamp": 1708790401000 }
  ]
}
```

#### `getNetwork`

Returns buffered network requests since last call.

**Params:** `{}`

**Response data:**
```json
{
  "requests": [
    {
      "method": "GET",
      "url": "https://api.example.com/data",
      "status": 200,
      "duration_ms": 142,
      "response_size": 4096,
      "timestamp": 1708790400000
    }
  ]
}
```

#### `getEnvironment`

Returns page metadata: URL, title, viewport, cookies, localStorage summary.

**Params:** `{}`

**Response data:**
```json
{
  "url": "https://example.com/app",
  "title": "My App",
  "viewport": { "width": 1280, "height": 720 },
  "cookie_count": 5,
  "local_storage_keys": ["token", "theme", "lang"]
}
```

### Interaction Commands

These commands modify page state or simulate user input.

#### `click`

Clicks an element matching a CSS selector.

**Params:**
```json
{
  "selector": "#submit-btn"
}
```

**Response data:**
```json
{
  "clicked": true,
  "element": { "tag": "button", "text": "Submit" }
}
```

#### `clickAt`

Clicks at specific viewport coordinates.

**Params:**
```json
{
  "x": 640,
  "y": 360
}
```

#### `type`

Types text into a focused or selected element.

**Params:**
```json
{
  "selector": "#email-input",
  "text": "user@example.com",
  "clear": true
}
```

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `selector` | string | required | Target element |
| `text` | string | required | Text to type |
| `clear` | bool | `false` | Clear existing value first |

#### `navigate`

Navigates the page to a new URL.

**Params:**
```json
{
  "url": "https://example.com/other-page"
}
```

**Response data:**
```json
{
  "navigated": true,
  "url": "https://example.com/other-page"
}
```

#### `execute`

Executes arbitrary JavaScript in the page context and returns the result.

**Params:**
```json
{
  "code": "document.querySelectorAll('li').length"
}
```

**Response data:**
```json
{
  "result": 12
}
```

### Mouse Commands

Fine-grained mouse control for drag, hover, and other complex interactions.

#### `moveMouse`

**Params:** `{ "x": 100, "y": 200 }`

#### `hover`

**Params:** `{ "selector": ".tooltip-trigger" }`

#### `mouseDown` / `mouseUp`

**Params:** `{ "x": 100, "y": 200, "button": "left" }`

Button values: `"left"`, `"right"`, `"middle"`.

#### `drag`

**Params:**
```json
{
  "from": { "x": 100, "y": 200 },
  "to": { "x": 300, "y": 400 }
}
```

### Keyboard Commands

Fine-grained keyboard control.

#### `pressKey`

Presses and releases a key.

**Params:** `{ "key": "Enter" }`

Key values follow the [KeyboardEvent.key](https://developer.mozilla.org/en-US/docs/Web/API/UI_Events/Keyboard_event_key_values) spec.

#### `keyDown` / `keyUp`

**Params:** `{ "key": "Shift" }`

#### `typeText`

Types a string character-by-character with realistic timing.

**Params:** `{ "text": "hello world", "delay_ms": 50 }`

### Waiting Commands

#### `waitFor`

Waits for a CSS selector to appear in the DOM.

**Params:**
```json
{
  "selector": ".results-loaded",
  "timeout_ms": 5000,
  "visible": true
}
```

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `selector` | string | required | CSS selector to wait for |
| `timeout_ms` | int | `5000` | Max wait time |
| `visible` | bool | `false` | Require element to be visible (not just in DOM) |

**Response data:**
```json
{
  "found": true,
  "elapsed_ms": 1200
}
```

### Batch Commands

#### `batch`

Executes multiple commands sequentially, returning all results.

**Params:**
```json
{
  "commands": [
    { "command": "click", "params": { "selector": "#tab-2" } },
    { "command": "waitFor", "params": { "selector": ".tab-2-content" } },
    { "command": "screenshot", "params": {} }
  ]
}
```

**Response data:**
```json
{
  "results": [
    { "success": true, "data": { "clicked": true, "element": { "tag": "a", "text": "Tab 2" } } },
    { "success": true, "data": { "found": true, "elapsed_ms": 300 } },
    { "success": true, "data": { "image": "data:image/png;base64,..." } }
  ]
}
```

If any command fails, subsequent commands are **not** executed and their results are `null`.

## REST API Endpoints

These endpoints allow the agent (Claude via proxy) to interact with the widget bound to its session. All endpoints are under `/api/widget/:session_id/`.

Authentication: same session cookie or proxy auth token used for other portal endpoints.

### `GET /api/widget/:session_id/status`

Returns whether a widget is connected for this session.

**Response:**
```json
{
  "connected": true,
  "page_url": "https://example.com/app",
  "viewport": { "width": 1280, "height": 720 },
  "connected_at": "2026-02-24T10:30:00Z"
}
```

### `POST /api/widget/:session_id/command`

Sends a command to the widget and waits for the response.

**Request body:**
```json
{
  "command": "screenshot",
  "params": {},
  "timeout_ms": 30000
}
```

**Response:** the `CommandResponse` data, or a 504 Gateway Timeout if the widget doesn't respond within `timeout_ms`.

### Convenience Endpoints

These are thin wrappers around `POST /command` for common operations:

| Endpoint | Method | Equivalent Command |
|----------|--------|--------------------|
| `/screenshot` | GET | `{ "command": "screenshot" }` |
| `/dom` | GET | `{ "command": "getDom" }` |
| `/a11y` | GET | `{ "command": "getAccessibilityTree" }` |
| `/console` | GET | `{ "command": "getConsole" }` |
| `/network` | GET | `{ "command": "getNetwork" }` |
| `/environment` | GET | `{ "command": "getEnvironment" }` |
| `/click` | POST | `{ "command": "click", "params": body }` |
| `/type` | POST | `{ "command": "type", "params": body }` |
| `/navigate` | POST | `{ "command": "navigate", "params": body }` |
| `/execute` | POST | `{ "command": "execute", "params": body }` |
| `/wait-for` | POST | `{ "command": "waitFor", "params": body }` |
| `/batch` | POST | `{ "command": "batch", "params": body }` |

## Backend Implementation

### State Tracking

Add to `SessionManager`:

```rust
/// Connected widget instances, keyed by portal_session_id
widget_sessions: DashMap<Uuid, WidgetConnection>,

/// Pending command requests awaiting widget response
widget_pending: DashMap<Uuid, PendingWidgetCommand>,
```

```rust
struct WidgetConnection {
    widget_id: Uuid,
    portal_session_id: Uuid,
    page_url: String,
    viewport: Viewport,
    connected_at: chrono::DateTime<chrono::Utc>,
    sender: mpsc::Sender<ServerToWidget>,
}

struct PendingWidgetCommand {
    request_id: Uuid,
    response_tx: oneshot::Sender<CommandResponse>,
    created_at: std::time::Instant,
}

struct Viewport {
    width: u32,
    height: u32,
}
```

### Command Bridge Flow

```
REST handler                    SessionManager              Widget WS
     │                              │                          │
     ├─ send_widget_command() ─────►│                          │
     │                              ├─ creates PendingCmd      │
     │                              ├─ Command{request_id} ───►│
     │   (awaits oneshot)           │                          ├─ executes
     │                              │◄── CommandResponse ──────┤
     │                              ├─ resolves PendingCmd     │
     │◄── CommandResponse ─────────┤                          │
     │                              │                          │
```

The `send_widget_command` method:

1. Looks up the widget connection for the given `portal_session_id`
2. Creates a `oneshot::channel` for the response
3. Inserts a `PendingWidgetCommand` into `widget_pending` keyed by `request_id`
4. Sends the `Command` message to the widget via its `mpsc::Sender`
5. Awaits the oneshot receiver with a timeout
6. On timeout, removes the pending entry and returns 504

When a `CommandResponse` arrives on the widget WebSocket:

1. Look up `request_id` in `widget_pending`
2. Remove the entry and send the response through the oneshot sender
3. If no pending entry found (timed out), discard the response

### Cleanup

- When the widget WebSocket disconnects, remove its entry from `widget_sessions`
- A periodic task (every 10s) sweeps `widget_pending` for entries older than the timeout and resolves them with a timeout error

## Shared Types

Types needed in `shared/src/` for WASM compatibility (used by both backend and frontend):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetStatus {
    pub connected: bool,
    pub page_url: Option<String>,
    pub viewport: Option<Viewport>,
    pub connected_at: Option<String>,
}
```

The full `ServerToWidget` / `WidgetToServer` enums live in `shared/src/endpoints.rs` alongside the other endpoint definitions.

## Widget Client (Browser-Side)

The widget client is a JavaScript module that:

1. Connects to `wss://{portal_host}/ws/widget`
2. Sends `WidgetRegister` with the `portal_session_id` (obtained from URL params, postMessage, or config)
3. Listens for `Command` messages and dispatches them to handler functions
4. Sends `CommandResponse` back for each command
5. Monitors page navigation and sends `WidgetPageChange`
6. Sends `WidgetHeartbeat` every 30 seconds
7. Reconnects with exponential backoff on disconnect

### Injection Methods

The widget script can be loaded into a target page via:

- **Browser extension**: Injects the script into all/matching pages
- **Bookmarklet**: User activates manually on a page
- **Script tag**: Added to pages under development
- **iframe companion**: Widget runs in a sibling iframe

The `portal_session_id` is passed to the widget via:
- URL query parameter: `?portal_session=uuid`
- `window.postMessage` from the portal frontend
- Manual configuration in the script tag

## Security Considerations

- The `execute` command runs arbitrary JavaScript in the page context. In production, consider restricting this to allowlisted patterns or removing it entirely.
- Widget connections must authenticate (same auth mechanisms as other WS endpoints).
- The `portal_session_id` binding ensures commands are only routed to the widget owned by that session's user.
- Screenshot data can be large (several MB as base64). Consider compression or binary WebSocket frames for production use.
- Console and network buffers should be size-capped to prevent memory exhaustion.

## MCP Integration Path

The REST endpoints map naturally to an MCP (Model Context Protocol) tool server. Each convenience endpoint becomes an MCP tool:

| MCP Tool | Endpoint |
|----------|----------|
| `widget_screenshot` | `GET /screenshot` |
| `widget_click` | `POST /click` |
| `widget_type` | `POST /type` |
| `widget_navigate` | `POST /navigate` |
| `widget_execute` | `POST /execute` |
| `widget_dom` | `GET /dom` |
| `widget_console` | `GET /console` |

This allows any MCP-compatible agent to interact with the widget without custom integration.

## File Changes Summary

| File | Change |
|------|--------|
| `shared/src/endpoints.rs` | `WidgetEndpoint`, `ServerToWidget`, `WidgetToServer` |
| `shared/src/lib.rs` | `Viewport`, `WidgetStatus` types |
| `backend/src/main.rs` | `/ws/widget` route, `/api/widget/` routes |
| `backend/src/handlers/websocket/` | Widget WS handler |
| `backend/src/handlers/widget.rs` | REST command endpoints |
| `backend/src/handlers/websocket/session_manager.rs` | `widget_sessions`, `widget_pending` maps |
