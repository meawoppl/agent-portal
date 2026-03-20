# Proxy Authentication Guide

The proxy CLI authenticates users through an OAuth device flow, similar to how `gh` (GitHub CLI) or `gcloud` work.

## How It Works

1. **First Run**: When you run `claude-portal` for the first time in a directory, it will:
   - Display a link and verification code
   - Wait for you to authenticate in your browser
   - Store the auth token in `~/.config/claude-code-portal/config.json`

2. **Subsequent Runs**: The proxy automatically uses the cached authentication

3. **Per-Directory Auth**: Each working directory can be associated with a different user account

## Usage Examples

### First Time Setup

```bash
$ cd ~/my-project
$ claude-portal

╔═══════════════════════════════════════════════════════╗
║           🔐 Authentication Required                  ║
╚═══════════════════════════════════════════════════════╝

  To authenticate this machine, please visit:

    http://localhost:3000/auth/device

  And enter the code:

    ABC-123

  ⏳ Waiting for authentication...
```

**In your browser:**
1. Open http://localhost:3000/auth/device
2. Enter code `ABC-123`
3. Sign in with Google

**Back in terminal:**
```
  ✓ Authentication successful!
  Logged in as: user@example.com

  Session registered with backend
  Claude CLI process spawned
```

### Subsequent Runs

```bash
$ cd ~/my-project
$ claude-portal
# Uses cached auth - no prompt!
```

### Different Projects, Different Accounts

```bash
# Work project - authenticated as work@company.com
$ cd ~/work-project
$ claude-portal
# Uses work account

# Personal project - authenticated as personal@gmail.com
$ cd ~/personal-project
$ claude-portal --reauth  # Force new authentication
# Now uses personal account
```

### Config File Location

The config file is stored at:
- **Linux/Mac**: `~/.config/claude-code-portal/config.json`
- **Windows**: `%APPDATA%\claude-code-portal\config.json`

### Config File Format

```json
{
  "sessions": {
    "/home/user/project1": {
      "user_id": "uuid-here",
      "auth_token": "ccp_token_here",
      "user_email": "user@example.com",
      "last_used": "2026-01-08T12:34:56Z"
    },
    "/home/user/project2": {
      "user_id": "another-uuid",
      "auth_token": "ccp_another_token",
      "user_email": "other@example.com",
      "last_used": "2026-01-07T10:00:00Z"
    }
  },
  "preferences": {
    "default_backend_url": null,
    "auto_open_browser": false
  }
}
```

## CLI Flags

```bash
# Use a specific backend URL
claude-portal --backend-url wss://my-server.com

# Force re-authentication
claude-portal --reauth

# Logout (remove cached auth for this directory)
claude-portal --logout

# Provide auth token directly (skips OAuth)
claude-portal --auth-token ccp_your_token_here

# Custom session name
claude-portal --session-name "my-machine"
```

## Troubleshooting

### "Authentication timed out"

The verification code expires after 5 minutes. Just run `claude-portal` again to get a new code.

### "Authentication was denied"

You clicked "Deny" in the browser. Run `claude-portal --reauth` to try again.

### "Failed to connect to backend"

Check that the backend server is running:
```bash
curl http://localhost:3000/
```

### Switch accounts for a directory

```bash
# Remove cached auth
claude-portal --logout

# Run again to authenticate with a different account
claude-portal
```

### View config file

```bash
# Linux/Mac
cat ~/.config/claude-code-portal/config.json | jq
```

### Manually edit config

```bash
# Open in editor
vim ~/.config/claude-code-portal/config.json
```

## Security Considerations

1. **Token Storage**: Tokens are stored in plaintext in your config file. Make sure your home directory has appropriate permissions (700 on Unix).

2. **Token Scope**: Each token is tied to a specific user and can access all their sessions on the backend.

3. **Token Rotation**: Currently tokens don't expire. In production, implement token expiration and refresh.

4. **Logout**: Always use `--logout` when you're done with a project to remove cached credentials.

5. **Shared Machines**: On shared machines, be aware that anyone with access to your home directory can read your tokens.

## Development Mode

For testing without OAuth:

```bash
# Backend with dev mode
DEV_MODE=true cargo run -p backend -- --dev-mode

# Proxy with explicit token (skips OAuth)
claude-portal --auth-token dev_token_12345
```

In dev mode, all sessions are associated with `testing@testing.local`.
