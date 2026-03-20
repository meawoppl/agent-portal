# Proxy Binary Login Flow

This document describes how the `claude-portal` CLI authenticates with the agent-portal backend.

## Overview

The proxy supports three authentication methods:

1. **JWT Token Init** (Recommended) - One-time setup via init URL from web UI
2. **Device Flow OAuth** - Interactive browser-based authentication
3. **Dev Mode** - Bypass authentication for local development

## Authentication Methods

### 1. JWT Token Init (Recommended)

The simplest way to authenticate the proxy is using a pre-generated JWT token from the web interface.

#### Flow Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                         WEB INTERFACE                                │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  1. User logs in via Google OAuth                                   │
│                     │                                                │
│                     ▼                                                │
│  2. User clicks "Create Proxy Token"                                │
│                     │                                                │
│                     ▼                                                │
│  3. Backend generates JWT + stores hash in DB                       │
│                     │                                                │
│                     ▼                                                │
│  4. User receives init URL:                                         │
│     https://server.com/p/{base64_encoded_config}                    │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
                      │
                      │ (User copies command)
                      ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         CLI (one-time setup)                         │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  $ claude-portal --init "https://server.com/p/eyJ0Ijoi..."           │
│                     │                                                │
│                     ▼                                                │
│  5. Proxy decodes config from URL                                   │
│     - Extracts backend URL                                          │
│     - Extracts JWT token                                            │
│     - Extracts optional session prefix                              │
│                     │                                                │
│                     ▼                                                │
│  6. Saves to ~/.config/agent-portal/config.json                         │
│                     │                                                │
│                     ▼                                                │
│  ✓ Configuration saved for user@example.com                         │
│    Backend: wss://server.com                                        │
│                                                                      │
│    You can now run claude-portal without arguments.                  │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
                      │
                      │ (Future runs)
                      ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         CLI (normal usage)                           │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  $ claude-portal                                                      │
│                     │                                                │
│                     ▼                                                │
│  7. Loads token from config.json                                    │
│                     │                                                │
│                     ▼                                                │
│  8. Connects to backend WebSocket                                   │
│                     │                                                │
│                     ▼                                                │
│  9. Sends Register message with JWT token                           │
│                     │                                                │
│                     ▼                                                │
│  10. Backend verifies JWT:                                          │
│      a. Check signature (stateless)                                 │
│      b. Check not revoked (DB lookup)                               │
│      c. Update last_used_at                                         │
│                     │                                                │
│                     ▼                                                │
│  11. Session created, linked to user account                        │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

#### Commands

```bash
# One-time setup with init URL
claude-portal --init "https://server.com/p/eyJ0IjoiZXlKaGJHY2lP..."

# One-time setup with raw JWT token
claude-portal --init "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..."

# Normal usage after init
claude-portal

# With custom session name
claude-portal --session-name "my-workstation"
```

#### JWT Token Structure

The JWT contains these claims:

```json
{
  "jti": "550e8400-e29b-41d4-a716-446655440000",  // Token ID (for revocation)
  "sub": "123e4567-e89b-12d3-a456-426614174000",  // User ID
  "email": "user@example.com",                     // User email
  "iat": 1704844800,                               // Issued at
  "exp": 1707523200                                // Expires at
}
```

#### Init URL Format

The init URL encodes a JSON config in base64url:

```
https://server.com/p/{base64url_encode(config)}
```

Where config is:

```json
{
  "t": "eyJhbGciOiJIUzI1NiIs...",  // JWT token
  "n": "matthew-"                   // Optional: session name prefix
}
```

---

### 2. Device Flow OAuth

For environments where you can't easily copy an init URL, the proxy supports OAuth device flow.

#### Flow Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                              CLI                                     │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  $ claude-portal --backend-url wss://server.com                      │
│                     │                                                │
│                     ▼                                                │
│  1. No cached credentials found                                     │
│                     │                                                │
│                     ▼                                                │
│  2. POST /api/auth/device/code                                          │
│     Response: { device_code, user_code, verification_uri }          │
│                     │                                                │
│                     ▼                                                │
│  3. Display to user:                                                │
│     ┌─────────────────────────────────────────────┐                 │
│     │  To authenticate, visit:                    │                 │
│     │  https://server.com/api/auth/device          │                 │
│     │                                             │                 │
│     │  And enter code: ABC-123                    │                 │
│     └─────────────────────────────────────────────┘                 │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
                      │
                      │ (User opens browser)
                      ▼
┌─────────────────────────────────────────────────────────────────────┐
│                           BROWSER                                    │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  4. User visits verification URL                                    │
│                     │                                                │
│                     ▼                                                │
│  5. Redirected to Google OAuth                                      │
│                     │                                                │
│                     ▼                                                │
│  6. User signs in with Google                                       │
│                     │                                                │
│                     ▼                                                │
│  7. Backend links user_code to authenticated user                   │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
                      │
                      │ (Meanwhile, CLI is polling)
                      ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         CLI (polling)                                │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  8. POST /api/auth/device/poll { device_code }                          │
│     Response: { status: "pending" }                                 │
│     ... (every 5 seconds)                                           │
│                     │                                                │
│                     ▼                                                │
│  9. POST /api/auth/device/poll { device_code }                          │
│     Response: { status: "complete", access_token, user_email }      │
│                     │                                                │
│                     ▼                                                │
│  10. Save credentials to config.json                                │
│                     │                                                │
│                     ▼                                                │
│  ✓ Authentication successful!                                       │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

#### Commands

```bash
# Trigger device flow (when no cached credentials)
claude-portal --backend-url wss://server.com

# Force re-authentication
claude-portal --reauth

# Logout (clear cached credentials)
claude-portal --logout
```

---

### 3. Dev Mode

For local development, authentication can be bypassed entirely.

```bash
# Backend must be started with --dev-mode
cargo run -p backend -- --dev-mode

# Proxy uses --dev flag
claude-portal --dev --backend-url ws://localhost:3000
```

In dev mode:
- Backend creates a test user: `testing@testing.local`
- Proxy skips authentication entirely
- Sessions are linked to the test user

---

## Configuration Storage

Credentials are stored in:

```
~/.config/agent-portal/config.json
```

Structure:

```json
{
  "sessions": {
    "/path/to/project": {
      "user_id": "123e4567-e89b-12d3-a456-426614174000",
      "auth_token": "eyJhbGciOiJIUzI1NiIs...",
      "user_email": "user@example.com",
      "last_used": "2024-01-09T12:00:00Z",
      "backend_url": "wss://server.com",
      "session_prefix": "matthew-"
    }
  },
  "preferences": {
    "default_backend_url": null,
    "auto_open_browser": false
  }
}
```

Credentials are stored per-directory, allowing different authentication for different projects.

---

## Token Lifecycle

### Creation

1. User creates token via web UI (`POST /api/proxy-tokens`)
2. Backend generates JWT with claims (jti, sub, email, iat, exp)
3. Backend stores SHA256 hash of JWT in `proxy_auth_tokens` table
4. JWT returned to user (only shown once)

### Verification (on each connection)

1. Proxy sends JWT in WebSocket Register message
2. Backend verifies JWT signature (stateless, fast)
3. Backend checks `proxy_auth_tokens` table:
   - Token exists (by hash lookup)
   - Not revoked
   - Not expired
4. Backend updates `last_used_at` timestamp
5. Session created with user_id from JWT claims

### Revocation

1. User revokes token via web UI (`DELETE /api/proxy-tokens/:id`)
2. Backend sets `revoked = true` in database
3. Future connections with that token are rejected

---

## Security Considerations

| Aspect | Implementation |
|--------|----------------|
| Token storage | JWT stored in user's config file with standard file permissions |
| Transport | WSS (WebSocket Secure) required in production |
| Signature | HMAC-SHA256 with server-side secret |
| Revocation | Database lookup on each connection |
| Expiration | JWT `exp` claim + database `expires_at` |
| Audit | `last_used_at` tracked for each token |

---

## Troubleshooting

### "Token verification failed"

- Token may be expired - create a new one
- Token may be revoked - check web UI
- Backend secret may have changed - create a new token

### "No cached credentials"

- Run `claude-portal --init <url>` with a token from the web UI
- Or use device flow by just running `claude-portal`

### "Connection refused"

- Check backend is running
- Check backend URL is correct (ws:// vs wss://)
- Check firewall settings

### Clear credentials and start fresh

```bash
claude-portal --logout
```
