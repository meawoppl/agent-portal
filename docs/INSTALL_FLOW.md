# Proxy Install Flow

This document explains how the `curl | bash` installation and initialization flow works.

## Overview

```
┌─────────────┐    1. Request token    ┌─────────────┐
│   Frontend  │ ────────────────────▶  │   Backend   │
│  (browser)  │ ◀──────────────────── │             │
└─────────────┘    2. Return token +   └─────────────┘
      │               init_url              │
      │                                     │
      ▼ 3. User copies curl command         │
┌─────────────┐                             │
│  Terminal   │ ────────────────────────────┘
│             │    4. Download install.sh
│             │    5. Download binary
│             │    6. Run --init with URL
└─────────────┘
```

## Step-by-Step Flow

### 1. Frontend Requests a Token

When user clicks "Setup" in the UI, the frontend calls:
```
POST /api/proxy-tokens
{
  "name": "CLI Setup - 1/10/2026",
  "expires_in_days": 30
}
```

### 2. Backend Creates Token and Init URL

The backend:
1. Generates a JWT token with user info and expiry
2. Stores the token hash in `proxy_auth_tokens` table
3. Creates a `ProxyInitConfig` containing the JWT
4. Base64-encodes the config
5. Returns the init URL

**Example response:**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
  "init_url": "http://localhost:3000/p/eyJ0IjoiZXlKaGJHY2lPaUpJVXpJMU5pSXNJblI1Y0NJNklrcFhWQ0o5Li4uIn0",
  "expires_at": "2026-02-09T22:35:51Z"
}
```

**The init_url structure:**
```
http://localhost:3000/p/{base64url_encoded_config}
                     ▲
                     │
            Path prefix "/p/" indicates
            this is a proxy init config
```

**The config (before base64 encoding):**
```json
{
  "t": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",  // JWT token
  "n": "optional-session-prefix"                    // optional
}
```

### 3. Frontend Displays Curl Command

The frontend URL-encodes the init_url and embeds it in the install script URL:

```bash
curl -fsSL "http://localhost:3000/api/download/install.sh?init_url=http%3A%2F%2Flocalhost%3A3000%2Fp%2FeyJ0Ijoi..." | bash
```

### 4. Install Script Downloads and Runs

When the user runs the curl command, the backend generates a bash script that:

1. **Downloads the binary** to `~/.config/agent-portal/claude-portal`
2. **Makes it executable**
3. **Adds to PATH** in `.bashrc`/`.zshrc`/`.profile`
4. **Runs initialization** (if init_url was provided):
   ```bash
   "${BIN_PATH}" --init "http://localhost:3000/p/eyJ0Ijoi..."
   ```

### 5. Proxy Parses the Init URL

When `claude-portal --init <url>` runs, it:

1. **Parses the URL** (`proxy/src/util.rs:parse_init_url`)
2. **Extracts the backend URL** (converts `http://` to `ws://` for WebSocket)
3. **Decodes the base64 config** from the `/p/{config}` path
4. **Extracts the JWT token** from the config
5. **Saves to config file** (`~/.config/agent-portal/agent-portal/config.json`):
   ```json
   {
     "sessions": {
       "/home/user/project": {
         "user_id": "",
         "auth_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
         "user_email": "user@example.com",
         "last_used": "2026-01-10T22:35:51Z",
         "backend_url": "ws://localhost:3000"
       }
     }
   }
   ```

### 6. Ready to Use

Now the user can simply run:
```bash
claude-portal
```

The proxy will:
1. Load the saved token from config
2. Connect to the backend via WebSocket
3. Start the Claude CLI session

## Data Flow Diagram

```
┌────────────────────────────────────────────────────────────────────┐
│                         init_url                                    │
│  http://localhost:3000/p/eyJ0IjoiZXlKaGJHY2lPaUpJVXpJMU5pSXNJ...   │
└────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────────────┐
│                    URL Structure                                    │
│  ┌──────────────────────┐ ┌──┐ ┌────────────────────────────────┐  │
│  │ http://localhost:3000│ │/p│ │ base64url(ProxyInitConfig)     │  │
│  └──────────────────────┘ └──┘ └────────────────────────────────┘  │
│         Backend URL       Path         Encoded Config              │
└────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────────────┐
│                    ProxyInitConfig (decoded)                        │
│  {                                                                  │
│    "t": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJqdGkiOiI1NTB...", │
│    "n": null  // optional session name prefix                       │
│  }                                                                  │
└────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────────────┐
│                    JWT Token (decoded payload)                      │
│  {                                                                  │
│    "jti": "550e8400-e29b-41d4-a716-446655440000",  // token ID     │
│    "sub": "123e4567-e89b-12d3-a456-426614174000",  // user ID      │
│    "email": "user@example.com",                                     │
│    "iat": 1736548551,  // issued at                                │
│    "exp": 1739140551   // expires at                               │
│  }                                                                  │
└────────────────────────────────────────────────────────────────────┘
```

## Security Considerations

1. **Token is only shown once** - The JWT is only returned at creation time
2. **Token hash stored in DB** - Only the hash is stored, not the actual token
3. **Token expiry** - Tokens expire after the configured number of days (default: 30)
4. **Token revocation** - Tokens can be revoked via the `proxy_auth_tokens` table
5. **HTTPS in production** - The init URL should use HTTPS in production

## Files Involved

| File | Purpose |
|------|---------|
| `shared/src/proxy_tokens.rs` | `ProxyInitConfig` struct and base64 encode/decode |
| `backend/src/handlers/proxy_tokens.rs` | Creates JWT and builds init_url |
| `backend/src/handlers/downloads.rs` | Generates install.sh with embedded init_url |
| `proxy/src/util.rs` | Parses init_url and extracts token |
| `proxy/src/commands.rs` | `handle_init()` saves config |
| `proxy/src/config.rs` | Config file management |
| `frontend/src/components/proxy_token_setup.rs` | Displays curl command |
