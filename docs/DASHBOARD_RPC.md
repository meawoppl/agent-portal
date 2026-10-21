# Dashboard launcher requests

The browser uses the authenticated `/ws/client?account=...` connection for launcher lists, directory browsing, agent probes, session launches/imports, agent login/install, and launcher update/restart. `ClientWebSocketProvider` keeps one user connection per portal across route changes; components consume its live state instead of opening their own connections.

`ClientToServer::Dashboard` carries a UUID and a typed `DashboardRequest`. `ServerToClient::DashboardResult` returns the same UUID with a typed response or an HTTP-equivalent status/message. Backend adapters call the existing handlers, retaining their launcher ownership and capability checks. The REST endpoints remain supported for older bundles and CLI clients. Session-scoped requests, plugin operations, and credential management retain their existing endpoints.

Requests run concurrently so a slow install or login cannot block reads or incoming live notifications. Each socket and the browser broker allow at most 32 pending requests. Browser calls time out after 195 seconds; individual launcher operations retain their shorter server deadlines. Dropping a call removes its waiter, and late replies are ignored.

A lost connection fails pending calls with an explicit unknown-outcome error. Reconnection restores the channel and live updates but **does not replay mutations**: a launch or restart may already have reached its destination. The user should inspect the resulting state before retrying. Account changes discard pending work; an older connection's cleanup cannot close its replacement.

Linked credentials do not currently authorize host management via REST, so dashboard RPCs are also denied on linked sockets. Agent sign-in from a linked session directs the user to the remote portal's Computers page rather than accidentally addressing a local host.

Validation:

- `cargo test -p frontend --lib dashboard_rpc` checks out-of-order responses, disconnects, cancelled waiters, and stale-connection cleanup.
- `cargo test -p backend dashboard_rpc` checks owner isolation and independent requests against a fake launcher.
- After `cd frontend && trunk build`, run `uv run --with playwright python scripts/test-dashboard-rpc.py` for real WASM browser controls with a mocked socket. It asserts there is one user socket and no launcher REST calls.
