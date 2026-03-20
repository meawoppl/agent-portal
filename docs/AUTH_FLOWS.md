# Authentication Flows - Complete Reference

This document provides exhaustive detail on every authentication pathway in agent-portal. There are **TWO COMPLETELY SEPARATE** authentication systems that share some infrastructure but serve different purposes.

---

## Table of Contents

1. [Overview](#overview)
2. [Web Browser Login](#1-web-browser-login)
3. [Device Flow (CLI Authentication)](#2-device-flow-cli-authentication)
4. [Shared Infrastructure](#shared-infrastructure)
5. [State Management](#state-management)
6. [Error Handling](#error-handling)
7. [Security Considerations](#security-considerations)

---

## Overview

| Authentication Type | Purpose | Initiator | Result | Primary Endpoint |
|---------------------|---------|-----------|--------|------------------|
| Web Browser Login | User accesses web dashboard | Browser | Session cookie | `/api/auth/google` |
| Device Flow | CLI tool gets API access | CLI binary | JWT token | `/api/auth/device/code` |

**Critical Rule**: These flows use different endpoints and should NEVER be confused. The OAuth callback distinguishes them by checking if the `state` parameter starts with `device:`.

---

## 1. Web Browser Login

### Purpose
Allow users to authenticate via their browser to access the web dashboard at `/dashboard`.

### Endpoints Involved

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/api/auth/google` | GET | Initiate OAuth flow |
| `/api/auth/google/callback` | GET | Handle OAuth callback |
| `/api/auth/dev-login` | GET | Dev mode auto-login |
| `/api/auth/me` | GET | Get current user info |
| `/api/auth/logout` | GET | Clear session |

### Production Flow (Google OAuth)

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         WEB BROWSER LOGIN - PRODUCTION                       │
└─────────────────────────────────────────────────────────────────────────────┘

Step 1: User clicks "Login" button in browser
        ↓
        Browser navigates to: GET /api/auth/google

Step 2: Backend handler (auth.rs:login)
        ├── Check: Is OAuth client configured?
        │   ├── YES → Continue to Step 3
        │   └── NO  → Redirect to /api/auth/dev-login (dev mode)
        ↓
        Generate OAuth URL with:
        ├── Random CSRF token (CsrfToken::new_random)
        ├── Scopes: openid, email, profile
        └── Redirect URI: /api/auth/google/callback

Step 3: Browser redirects to Google OAuth consent screen
        ├── URL: https://accounts.google.com/o/oauth2/v2/auth?...
        ├── Parameters:
        │   ├── client_id=<GOOGLE_CLIENT_ID>
        │   ├── redirect_uri=<GOOGLE_REDIRECT_URI>
        │   ├── scope=openid+email+profile
        │   ├── state=<random_csrf_token>  ← NOT prefixed with "device:"
        │   └── response_type=code
        ↓
        User sees Google login page, enters credentials

Step 4: User grants permission
        ↓
        Google redirects to: /api/auth/google/callback?code=<auth_code>&state=<csrf_token>

Step 5: Backend handler (auth.rs:callback)
        ├── Exchange auth code for access token with Google
        ├── Fetch user info from Google API (sub, email, name, picture)
        ├── Check email allowlist (if configured)
        │   └── FAIL → Redirect to /access-denied
        ├── Create or update user in database
        ├── Check if user is banned
        │   └── BANNED → Redirect to /banned?reason=<encoded_reason>
        │
        │   (Note: Callback also handles device flow, but web login
        │    uses random CSRF token that won't match "device:" prefix)
        ↓
        Set session cookie:
        ├── Name: "cc_session"
        ├── Value: <user_uuid>
        ├── Path: "/"
        ├── HttpOnly: true
        ├── Secure: true (false in dev mode)
        ├── SameSite: Lax
        └── Signed with app_state.cookie_key

Step 6: Redirect to /dashboard
        ↓
        SUCCESS: User is now logged in with valid session cookie
```

### Dev Mode Flow (No OAuth)

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         WEB BROWSER LOGIN - DEV MODE                         │
└─────────────────────────────────────────────────────────────────────────────┘

Step 1: User clicks "Login" button in browser
        ↓
        Browser navigates to: GET /api/auth/google

Step 2: Backend handler (auth.rs:login)
        ├── Check: Is OAuth client configured?
        │   └── NO (dev mode) → Redirect to /api/auth/dev-login
        ↓

Step 3: Backend handler (auth.rs:dev_login)
        ├── Query database for user with email "testing@testing.local"
        │   └── FAIL → Return 500 Internal Server Error
        ├── Check if user is banned
        │   └── BANNED → Redirect to /banned?reason=<encoded_reason>
        ↓
        Set session cookie (same as production)

Step 4: Redirect to /dashboard
        ↓
        SUCCESS: User is logged in as testing@testing.local
```

### Success Criteria
- User lands on `/dashboard`
- Valid `cc_session` cookie is set
- Cookie is signed and HttpOnly

### Failure Cases

| Failure | Cause | User Sees |
|---------|-------|-----------|
| OAuth client not configured (prod) | Missing GOOGLE_CLIENT_ID env | 503 Service Unavailable |
| Test user not found (dev) | Database not seeded | 500 Internal Server Error |
| Google OAuth denied | User clicked "Deny" | Google error page |
| Email not in allowlist | ALLOWED_EMAILS/ALLOWED_EMAIL_DOMAIN set | `/access-denied` page |
| User is banned | user.disabled = true | `/banned?reason=...` page |
| Token exchange failed | Network error to Google | 500 Internal Server Error |

### Code Locations

| File | Function | Purpose |
|------|----------|---------|
| `backend/src/handlers/auth.rs` | `login()` | Initiate OAuth |
| `backend/src/handlers/auth.rs` | `callback()` | Handle OAuth callback |
| `backend/src/handlers/auth.rs` | `dev_login()` | Dev mode auto-login |
| `backend/src/handlers/auth.rs` | `me()` | Get current user |
| `backend/src/handlers/auth.rs` | `logout()` | Clear session |
| `backend/src/handlers/auth.rs` | `check_email_allowed()` | Validate email allowlist |

---

## 2. Device Flow (CLI Authentication)

### Purpose
Allow the CLI tool (proxy binary) to authenticate without requiring a browser on the same machine. The user authorizes the CLI from any browser, potentially on a different device.

### Why Device Flow?
- CLI runs in terminal, can't open browser directly
- User might be SSH'd into a remote server
- Follows RFC 8628 (OAuth 2.0 Device Authorization Grant) pattern

### Endpoints Involved

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/api/auth/device/code` | POST | CLI requests device code |
| `/api/auth/device` | GET | User verification page |
| `/api/auth/device-login` | GET | Device-specific OAuth (if not logged in) |
| `/api/auth/device/approve` | POST | User approves device |
| `/api/auth/device/deny` | POST | User denies device |
| `/api/auth/device/poll` | POST | CLI polls for completion |
| `/api/auth/device/error` | GET | Error display page |

### Complete Production Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         DEVICE FLOW - COMPLETE SEQUENCE                      │
└─────────────────────────────────────────────────────────────────────────────┘

=== PHASE 1: CLI Requests Device Code ===

Step 1.1: User runs CLI command that requires authentication
          Example: claude-portal --backend-url wss://example.com
          ↓
          CLI checks for cached token in ~/.config/agent-portal/config.json
          ├── Token exists and valid → Use it, skip device flow
          └── No token or expired → Start device flow

Step 1.2: CLI sends POST /api/auth/device/code
          Request body (JSON):
          {
            "hostname": "users-macbook.local",      // gethostname()
            "working_directory": "/home/user/project"  // current working dir
          }

Step 1.3: Backend handler (device_flow.rs:device_code)
          ├── Check: Is device flow store configured?
          │   └── NO → Return 503 "Device flow not available"
          ├── Generate device_code: 32 random alphanumeric chars
          │   Example: "xK9mN2pQ8rT1vZ3wY5aB7cD0eF2gH4iJ"
          ├── Generate user_code: 6 chars formatted as XXX-XXX
          │   Example: "ABC-123"
          ├── Create DeviceFlowState:
          │   {
          │     device_code: "xK9mN2pQ8rT1vZ...",
          │     user_code: "ABC-123",
          │     user_id: None,
          │     access_token: None,
          │     expires_at: now + 15 minutes,
          │     status: Pending,
          │     hostname: Some("users-macbook.local"),
          │     working_directory: Some("/home/user/project")
          │   }
          └── Store in device_flow_store (in-memory HashMap)

Step 1.4: Backend returns response to CLI
          Response (JSON):
          {
            "device_code": "xK9mN2pQ8rT1vZ3wY5aB7cD0eF2gH4iJ",
            "user_code": "ABC-123",
            "verification_uri": "https://example.com/api/auth/device",
            "expires_in": 900,
            "interval": 5
          }

Step 1.5: CLI displays message to user
          ┌────────────────────────────────────────────────┐
          │ To authenticate, visit:                        │
          │ https://example.com/api/auth/device            │
          │                                                │
          │ And enter code: ABC-123                        │
          │                                                │
          │ Waiting for authorization...                   │
          └────────────────────────────────────────────────┘


=== PHASE 2: User Verifies in Browser ===

Step 2.1: User opens browser and navigates to verification_uri
          GET /api/auth/device
          OR
          GET /api/auth/device?user_code=ABC-123 (if code in URL)

Step 2.2: Backend handler (device_flow.rs:device_verify_page)
          ├── Check: Is user_code provided in query string?
          │   └── NO → Show HTML form to enter code manually
          │            (DEVICE_CODE_FORM_HTML constant)
          ├── Look up user_code in device_flow_store
          │   └── NOT FOUND or EXPIRED → Redirect to /api/auth/device/error?message=Invalid+or+expired+code
          ├── Extract hostname and working_directory from state
          ├── Check: Is user logged in? (session cookie)
          │   ├── YES → Show approval page (Step 2.4)
          │   └── NO  → Redirect to device-login (Step 2.3)
          ↓

Step 2.3: User not logged in - redirect to device-specific OAuth
          Redirect to: /api/auth/device-login?device_user_code=ABC-123

Step 2.3a: Backend handler (auth.rs:device_login)
           ├── Check: Is OAuth client configured?
           │   ├── YES → Generate OAuth URL with state="device:ABC-123"
           │   │         Redirect to Google OAuth
           │   └── NO (dev mode) → Auto-login as testing@testing.local
           │                       Set session cookie
           │                       Redirect to /api/auth/device?user_code=ABC-123
           ↓

Step 2.3b: (Production only) User completes Google OAuth
           Google redirects to: /api/auth/google/callback?code=...&state=device:ABC-123

Step 2.3c: Backend handler (auth.rs:callback)
           ├── Exchange code for token, fetch user info (same as web login)
           ├── Create/update user, check banned status
           ├── Check state parameter:
           │   └── state.starts_with("device:") → YES!
           │       Extract user_code: "ABC-123"
           ├── Set session cookie (user is now logged in)
           └── Redirect to /api/auth/device?user_code=ABC-123

Step 2.4: User is now logged in, show approval page
          Backend returns HTML (render_approval_page function):
          ┌────────────────────────────────────────────────────────────┐
          │              Authorize Device Access                        │
          │                                                            │
          │  A device is requesting access to your account:            │
          │                                                            │
          │  ┌──────────────────────────────────────────────────────┐  │
          │  │ Hostname: users-macbook.local                        │  │
          │  │ Directory: /home/user/project                        │  │
          │  │ Code: ABC-123                                        │  │
          │  └──────────────────────────────────────────────────────┘  │
          │                                                            │
          │  [  Deny  ]                    [  Approve  ]               │
          │                                                            │
          └────────────────────────────────────────────────────────────┘


=== PHASE 3: User Approves or Denies ===

Step 3a: User clicks "Approve"
         Browser sends: POST /api/auth/device/approve
         Request body: { "user_code": "ABC-123" }

Step 3a.1: Backend handler (device_flow.rs:device_approve)
           ├── Verify user is logged in (session cookie)
           │   └── NOT LOGGED IN → Return 401 Unauthorized
           ├── Parse user_id from session cookie
           ├── Look up user_code in device_flow_store
           │   └── NOT FOUND → Return 404 "Code not found"
           ├── Check state.status == Pending
           │   └── NOT PENDING → Return 400 "Already processed"
           ├── Create JWT token for CLI:
           │   {
           │     "sub": "<user_uuid>",
           │     "iat": <issued_at_timestamp>,
           │     "exp": <expiry_timestamp>  // 30 days
           │   }
           ├── Hash token and store in proxy_auth_tokens table
           ├── Update device flow state:
           │   {
           │     status: Complete,
           │     user_id: Some(<user_uuid>),
           │     access_token: Some("<jwt_token>")
           │   }
           └── Return success response

         --- OR ---

Step 3b: User clicks "Deny"
         Browser sends: POST /api/auth/device/deny
         Request body: { "user_code": "ABC-123" }

Step 3b.1: Backend handler (device_flow.rs:device_deny)
           ├── Verify user is logged in
           ├── Look up user_code in device_flow_store
           ├── Update state: status = Denied
           └── Return success response


=== PHASE 4: CLI Polls for Result ===

Step 4.1: CLI has been polling every 5 seconds since Step 1.5
          POST /api/auth/device/poll
          Request body: { "device_code": "xK9mN2pQ8rT1vZ3wY5aB7cD0eF2gH4iJ" }

Step 4.2: Backend handler (device_flow.rs:device_poll)
          ├── Look up device_code in device_flow_store
          │   └── NOT FOUND → Return 404
          ├── Check expiration
          │   └── EXPIRED → Update status to Expired, return "expired"
          ├── Check status:
          │   ├── Pending → Return { "status": "authorization_pending" }
          │   ├── Complete → Return { "status": "complete", "access_token": "...", "user_id": "...", "user_email": "..." }
          │   ├── Denied → Return { "status": "denied" }
          │   └── Expired → Return { "status": "expired" }

Step 4.3: CLI receives "complete" response
          {
            "status": "complete",
            "access_token": "eyJhbGciOiJIUzI1NiIs...",
            "user_id": "550e8400-e29b-41d4-a716-446655440000",
            "user_email": "user@example.com"
          }

Step 4.4: CLI stores credentials
          Write to ~/.config/agent-portal/config.json:
          {
            "backend_url": "wss://example.com",
            "auth_token": "eyJhbGciOiJIUzI1NiIs...",
            "user_id": "550e8400-e29b-41d4-a716-446655440000",
            "user_email": "user@example.com"
          }

Step 4.5: SUCCESS - CLI can now connect to backend with JWT token
          CLI uses auth_token in WebSocket connection header
```

### Dev Mode Flow

In dev mode, Step 2.3a short-circuits:

```
Step 2.3a (DEV MODE): Backend handler (auth.rs:device_login)
          ├── Check: Is OAuth client configured?
          │   └── NO (dev mode detected)
          ├── Query database for "testing@testing.local"
          ├── Check if user is banned
          │   └── BANNED → Redirect to /banned
          ├── Set session cookie
          └── Redirect to /api/auth/device?user_code=ABC-123

          (Skips Google OAuth entirely)
```

### User Code Format

- Format: `XXX-XXX` (e.g., "ABC-123")
- Characters: Uppercase alphanumeric only
- Generated by `generate_user_code()` in device_flow.rs
- Easy to read aloud and type manually

### Device Code Format

- Format: 32 random alphanumeric characters
- Example: `xK9mN2pQ8rT1vZ3wY5aB7cD0eF2gH4iJ`
- Never shown to user, only used by CLI
- Generated by `generate_device_code()` in device_flow.rs

### Success Criteria
- CLI receives valid JWT token
- Token stored in config file
- CLI can establish WebSocket connection to backend

### Failure Cases

| Phase | Failure | Cause | CLI/User Sees |
|-------|---------|-------|---------------|
| 1 | Device flow unavailable | Server in wrong mode | CLI: 503 error |
| 2 | Invalid code | Typo or expired | Browser: "Invalid or expired code" |
| 2 | Not logged in | No session | Browser: Redirect to OAuth |
| 3 | User denies | Clicked "Deny" | CLI: Poll returns "denied" |
| 4 | Timeout | User never approved | CLI: Poll returns "expired" after 15 min |
| 4 | Network error | Connection issues | CLI: HTTP error on poll |

### State Machine

```
                    ┌─────────┐
                    │ PENDING │ ← Initial state
                    └────┬────┘
                         │
         ┌───────────────┼───────────────┐
         │               │               │
         ▼               ▼               ▼
    ┌─────────┐    ┌──────────┐    ┌─────────┐
    │ COMPLETE│    │  DENIED  │    │ EXPIRED │
    └─────────┘    └──────────┘    └─────────┘
    (user approved) (user denied)  (15 min timeout)
```

### Code Locations

| File | Function | Purpose |
|------|----------|---------|
| `backend/src/handlers/device_flow.rs` | `device_code()` | Generate device/user codes |
| `backend/src/handlers/device_flow.rs` | `device_verify_page()` | Show verification UI |
| `backend/src/handlers/device_flow.rs` | `device_approve()` | Handle approval |
| `backend/src/handlers/device_flow.rs` | `device_deny()` | Handle denial |
| `backend/src/handlers/device_flow.rs` | `device_poll()` | CLI polling endpoint |
| `backend/src/handlers/device_flow.rs` | `render_approval_page()` | Generate approval HTML |
| `backend/src/handlers/device_flow.rs` | `generate_user_code()` | Create XXX-XXX code |
| `backend/src/handlers/device_flow.rs` | `generate_device_code()` | Create 32-char code |
| `backend/src/handlers/auth.rs` | `device_login()` | Device-specific OAuth |
| `backend/src/jwt.rs` | `create_proxy_token()` | Generate JWT |
| `portal-auth/src/lib.rs` | `device_flow_login()` | CLI device flow logic (shared crate) |

---

## Shared Infrastructure

**Important**: Both flows share a single OAuth callback endpoint (`/api/auth/google/callback`). The callback uses the `state` parameter to determine which flow initiated the request:
- Random CSRF token → Web login → Redirect to `/dashboard`
- `device:XXX-XXX` → Device flow → Redirect to `/api/auth/device?user_code=XXX-XXX`

### Session Cookie

Both flows use the same session cookie format:

```rust
Cookie {
    name: "cc_session",
    value: "<user_uuid>",
    path: "/",
    http_only: true,
    secure: !dev_mode,  // true in production
    same_site: SameSite::Lax,
    signed: true  // Using app_state.cookie_key
}
```

### Database Tables

| Table | Used By | Purpose |
|-------|---------|---------|
| `users` | Both | Store user info (google_id, email, etc.) |
| `proxy_auth_tokens` | Device Flow | Store hashed JWT tokens |

### In-Memory Stores

| Store | Type | Purpose |
|-------|------|---------|
| `device_flow_store` | `Arc<RwLock<HashMap<String, DeviceFlowState>>>` | Active device flow sessions |

---

## State Management

### How OAuth Callback Distinguishes Flows

The OAuth callback (`/api/auth/google/callback`) receives a `state` parameter from Google. This is how it knows which flow to complete:

```rust
// In auth.rs:callback()

if let Some(ref state) = query.state {
    if let Some(device_user_code) = state.strip_prefix("device:") {
        // This is a device flow!
        // state = "device:ABC-123"
        // device_user_code = "ABC-123"
        // → Redirect to /api/auth/device?user_code=ABC-123
    }
}
// Otherwise, this is a regular web login
// → Redirect to /dashboard
```

### State Parameter Values

| Flow | State Value | Example |
|------|-------------|---------|
| Web Login | Random CSRF token | `f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2` |
| Device Flow | `device:` + user_code | `device:ABC-123` |

**CRITICAL**: The `device:` prefix is what distinguishes the flows. Regular OAuth CSRF tokens are long random strings that will never start with `device:`.

---

## Error Handling

### Error Pages

| URL | When Shown |
|-----|------------|
| `/access-denied` | Email not in allowlist |
| `/banned?reason=...` | User is disabled |
| `/api/auth/device/error?message=...` | Invalid/expired device code |

### HTTP Status Codes

| Code | Meaning |
|------|---------|
| 200 | Success |
| 302 | Redirect (most auth responses) |
| 400 | Bad request (invalid input) |
| 401 | Unauthorized (not logged in) |
| 404 | Not found (invalid code) |
| 500 | Internal server error |
| 503 | Service unavailable (OAuth/device flow not configured) |

---

## Security Considerations

### Cookie Security

- **HttpOnly**: Prevents JavaScript access (XSS protection)
- **Secure**: Only sent over HTTPS (disabled in dev mode)
- **SameSite=Lax**: Protects against CSRF while allowing top-level navigation
- **Signed**: Tamper-proof using server secret

### Token Security

- **JWT tokens** for device flow are signed with server secret
- **Token hash** stored in database, not the actual token
- **Expiration**: 30 days for proxy tokens

### Device Flow Security

- **User codes** expire after 15 minutes
- **Hostname/directory** shown to user for verification
- **Explicit approval** required (not automatic)
- **Denial option** available

### OAuth Security

- **CSRF token** in state parameter (random for web, prefixed for device)
- **PKCE** could be added for additional security (not currently implemented)

---

## Quick Reference

### Web Login Endpoints

```
GET  /api/auth/google           → Start OAuth
GET  /api/auth/google/callback  → OAuth callback → /dashboard
GET  /api/auth/dev-login        → Dev mode login → /dashboard
GET  /api/auth/me               → Get current user
GET  /api/auth/logout           → Clear session
```

### Device Flow Endpoints

```
POST /api/auth/device/code      → Get device code
GET  /api/auth/device           → Verification page
GET  /api/auth/device-login     → Device OAuth (if not logged in)
POST /api/auth/device/approve   → Approve device
POST /api/auth/device/deny      → Deny device
POST /api/auth/device/poll      → Poll for result
GET  /api/auth/device/error     → Error page
```

### Environment Variables

| Variable | Required | Purpose |
|----------|----------|---------|
| `GOOGLE_CLIENT_ID` | Production | OAuth client ID |
| `GOOGLE_CLIENT_SECRET` | Production | OAuth client secret |
| `GOOGLE_REDIRECT_URI` | Production | OAuth callback URL |
| `SESSION_SECRET` | Production | Cookie signing key |
| `ALLOWED_EMAIL_DOMAIN` | Optional | Restrict to email domain |
| `ALLOWED_EMAILS` | Optional | Restrict to specific emails |
| `--dev-mode` (CLI flag) | Development | Enable dev mode (not an env var) |
