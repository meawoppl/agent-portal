# VS Code Extension Setup

Route Claude Code extension sessions through the portal so they appear on the dashboard.

## Prerequisites

- `claude-portal` binary installed (from [GitHub Releases](https://github.com/meawoppl/agent-portal/releases/latest) or built from source)
- Portal backend running (locally or remotely)

## Setup

### 1. Authenticate (one-time per machine)

Run from a terminal — the device flow requires interactive approval:

```bash
claude-portal --backend-url wss://your-portal-server.example.com
```

For local development:
```bash
claude-portal --backend-url ws://localhost:3000
```

This will:
1. Display a verification URL
2. Prompt you to visit the URL in your browser
3. Show an approval page (auto-approved in dev mode)
4. Store the auth token locally

Once authenticated, all future sessions (terminal and VS Code) reuse the stored token.

**Important**: The auth token is stored per-working-directory. The shim uses a cross-directory fallback, so authenticating from *any* directory is sufficient for VS Code sessions to work.

**Config location on macOS**: `~/Library/Application Support/com.anthropic.agent-portal/config.json` (not `~/.config/`). The proxy uses the `directories` crate which follows OS-standard paths.

### 2. Create the shim script

```bash
cat > ~/.claude/claude-portal-shim.sh << 'EOF'
#!/bin/bash
exec claude-portal --shim --backend-url wss://your-portal-server.example.com -- "$@"
EOF
chmod +x ~/.claude/claude-portal-shim.sh
```

For local development, use `ws://localhost:3000` instead.

### 3. Configure VS Code

Add to your VS Code `settings.json` (Cmd+Shift+P > "Open User Settings (JSON)"):

```json
"claudeCode.claudeProcessWrapper": "/Users/<you>/.claude/claude-portal-shim.sh"
```

### 4. Restart VS Code

Cmd+Q and reopen. Claude sessions will now appear on the portal dashboard.

## How It Works

```
VS Code Extension
    -> launches claude-portal-shim.sh (instead of claude directly)
    -> claude-portal --shim spawns the real claude binary
    -> stdin/stdout pass through transparently to VS Code
    -> output is also forwarded to portal backend via WebSocket
```

- Claude works exactly as before from VS Code's perspective
- The shim is transparent — same stdin/stdout JSON protocol
- All tracing/diagnostic output goes to stderr (never contaminates JSON on stdout)
- If the portal backend is down, Claude still works (graceful degradation)

### Authentication in shim mode

The shim **never** triggers interactive device flow authentication. This is by design — the device flow requires a browser and would block Claude from starting.

Instead, the shim resolves auth in this order:
1. Cached token for the current working directory
2. **Cross-directory fallback**: most recently used token from any directory
3. No token (session may still register in dev mode)

If no cached token is found, the shim logs a warning to stderr and continues without portal auth. To fix, run the interactive auth from a terminal (Step 1 above).

## Troubleshooting

### "CLI output was not valid JSON"

The portal binary is printing non-JSON to stdout. Ensure you have the latest binary with tracing directed to stderr:

```bash
# Rebuild from source
cd /path/to/agent-portal
cargo build --release -p claude-portal
cp target/release/claude-portal ~/.claude/claude-portal
```

### "Claude Code process exited with code 1"

The shim crashed before launching claude. Common causes:

| Cause | Fix |
|-------|-----|
| Auth failed | Run `claude-portal --backend-url wss://your-server` in a terminal to authenticate interactively |
| Backend unreachable | Check that the portal server is running |

### Session not showing on dashboard

The shim launched claude but couldn't register with the portal. Check:

1. **Auth token exists**: Check the config file for `auth_token` entries:
   - macOS: `cat ~/Library/Application\ Support/com.anthropic.agent-portal/config.json`
   - Linux: `cat ~/.config/agent-portal/config.json`
2. **Backend is reachable**: `curl https://your-portal-server/` should return HTML
3. **WebSocket connects**: Check stderr output (visible in VS Code output panel > Claude Code)

If no auth token, run the interactive auth from a terminal (Step 1 above).

## Removing the Integration

1. Remove the shim setting from VS Code:
   ```
   Delete "claudeCode.claudeProcessWrapper" from settings.json
   ```
2. Optionally delete the shim script:
   ```bash
   rm ~/.claude/claude-portal-shim.sh
   ```

Claude will launch directly without the portal.
