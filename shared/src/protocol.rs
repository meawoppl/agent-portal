/// Session cookie name used for web client authentication.
/// Shared between all backend handlers that read or write the session cookie.
pub const SESSION_COOKIE_NAME: &str = "cc_session";

/// Maximum number of messages to queue per session when the proxy is disconnected.
pub const MAX_PENDING_MESSAGES_PER_SESSION: usize = 100;

/// Maximum age (in seconds) of pending messages before they are dropped.
pub const MAX_PENDING_MESSAGE_AGE_SECS: u64 = 300;

/// Device authorization code lifetime in seconds (5 minutes).
pub const DEVICE_CODE_EXPIRES_SECS: u64 = 300;

/// Maximum reconnection backoff for proxies and launchers (in seconds).
/// Used by proxy/launcher to cap exponential backoff, and by the backend
/// to determine how long to wait before cleaning up stale sessions.
pub const MAX_RECONNECT_BACKOFF_SECS: u64 = 30;

// --- File upload chunking (ClientToServer::FileUploadStart/Chunk) ---
//
// The frontend slices files into chunks of `UPLOAD_CHUNK_SIZE` decoded
// bytes; the backend validates every upload against `MAX_UPLOAD_CHUNK_BYTES`
// and `MAX_UPLOAD_TOTAL_CHUNKS`. Any sender must satisfy the validator:
// chunk size ≤ `MAX_UPLOAD_CHUNK_BYTES` and chunk count ≤
// `MAX_UPLOAD_TOTAL_CHUNKS`.

/// Decoded byte size of each chunk the frontend sends when uploading a
/// file. Must be ≤ `MAX_UPLOAD_CHUNK_BYTES` or the backend rejects the
/// chunks.
pub const UPLOAD_CHUNK_SIZE: usize = 1024;

/// Per-chunk decoded byte cap enforced by the backend after
/// base64-decoding each chunk, so the server's total-byte budget can't be
/// bypassed by a client sending arbitrarily-large chunks (see #785).
pub const MAX_UPLOAD_CHUNK_BYTES: usize = 64 * 1024; // 64 KiB

/// Hard ceiling on the number of chunks per upload, enforced by the
/// backend at `FileUploadStart` time. With `MAX_UPLOAD_CHUNK_BYTES = 64
/// KiB` this caps any single upload at 4 GiB, but the **binding** limit
/// is the per-server `PORTAL_MAX_IMAGE_MB` byte cap — this constant only
/// stops obviously-pathological `total_chunks` values from being accepted.
pub const MAX_UPLOAD_TOTAL_CHUNKS: u32 = 65_536;

/// Interval (in seconds) between launcher heartbeats on the launcher
/// WebSocket. The backend has no explicit heartbeat timeout — launcher
/// liveness is the WebSocket connection itself — but it treats each
/// heartbeat's `running_sessions` as the authoritative process list when
/// reconciling desired sessions (see `LauncherToServer::LauncherHeartbeat`
/// handling in the backend's launcher socket).
pub const LAUNCHER_HEARTBEAT_INTERVAL_SECS: u64 = 30;
