# Database Schema Reference

This document describes the PostgreSQL database schema used by the backend. The schema is managed by Diesel migrations in `backend/migrations/`.

## Tables

### `users`

Stores authenticated users. Created on first OAuth login.

| Column | Type | Nullable | Description |
|---|---|---|---|
| `id` | UUID (PK) | No | User ID |
| `google_id` | VARCHAR(255) | No | Google OAuth subject ID |
| `email` | VARCHAR(255) | No | User's email address |
| `name` | VARCHAR(255) | Yes | Display name |
| `avatar_url` | TEXT | Yes | Profile picture URL |
| `created_at` | TIMESTAMP | No | Account creation time |
| `updated_at` | TIMESTAMP | No | Last profile update |
| `is_admin` | BOOL | No | Admin privileges flag |
| `disabled` | BOOL | No | Account disabled flag |
| `voice_enabled` | BOOL | No | Voice input access flag |
| `ban_reason` | TEXT | Yes | Reason for ban (if disabled) |
| `sound_config` | JSONB | Yes | Per-user sound notification settings |

### `sessions`

Stores Claude Code proxy sessions. Each session maps to one claude CLI instance.

| Column | Type | Nullable | Description |
|---|---|---|---|
| `id` | UUID (PK) | No | Session ID (matches Claude's session ID) |
| `user_id` | UUID (FK → users) | No | Session owner |
| `session_name` | VARCHAR(255) | No | Human-readable name |
| `session_key` | VARCHAR(255) | No | WebSocket routing key |
| `working_directory` | TEXT | No | Filesystem path where session was started |
| `status` | VARCHAR(50) | No | `active`, `inactive`, or `disconnected` |
| `last_activity` | TIMESTAMP | No | Last message timestamp |
| `created_at` | TIMESTAMP | No | Session creation time |
| `updated_at` | TIMESTAMP | No | Last metadata update |
| `git_branch` | VARCHAR(255) | Yes | Current git branch |
| `total_cost_usd` | FLOAT8 | No | Cumulative API cost |
| `input_tokens` | INT8 | No | Total input tokens used |
| `output_tokens` | INT8 | No | Total output tokens used |
| `cache_creation_tokens` | INT8 | No | Cache creation tokens |
| `cache_read_tokens` | INT8 | No | Cache read tokens |
| `client_version` | VARCHAR(32) | Yes | Proxy CLI version |
| `input_seq` | INT8 | No | Next input sequence number |
| `hostname` | VARCHAR(255) | No | Hostname of machine running the session |
| `launcher_id` | UUID | Yes | Launcher that started this session |
| `pr_url` | VARCHAR(512) | Yes | GitHub PR URL for current branch |
| `agent_type` | VARCHAR(16) | No | Agent CLI type (`claude` or `codex`) |

### `session_members`

Maps users to sessions with roles. Enables session sharing.

| Column | Type | Nullable | Description |
|---|---|---|---|
| `id` | UUID (PK) | No | Membership ID |
| `session_id` | UUID (FK → sessions) | No | Session |
| `user_id` | UUID (FK → users) | No | Member |
| `role` | VARCHAR(20) | No | `owner`, `editor`, or `viewer` |
| `created_at` | TIMESTAMP | No | When member was added |

**Unique constraint**: `(session_id, user_id)` - a user can only have one role per session.

### `messages`

Stores all Claude output messages for session history replay.

| Column | Type | Nullable | Description |
|---|---|---|---|
| `id` | UUID (PK) | No | Message ID |
| `session_id` | UUID (FK → sessions) | No | Parent session |
| `role` | VARCHAR(50) | No | Message role (from Claude's JSON `type` field) |
| `content` | TEXT | No | Full JSON content |
| `created_at` | TIMESTAMP | No | Storage timestamp |
| `user_id` | UUID (FK → users) | No | Session owner at time of storage |

**Index**: `idx_messages_session_created` on `(session_id, created_at)` for efficient history queries.

### `pending_inputs`

Stores user inputs that haven't been acknowledged by the proxy. Used for reliable input delivery across reconnections.

| Column | Type | Nullable | Description |
|---|---|---|---|
| `id` | UUID (PK) | No | Input ID |
| `session_id` | UUID (FK → sessions) | No | Target session |
| `seq_num` | INT8 | No | Sequence number |
| `content` | TEXT | No | JSON input content |
| `created_at` | TIMESTAMP | No | When input was stored |

Rows are deleted when the proxy sends an `InputAck` with `ack_seq >= seq_num`.

### `pending_permission_requests`

Stores permission requests waiting for user approval. Replayed to web clients on connection.

| Column | Type | Nullable | Description |
|---|---|---|---|
| `id` | UUID (PK) | No | Request ID |
| `session_id` | UUID (FK → sessions) | No | Session requesting permission |
| `request_id` | VARCHAR(255) | No | Correlation ID for request/response matching |
| `tool_name` | VARCHAR(255) | No | Tool requesting permission |
| `input` | JSONB | No | Tool input parameters |
| `permission_suggestions` | JSONB | Yes | Suggested permissions for "allow & remember" |
| `created_at` | TIMESTAMP | No | When request was stored |

Rows are deleted when a `PermissionResponse` is received.

### `proxy_auth_tokens`

Stores hashed JWT tokens for proxy CLI authentication.

| Column | Type | Nullable | Description |
|---|---|---|---|
| `id` | UUID (PK) | No | Token ID |
| `user_id` | UUID (FK → users) | No | Token owner |
| `name` | VARCHAR(255) | No | Token name/description |
| `token_hash` | VARCHAR(64) | No | SHA-256 hash of the token |
| `created_at` | TIMESTAMP | No | Token creation time |
| `last_used_at` | TIMESTAMP | Yes | Last authentication time |
| `expires_at` | TIMESTAMP | No | Token expiration time |
| `revoked` | BOOL | No | Whether token has been revoked |

### `deleted_session_costs`

Aggregates cost data from deleted sessions so user spend totals remain accurate.

| Column | Type | Nullable | Description |
|---|---|---|---|
| `id` | UUID (PK) | No | Record ID |
| `user_id` | UUID (FK → users) | No | User who owned the sessions |
| `cost_usd` | FLOAT8 | No | Total cost of deleted sessions |
| `session_count` | INT4 | No | Number of sessions aggregated |
| `created_at` | TIMESTAMPTZ | No | When aggregation was created |
| `updated_at` | TIMESTAMPTZ | No | Last update |
| `input_tokens` | INT8 | No | Total input tokens from deleted sessions |
| `output_tokens` | INT8 | No | Total output tokens from deleted sessions |
| `cache_creation_tokens` | INT8 | No | Cache creation tokens from deleted sessions |
| `cache_read_tokens` | INT8 | No | Cache read tokens from deleted sessions |

## Relationships

```
users ──┬── sessions ──┬── messages
        │              ├── session_members
        │              ├── pending_inputs
        │              └── pending_permission_requests
        ├── session_members
        ├── proxy_auth_tokens
        └── deleted_session_costs
```

All foreign keys reference `users.id` or `sessions.id`. Diesel's `joinable!` macro declarations in `schema.rs` define these relationships.

## Indexes

| Index | Table | Columns | Purpose |
|---|---|---|---|
| `idx_messages_session_created` | messages | (session_id, created_at) | History replay queries |
| `idx_sessions_user_id` | sessions | user_id | User's session list |
| `idx_sessions_status` | sessions | status | Active session filtering |
| `idx_sessions_last_activity` | sessions | last_activity | Recent session sorting |
| `idx_raw_message_log_session` | raw_message_log | session_id | Session message lookup |
| `idx_raw_message_log_created` | raw_message_log | created_at | Time-range queries |
| `idx_session_members_session` | session_members | session_id | Session member lookup |
| `idx_session_members_user` | session_members | user_id | User's shared sessions |
| `idx_pending_inputs_session` | pending_inputs | session_id | Pending input replay |

## Migration History

Migrations are in `backend/migrations/` and follow the naming convention `YYYY-MM-DD-HHMMSS_description`.

| Migration | Description |
|---|---|
| `00000000000000_initial_setup` | Creates users, sessions, messages tables |
| `2026-01-09-000000_add_proxy_auth_tokens` | Adds proxy_auth_tokens table |
| `2026-01-10-211719_add_user_id_to_messages` | Adds user_id FK to messages |
| `2026-01-14-160836_add_admin_fields_to_users` | Adds is_admin, disabled columns |
| `2026-01-14-185028_add_git_branch_to_sessions` | Adds git_branch to sessions |
| `2026-01-14-221102_add_cost_tracking` | Adds total_cost_usd to sessions |
| `2026-01-15-184305_add_deleted_session_costs` | Adds deleted_session_costs table |
| `2026-01-15-191412_add_token_tracking` | Adds token count columns to sessions |
| `2026-01-16-203452_add_voice_enabled_to_users` | Adds voice_enabled to users |
| `2026-01-16-210151_add_pending_permission_requests` | Adds pending_permission_requests table |
| `2026-01-17-030335_add_performance_indices` | Adds performance indexes |
| `2026-01-17-200839_add_raw_message_log` | Adds raw_message_log table |
| `2026-01-17-233608_add_session_members` | Adds session_members table |
| `2026-01-18-023257_add_ban_reason` | Adds ban_reason to users |
| `2026-01-18-045007_make_working_directory_not_null` | Makes working_directory NOT NULL |
| `2026-01-18-045238_add_client_version_to_sessions` | Adds client_version to sessions |
| `2026-01-18-205949_add_content_hash_to_raw_messages` | Adds content_hash dedup column |
| `2026-01-18-221922_add_messages_created_at_index` | Adds messages index |
| `2026-01-20-231629_add_pending_inputs_table` | Adds pending_inputs table |
| `2026-02-16-171025_add_hostname_and_launcher_id` | Adds hostname and launcher_id to sessions |
| `2026-02-18-222738_add_pr_url` | Adds pr_url to sessions |
| `2026-02-19-180649_add_sound_config_to_users` | Adds sound_config to users |
| `2026-02-21-204646_add_agent_type_to_sessions` | Adds agent_type to sessions |

## Working with the Schema

```bash
# Apply all pending migrations
cd backend && diesel migration run

# Create a new migration
cd backend && diesel migration generate add_new_feature

# Revert the last migration
cd backend && diesel migration revert
```

The `backend/src/schema.rs` file is auto-generated by Diesel and should never be edited manually. Run `diesel migration run` to regenerate it after adding or modifying migrations.
