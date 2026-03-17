# Usage Guide

This guide covers how to use Claude Code Portal once it's set up.

## Web Interface

1. Open the portal URL in your browser
2. Sign in with Google
3. View your active Claude Code sessions
4. Click any session to interact with Claude
5. Use the microphone button or `Ctrl+M` for voice input

### Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl+M` | Toggle voice recording |
| `Enter` | Send message |
| `Escape` | Cancel current action |

### Session Management

- **Active sessions** show a green indicator
- **Disconnected sessions** are greyed out but remain accessible for history
- **Hidden sessions** are dimmed and excluded from rotation
- Click the hide button on any session to toggle hidden state

## Running the CLI

On your development machine, run the `claude-portal` binary to connect to the portal:

```bash
claude-portal \
  --backend-url wss://your-portal.com \
  --session-name "my-dev-machine"
```

### First Run Authentication

On first run, the CLI displays a verification URL and code:

```
To authenticate, visit: https://your-portal.com/api/auth/device
Enter code: ABC-123
```

Open the URL in your browser, sign in with Google, and enter the code. Credentials are cached in `~/.config/claude-code-portal/config.json`.

### CLI Options

```bash
claude-portal [OPTIONS] -- [CLAUDE_ARGS]

Options:
  --backend-url <URL>     Backend WebSocket URL [default: wss://txcl.io in release]
  --session-name <NAME>   Session name [default: hostname-timestamp]
  --auth-token <TOKEN>    Authentication token (skips OAuth flow)
  --init <TOKEN_URL>      Initialize with setup token from web UI
  --reauth                Force re-authentication
  --logout                Remove cached credentials and exit
  --new-session           Start fresh session (don't resume previous)
  --dev                   Development mode (bypass auth)
  --shim                  Shim mode for VS Code extension
  --agent <AGENT>         Agent CLI to use: "claude" (default) or "codex"
  --no-update             Skip automatic update check
  --update                Force update from GitHub releases
  --check-update          Check for updates without installing
  -v, --verbose           Enable debug-level logging

# All arguments after -- are forwarded to the agent CLI
```

### Examples

```bash
# Connect to production portal
claude-portal --backend-url wss://txcl.io

# Use a custom session name
claude-portal --backend-url wss://txcl.io --session-name "gpu-workstation"

# Force re-authentication
claude-portal --backend-url wss://txcl.io --reauth

# Clear cached credentials
claude-portal --logout

# Pass arguments to claude CLI
claude-portal --backend-url wss://txcl.io -- --model claude-3-opus
```

## Voice Commands

The web interface supports voice input for hands-free coding:

1. Click the microphone icon or press `Ctrl+M` to start recording
2. Speak your command naturally
3. Click again or press `Ctrl+M` to stop and send

### Browser Support

Voice input works in browsers with Web Speech API support:
- Chrome (recommended)
- Edge
- Safari
- Firefox (limited support)

### Tips for Voice Input

- Speak clearly and at a natural pace
- The transcript appears in real-time as you speak
- You can edit the transcribed text before sending
- Works best in quiet environments

## Session Sharing

Share sessions with team members for collaborative coding:

1. Click the share icon on any session you own
2. Enter the email address of the person to share with
3. Choose their role:
   - **Editor**: Can send messages and interact with Claude
   - **Viewer**: Can only watch the session
4. The shared user will see the session in their dashboard

### Managing Shared Sessions

- **Owners** can add/remove members and change roles
- **Editors** can interact but cannot manage sharing
- **Viewers** have read-only access
- Click "Leave" on a shared session to remove yourself

## Tips and Best Practices

### Session Naming

Use descriptive session names to easily identify sessions:
```bash
# Good session names
claude-portal --session-name "gpu-ml-training"
claude-portal --session-name "frontend-dev"
claude-portal --session-name "raspberry-pi"

# Default uses hostname + timestamp
claude-portal  # Results in: my-laptop-20260118-143022
```

### Multiple Sessions

You can run multiple `claude-portal` instances on the same machine:
```bash
# Terminal 1
claude-portal --session-name "project-a"

# Terminal 2
claude-portal --session-name "project-b"
```

### Long-Running Tasks

For long-running tasks:
1. Start the task in your session
2. Close your browser - the session continues running
3. Check back later from any device
4. All history is preserved

### Working Directory

The portal displays the working directory where `claude-portal` was started. Run it from your project root for clear context:
```bash
cd ~/projects/my-app
claude-portal --backend-url wss://txcl.io
```
