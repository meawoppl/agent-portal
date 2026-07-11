// @generated automatically by Diesel CLI.

diesel::table! {
    custom_subdomains (label) {
        label -> Text,
        session_id -> Uuid,
        created_by -> Nullable<Uuid>,
        created_at -> Timestamp,
    }
}

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
    forward_subdomains (label) {
        label -> Text,
        session_id -> Uuid,
        created_at -> Timestamp,
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
        #[max_length = 16]
        agent_type -> Varchar,
        #[max_length = 32]
        provenance_kind -> Nullable<Varchar>,
        provenance_session_id -> Nullable<Uuid>,
        #[max_length = 16]
        provenance_agent_type -> Nullable<Varchar>,
    }
}

diesel::table! {
    pending_inputs (id) {
        id -> Uuid,
        session_id -> Uuid,
        seq_num -> Int8,
        content -> Text,
        created_at -> Timestamp,
        #[max_length = 32]
        send_mode -> Nullable<Varchar>,
        client_msg_id -> Nullable<Uuid>,
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
        expires_at -> Nullable<Timestamp>,
        revoked -> Bool,
        session_id -> Nullable<Uuid>,
    }
}

diesel::table! {
    push_subscriptions (id) {
        id -> Uuid,
        user_id -> Uuid,
        platform -> Varchar,
        endpoint_or_token -> Text,
        p256dh -> Nullable<Text>,
        auth -> Nullable<Text>,
        device_label -> Nullable<Varchar>,
        created_at -> Timestamptz,
        last_success_at -> Nullable<Timestamptz>,
        disabled_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    scheduled_tasks (id) {
        id -> Uuid,
        user_id -> Uuid,
        #[max_length = 255]
        name -> Varchar,
        #[max_length = 128]
        cron_expression -> Varchar,
        #[max_length = 64]
        timezone -> Varchar,
        #[max_length = 255]
        hostname -> Varchar,
        working_directory -> Text,
        prompt -> Text,
        claude_args -> Jsonb,
        #[max_length = 16]
        agent_type -> Varchar,
        enabled -> Bool,
        max_runtime_minutes -> Int4,
        last_session_id -> Nullable<Uuid>,
        last_run_at -> Nullable<Timestamp>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    session_continuations (id) {
        id -> Uuid,
        session_id -> Uuid,
        user_id -> Uuid,
        launcher_id -> Uuid,
        reset_at -> Timestamptz,
        prompt -> Text,
        #[max_length = 32]
        status -> Varchar,
        source_message -> Nullable<Text>,
        last_error -> Nullable<Text>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
        scheduled_at -> Nullable<Timestamp>,
        fired_at -> Nullable<Timestamp>,
        dropped_at -> Nullable<Timestamp>,
        cancelled_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    session_forwards (id) {
        id -> Uuid,
        session_id -> Uuid,
        port -> Int4,
        created_at -> Timestamp,
        public -> Bool,
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
        scheduled_task_id -> Nullable<Uuid>,
        paused -> Bool,
        claude_args -> Jsonb,
        launch_failure_count -> Int4,
        last_launch_attempt_at -> Nullable<Timestamp>,
        launch_lease_until -> Nullable<Timestamp>,
        open_prs -> Jsonb,
        archived_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    turn_metrics (id) {
        id -> Uuid,
        session_id -> Nullable<Uuid>,
        user_message_id -> Nullable<Uuid>,
        agent_type -> Text,
        model -> Nullable<Text>,
        service_tier -> Nullable<Text>,
        started_at -> Timestamptz,
        first_token_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        ttft_ms -> Nullable<Int8>,
        total_duration_ms -> Nullable<Int8>,
        generation_duration_ms -> Nullable<Int8>,
        max_inter_token_gap_ms -> Nullable<Int8>,
        input_tokens -> Int8,
        output_tokens -> Int8,
        cache_creation_tokens -> Int8,
        cache_read_tokens -> Int8,
        thinking_tokens -> Int8,
        stop_reason -> Nullable<Text>,
        is_error -> Bool,
        tool_call_count -> Int4,
        stream_restarts -> Int4,
        total_cost_usd -> Nullable<Float8>,
        created_at -> Timestamptz,
        user_id -> Uuid,
        subagent_tokens -> Int8,
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
        ban_reason -> Nullable<Text>,
        sound_config -> Nullable<Jsonb>,
        notification_prefs -> Nullable<Jsonb>,
    }
}

diesel::joinable!(custom_subdomains -> sessions (session_id));
diesel::joinable!(custom_subdomains -> users (created_by));
diesel::joinable!(deleted_session_costs -> users (user_id));
diesel::joinable!(forward_subdomains -> sessions (session_id));
diesel::joinable!(messages -> sessions (session_id));
diesel::joinable!(messages -> users (user_id));
diesel::joinable!(pending_inputs -> sessions (session_id));
diesel::joinable!(pending_permission_requests -> sessions (session_id));
diesel::joinable!(proxy_auth_tokens -> users (user_id));
diesel::joinable!(push_subscriptions -> users (user_id));
diesel::joinable!(scheduled_tasks -> users (user_id));
diesel::joinable!(session_continuations -> sessions (session_id));
diesel::joinable!(session_continuations -> users (user_id));
diesel::joinable!(session_forwards -> sessions (session_id));
diesel::joinable!(session_members -> sessions (session_id));
diesel::joinable!(session_members -> users (user_id));
diesel::joinable!(sessions -> users (user_id));
diesel::joinable!(turn_metrics -> messages (user_message_id));
diesel::joinable!(turn_metrics -> sessions (session_id));
diesel::joinable!(turn_metrics -> users (user_id));

diesel::allow_tables_to_appear_in_same_query!(
    custom_subdomains,
    deleted_session_costs,
    forward_subdomains,
    messages,
    pending_inputs,
    pending_permission_requests,
    proxy_auth_tokens,
    push_subscriptions,
    scheduled_tasks,
    session_continuations,
    session_forwards,
    session_members,
    sessions,
    turn_metrics,
    users,
);
