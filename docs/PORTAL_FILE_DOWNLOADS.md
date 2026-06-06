# Portal File Downloads

Agent sessions can offer files to the user with markdown links that use the
`portal://file/` scheme:

```md
[Download the report](portal://file/reports/final.pdf)
```

The path is interpreted as a relative path inside the agent session's working
directory. The browser does not resolve `portal://` directly. The frontend
recognizes the scheme and renders a normal authenticated download link to the
backend:

```text
GET /api/sessions/{session_id}/files/pull?path=reports/final.pdf
```

On click, the backend verifies the user can read the session, sends a bounded
file request to the currently connected proxy, waits for the proxy response,
and streams the bytes back to the browser with `Content-Disposition:
attachment`. The backend does not write the file to disk.

## Security Model

- Only users with session access can request a pull.
- The request is only sent to the proxy currently registered for that session.
- Proxy-side path resolution rejects absolute paths, parent-directory escapes,
  and paths that canonicalize outside the session working directory.
- Downloads are size-limited before and after reading.
- Missing files, disconnected proxies, and unauthorized access are reported as
  generic failures where practical; the browser never talks to the proxy
  directly.
- The backend response is `Cache-Control: private, no-store`.

## Agent Hint

The portal reminder tells agents:

```md
To offer a generated file to the user, create it under the current working
directory and include a markdown link using `portal://file/relative/path.ext`.
Only use relative paths inside the workspace.
```

## Future Work

This first version is intentionally transient and in-memory. If downloads need
to survive disconnected proxies or old transcript replay, add an artifact store
backed by disk or object storage and keep metadata in Postgres.
