# Transport compatibility

Portal's transport major is `shared::protocol::PROTOCOL_VERSION`. It is
independent of the application release. Missing version metadata means the
original **version 1**, never the newest version. Additive feature support still
uses the existing capability flags; a matching major does not enable every
feature.

Proxies and launchers advertise `protocol_version` during registration. The
server rejects a different major before registering the connection or creating
a session. Rejection acknowledgements include actionable update guidance and
are non-retryable for a session proxy; a launcher parks on its existing slow
retry schedule. Clients also validate the version on successful server
acknowledgements, so a success flag cannot override incompatible metadata.
Legacy acknowledgements without metadata remain supported as version 1.

Browser connections fetch `/api/config` through the same PortalClient that owns
the socket, including the credentials and origin for a linked account. A
successful response with no `protocol_version` identifies a legacy server.
Otherwise the browser validates the advertised major, sends `Hello`, and waits
for a successful `HelloAck` before exposing any dashboard or session mutation
sender. Read-only seed snapshots arriving first are delivered normally, not
lost. A failed config fetch, negative acknowledgement, closed connection or
10-second acknowledgement timeout fails closed; none triggers a legacy fallback
or automatic mutation replay. The user sees the connection error and can
reconnect/update.

The server handles Hello before dashboard dispatch and rejects incompatible
session Register frames too. It closes the socket after sending a rejection,
so subsequent pipelined commands are discarded. Legacy clients may still start
with their original frame without Hello; supporting them is deliberate and is
not an authentication boundary. Existing account, session-role and linked-token
permission checks remain authoritative.

When introducing an incompatible major, update the shared constant and preserve
explicit supported-version rules rather than treating absent metadata as
current. Add wire goldens and real socket skew tests before changing the
contract. The harness covers legacy/current registration, incompatible proxy
registration without session creation, fatal launcher rejection, browser
Hello acceptance/rejection and pipelined commands. Client-side proxy tests use a
real minimal WebSocket peer with legacy/current/incompatible success acks.
