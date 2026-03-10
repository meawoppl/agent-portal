// @generated automatically by Diesel CLI.

diesel::table! {
    deleted_session_costs (id) {
        id -> Uuid,
        user_id -> Uuid,
        cost_usd -> Float8,
        session_count -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        input_tokens -> Int8,
        output_tokens -> Int8,
        cache_creation_tokens -> Int8,
        cache_read_tokens -> Int8,
    }
}

diesel::table! {
    messages (id) {
        id -> Uuid,
        session_id -> Uuid,
        #[max_length = 50]
        role -> Varchar,
        content -> Text,
        created_at -> Timestamp,
        user_id -> Uuid,
    }
}

diesel::table! {
    pending_inputs (id) {
        id -> Uuid,
        session_id -> Uuid,
        seq_num -> Int8,
        content -> Text,
        created_at -> Timestamp,
    }
}

diesel::table! {
    pending_permission_requests (id) {
        id -> Uuid,
        session_id -> Uuid,
        #[max_length = 255]
        request_id -> Varchar,
        #[max_length = 255]
        tool_name -> Varchar,
        input -> Jsonb,
        permission_suggestions -> Nullable<Jsonb>,
        created_at -> Timestamp,
    }
}

diesel::table! {
    proxy_auth_tokens (id) {
        id -> Uuid,
        user_id -> Uuid,
        #[max_length = 255]
        name -> Varchar,
        #[max_length = 64]
        token_hash -> Varchar,
        created_at -> Timestamp,
        last_used_at -> Nullable<Timestamp>,
        expires_at -> Timestamp,
        revoked -> Bool,
    }
}

diesel::table! {
    session_members (id) {
        id -> Uuid,
        session_id -> Uuid,
        user_id -> Uuid,
        #[max_length = 20]
        role -> Varchar,
        created_at -> Timestamp,
    }
}

diesel::table! {
    sessions (id) {
        id -> Uuid,
        user_id -> Uuid,
        #[max_length = 255]
        session_name -> Varchar,
        #[max_length = 255]
        session_key -> Varchar,
        working_directory -> Text,
        #[max_length = 50]
        status -> Varchar,
        last_activity -> Timestamp,
        created_at -> Timestamp,
        updated_at -> Timestamp,
        #[max_length = 255]
        git_branch -> Nullable<Varchar>,
        total_cost_usd -> Float8,
        input_tokens -> Int8,
        output_tokens -> Int8,
        cache_creation_tokens -> Int8,
        cache_read_tokens -> Int8,
        #[max_length = 32]
        client_version -> Nullable<Varchar>,
        input_seq -> Int8,
        #[max_length = 255]
        hostname -> Varchar,
        launcher_id -> Nullable<Uuid>,
        #[max_length = 512]
        pr_url -> Nullable<Varchar>,
        #[max_length = 16]
        agent_type -> Varchar,
        #[max_length = 512]
        repo_url -> Nullable<Varchar>,
    }
}

diesel::table! {
    users (id) {
        id -> Uuid,
        #[max_length = 255]
        google_id -> Varchar,
        #[max_length = 255]
        email -> Varchar,
        #[max_length = 255]
        name -> Nullable<Varchar>,
        avatar_url -> Nullable<Text>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
        is_admin -> Bool,
        disabled -> Bool,
        voice_enabled -> Bool,
        ban_reason -> Nullable<Text>,
        sound_config -> Nullable<Jsonb>,
    }
}

diesel::joinable!(deleted_session_costs -> users (user_id));
diesel::joinable!(messages -> sessions (session_id));
diesel::joinable!(messages -> users (user_id));
diesel::joinable!(pending_inputs -> sessions (session_id));
diesel::joinable!(pending_permission_requests -> sessions (session_id));
diesel::joinable!(proxy_auth_tokens -> users (user_id));
diesel::joinable!(session_members -> sessions (session_id));
diesel::joinable!(session_members -> users (user_id));
diesel::joinable!(sessions -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(
    deleted_session_costs,
    messages,
    pending_inputs,
    pending_permission_requests,
    proxy_auth_tokens,
    session_members,
    sessions,
    users,
);
