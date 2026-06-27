//! Shared API request/response types for HTTP endpoints.

mod device_flow;
mod error;
mod launch;
mod metrics;
mod permissions;
mod scheduled_tasks;
mod sessions;
mod system_extra;
mod users;

pub use device_flow::*;
pub use error::*;
pub use launch::*;
pub use metrics::*;
pub use permissions::*;
pub use scheduled_tasks::*;
pub use sessions::*;
pub use system_extra::*;
pub use users::*;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;
    use uuid::Uuid;

    #[test]
    fn codex_permission_input_file_change_roundtrip() {
        // Wire shape mirrors codex_codes 0.129.3 FileChangeRequestApprovalParams
        // — item_id present, reason and grantRoot optional.
        let input = CodexPermissionInput::FileChange {
            item_id: "call_HKaP84kozIUwWE1Ynd5hPpCN".to_string(),
            reason: None,
            grant_root: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "fileChange");
        assert_eq!(json["itemId"], "call_HKaP84kozIUwWE1Ynd5hPpCN");
        // Optional `None`s should not be serialized
        assert!(json.get("reason").is_none());
        assert!(json.get("grantRoot").is_none());

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "FileChange");
    }

    #[test]
    fn codex_permission_input_apply_patch_roundtrip() {
        let input = CodexPermissionInput::ApplyPatch {
            file_changes: serde_json::json!({
                "/tmp/a.rs": {"kind": "modify"},
                "/tmp/b.rs": {"kind": "add"},
            }),
            grant_root: Some("/tmp".to_string()),
            reason: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "applyPatch");
        assert_eq!(json["fileChanges"]["/tmp/a.rs"]["kind"], "modify");
        assert_eq!(json["grantRoot"], "/tmp");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "ApplyPatch");
    }

    #[test]
    fn codex_permission_input_bash_roundtrip() {
        let input = CodexPermissionInput::Bash {
            command: "ls -la".to_string(),
            cwd: "/tmp".to_string(),
            parsed_cmd: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "bash");
        assert_eq!(json["command"], "ls -la");
        assert_eq!(json["cwd"], "/tmp");
        assert!(json.get("parsedCmd").is_none());

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "Bash");
    }

    #[test]
    fn codex_permission_input_exec_command_roundtrip() {
        let input = CodexPermissionInput::ExecCommand {
            command: "ls -la".to_string(),
            cwd: "/tmp".to_string(),
            parsed_cmd: Some(serde_json::json!([{"cmd": "ls"}])),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "execCommand");
        assert_eq!(json["parsedCmd"][0]["cmd"], "ls");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "ExecCommand");
    }

    #[test]
    fn codex_permission_input_permissions_roundtrip() {
        let input = CodexPermissionInput::Permissions {
            cwd: Some("/home/user/project".to_string()),
            permissions: Some(serde_json::json!({"read": ["/tmp"]})),
            reason: Some("requested by agent".to_string()),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "permissions");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "Permissions");
    }

    #[test]
    fn codex_permission_input_mcp_elicitation_roundtrip() {
        let input = CodexPermissionInput::McpElicitation {
            server_name: "github".to_string(),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "mcpElicitation");
        assert_eq!(json["serverName"], "github");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "McpElicitation");
    }

    #[test]
    fn codex_permission_input_ask_user_question_roundtrip() {
        let input = CodexPermissionInput::AskUserQuestion {
            questions: serde_json::json!([{"question": "ok?"}]),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "askUserQuestion");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "AskUserQuestion");
    }

    /// Wire shape lifted verbatim from `claude-codes`'
    /// `test_result_with_new_fields`: per-model entry with camelCase keys and
    /// `costUSD` (not `costUsd`). The frontend renderer iterates this map for
    /// the timing tooltip; the typed parse must accept the live wire shape.
    #[test]
    fn model_usage_entry_roundtrip() {
        let json = serde_json::json!({
            "inputTokens": 3817,
            "outputTokens": 14,
            "costUSD": 0.06,
            "futureField": 99,
        });
        let parsed: ModelUsageEntry = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.input_tokens, 3817);
        assert_eq!(parsed.output_tokens, 14);
        assert!((parsed.cost_usd - 0.06).abs() < 1e-9);
        // Unset fields default to 0
        assert_eq!(parsed.cache_read_input_tokens, 0);
        assert_eq!(parsed.cache_creation_input_tokens, 0);
        assert_eq!(parsed.web_search_requests, 0);
        assert_eq!(parsed.extra["futureField"], 99);
    }

    /// The full `modelUsage` map: a `BTreeMap<String, ModelUsageEntry>` keyed
    /// by model name. Multiple models accumulate when a session uses haiku +
    /// opus or similar.
    #[test]
    fn model_usage_map_roundtrip() {
        let json = serde_json::json!({
            "claude-opus-4-7[1m]": {
                "inputTokens": 3817,
                "outputTokens": 14,
                "costUSD": 0.06,
            },
            "claude-haiku-4-5": {
                "inputTokens": 100,
                "outputTokens": 5,
                "costUSD": 0.001,
            },
        });
        let parsed: ModelUsage = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.len(), 2);
        let opus = parsed.get("claude-opus-4-7[1m]").unwrap();
        assert_eq!(opus.input_tokens, 3817);
        assert!((opus.cost_usd - 0.06).abs() < 1e-9);
        let haiku = parsed.get("claude-haiku-4-5").unwrap();
        assert_eq!(haiku.output_tokens, 5);
    }

    /// Regression guard for the pattern that motivated #725/#731: optional
    /// fields must default cleanly so a Codex frame that omits them parses
    /// rather than silently surfacing as Unknown.
    #[test]
    fn codex_permission_input_file_change_omits_optional_fields() {
        // Frame as observed live in PR #721's bug report — no `reason` /
        // `grantRoot`; should parse as-is.
        let json = serde_json::json!({
            "tool": "fileChange",
            "itemId": "call_HKaP84kozIUwWE1Ynd5hPpCN",
        });
        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        match parsed {
            CodexPermissionInput::FileChange {
                item_id,
                reason,
                grant_root,
            } => {
                assert_eq!(item_id, "call_HKaP84kozIUwWE1Ynd5hPpCN");
                assert!(reason.is_none());
                assert!(grant_root.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// `TurnMetrics` must round-trip with all the optional fields populated
    /// and with cost present (Claude shape).
    #[test]
    fn turn_metrics_full_roundtrip() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let metrics = TurnMetrics {
            id: None,
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            service_tier: Some("standard".to_string()),
            started_at: started,
            first_token_at: Some(started + chrono::Duration::milliseconds(420)),
            completed_at: Some(started + chrono::Duration::milliseconds(8000)),
            ttft_ms: Some(420),
            total_duration_ms: Some(8000),
            generation_duration_ms: Some(7580),
            max_inter_token_gap_ms: Some(150),
            input_tokens: 1234,
            output_tokens: 567,
            cache_creation_tokens: 0,
            cache_read_tokens: 90,
            thinking_tokens: 12,
            subagent_tokens: 0,
            stop_reason: Some("end_turn".to_string()),
            is_error: false,
            tool_call_count: 3,
            stream_restarts: 0,
            total_cost_usd: Some(0.0145),
        };
        let json = serde_json::to_value(&metrics).unwrap();
        assert_eq!(json["agent_type"], "claude");
        assert_eq!(json["ttft_ms"], 420);
        assert_eq!(json["total_cost_usd"], 0.0145);
        let parsed: TurnMetrics = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, metrics);
    }

    /// The shared known-model gate: missing, blank, whitespace, and the
    /// literal `unknown` placeholder (any case) are all rejected; real model
    /// ids pass.
    #[test]
    fn turn_metrics_has_known_model() {
        let mut metrics = TurnMetrics {
            id: None,
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            service_tier: None,
            started_at: Utc::now(),
            first_token_at: None,
            completed_at: None,
            ttft_ms: None,
            total_duration_ms: None,
            generation_duration_ms: None,
            max_inter_token_gap_ms: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            thinking_tokens: 0,
            subagent_tokens: 0,
            stop_reason: None,
            is_error: false,
            tool_call_count: 0,
            stream_restarts: 0,
            total_cost_usd: None,
        };
        assert!(metrics.has_known_model());

        for bad in [
            None,
            Some(""),
            Some("   "),
            Some("unknown"),
            Some("UNKNOWN"),
        ] {
            metrics.model = bad.map(str::to_string);
            assert!(!metrics.has_known_model(), "expected {:?} rejected", bad);
        }
    }

    /// `MetricBucket` round-trip. Claude row with all fields populated; the
    /// stop-reason mix has a couple of representative entries.
    #[test]
    fn metric_bucket_full_roundtrip() {
        let bucket_start = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut counts = BTreeMap::new();
        counts.insert("end_turn".to_string(), 5);
        counts.insert("max_tokens".to_string(), 1);
        let bucket = MetricBucket {
            bucket_start,
            agent_type: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            service_tier: Some("standard".to_string()),
            turn_count: 6,
            error_count: 0,
            ttft_p50_ms: Some(420),
            ttft_p95_ms: Some(1200),
            throughput_p50_tps: Some(47.5),
            throughput_p95_tps: Some(65.0),
            input_tokens_sum: 12_000,
            output_tokens_sum: 3_400,
            cache_read_tokens_sum: 800,
            cache_creation_tokens_sum: 200,
            thinking_tokens_sum: 120,
            subagent_tokens_sum: 450,
            total_cost_usd_sum: Some(0.18),
            stop_reason_counts: counts,
        };
        let json = serde_json::to_value(&bucket).unwrap();
        assert_eq!(json["agent_type"], "claude");
        assert_eq!(json["turn_count"], 6);
        assert_eq!(json["stop_reason_counts"]["end_turn"], 5);
        let parsed: MetricBucket = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, bucket);
    }

    /// `MetricBucketsResponse` round-trip via the top-level envelope.
    #[test]
    fn metric_buckets_response_roundtrip() {
        let bucket = MetricBucket {
            bucket_start: chrono::Utc::now(),
            agent_type: "codex".to_string(),
            model: None,
            service_tier: None,
            turn_count: 3,
            error_count: 1,
            ttft_p50_ms: None,
            ttft_p95_ms: None,
            throughput_p50_tps: None,
            throughput_p95_tps: None,
            input_tokens_sum: 0,
            output_tokens_sum: 0,
            cache_read_tokens_sum: 0,
            cache_creation_tokens_sum: 0,
            thinking_tokens_sum: 0,
            subagent_tokens_sum: 0,
            total_cost_usd_sum: None,
            stop_reason_counts: BTreeMap::new(),
        };
        let resp = MetricBucketsResponse {
            buckets: vec![bucket.clone()],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json["buckets"].is_array());
        let parsed: MetricBucketsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.buckets, vec![bucket]);
    }

    /// Codex-shape: cost is `None` and many of the optional fields are also
    /// unset. Must serialize without nulls for the skip-if-none fields and
    /// must round-trip back to the same value.
    #[test]
    fn turn_metrics_codex_no_cost_roundtrip() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let metrics = TurnMetrics {
            id: None,
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: "codex".to_string(),
            model: None,
            service_tier: None,
            started_at: started,
            first_token_at: None,
            completed_at: Some(started + chrono::Duration::milliseconds(120)),
            ttft_ms: None,
            total_duration_ms: Some(120),
            generation_duration_ms: None,
            max_inter_token_gap_ms: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            thinking_tokens: 0,
            subagent_tokens: 0,
            stop_reason: Some("failed".to_string()),
            is_error: true,
            tool_call_count: 0,
            stream_restarts: 0,
            total_cost_usd: None,
        };
        let json = serde_json::to_value(&metrics).unwrap();
        assert_eq!(json["agent_type"], "codex");
        assert!(json.get("total_cost_usd").is_none());
        assert!(json.get("ttft_ms").is_none());
        let parsed: TurnMetrics = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, metrics);
    }

    fn sample_task_fields() -> crate::endpoints::ScheduledTaskFields {
        crate::endpoints::ScheduledTaskFields {
            name: "nightly audit".to_string(),
            cron_expression: "0 3 * * *".to_string(),
            timezone: "UTC".to_string(),
            working_directory: "/home/user/project".to_string(),
            prompt: "Check deps".to_string(),
            claude_args: vec!["--verbose".to_string()],
            agent_type: crate::AgentType::Claude,
            max_runtime_minutes: 30,
        }
    }

    /// Pins the CreateScheduledTaskRequest wire shape: flattened fields must
    /// produce the same keys/values as the pre-flatten struct, old field
    /// order must still parse, and the timezone / max_runtime / claude_args /
    /// agent_type defaults must be preserved.
    #[test]
    fn create_scheduled_task_request_wire_compat() {
        let req = CreateScheduledTaskRequest {
            fields: sample_task_fields(),
            hostname: "buildbox".to_string(),
        };
        let expected: serde_json::Value = serde_json::from_str(
            r#"{
                "name": "nightly audit",
                "cron_expression": "0 3 * * *",
                "timezone": "UTC",
                "hostname": "buildbox",
                "working_directory": "/home/user/project",
                "prompt": "Check deps",
                "claude_args": ["--verbose"],
                "agent_type": "claude",
                "max_runtime_minutes": 30
            }"#,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&req).unwrap(), expected);

        // Old wire order (hostname between timezone and working_directory)
        // still parses identically.
        let parsed: CreateScheduledTaskRequest = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(parsed.fields, req.fields);
        assert_eq!(parsed.hostname, "buildbox");

        // Optional fields keep their historical defaults when omitted.
        let minimal = r#"{
            "name": "n",
            "cron_expression": "* * * * *",
            "hostname": "h",
            "working_directory": "/tmp",
            "prompt": "p"
        }"#;
        let parsed: CreateScheduledTaskRequest = serde_json::from_str(minimal).unwrap();
        assert_eq!(parsed.fields.timezone, "UTC");
        assert_eq!(parsed.fields.max_runtime_minutes, 30);
        assert!(parsed.fields.claude_args.is_empty());
        assert_eq!(parsed.fields.agent_type, crate::AgentType::Claude);
    }

    /// Pins the ScheduledTaskInfo wire shape, including the always-serialized
    /// `last_session_id: null` for None.
    #[test]
    fn scheduled_task_info_wire_compat() {
        let info = ScheduledTaskInfo {
            id: uuid::Uuid::nil(),
            fields: sample_task_fields(),
            hostname: "buildbox".to_string(),
            enabled: true,
            last_session_id: None,
            last_run_at: Some("2026-01-01T00:00:00+00:00".to_string()),
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
            updated_at: "2026-01-01T00:00:00+00:00".to_string(),
        };
        let expected: serde_json::Value = serde_json::from_str(
            r#"{
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "nightly audit",
                "cron_expression": "0 3 * * *",
                "timezone": "UTC",
                "hostname": "buildbox",
                "working_directory": "/home/user/project",
                "prompt": "Check deps",
                "claude_args": ["--verbose"],
                "agent_type": "claude",
                "enabled": true,
                "max_runtime_minutes": 30,
                "last_session_id": null,
                "last_run_at": "2026-01-01T00:00:00+00:00",
                "created_at": "2026-01-01T00:00:00+00:00",
                "updated_at": "2026-01-01T00:00:00+00:00"
            }"#,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&info).unwrap(), expected);

        // Old wire order still parses identically.
        let parsed: ScheduledTaskInfo = serde_json::from_value(expected).unwrap();
        assert_eq!(parsed, info);
    }
}
