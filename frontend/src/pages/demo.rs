use crate::components::{group_messages, thinking_chip_starts, MessageGroupRenderer};
use crate::pages::dashboard::session_rail::{ActivityRef, BroadcastRef, SessionRail};
use crate::pages::dashboard::RailPosition;
use gloo::timers::callback::Interval;
use serde_json::json as fixture_json;
use shared::{AgentType, MessageSource, PortalMeta, SessionInfo, SessionRole, SessionStatus};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use web_sys::HtmlElement;
use yew::prelude::*;

const PLAYBACK_MS: u32 = 1_400;

#[derive(Clone, PartialEq)]
struct DemoMessage {
    content: String,
    meta: PortalMeta,
}

#[derive(Clone, PartialEq)]
struct DemoScenario {
    session: SessionInfo,
    messages: Vec<DemoMessage>,
}

struct DemoSessionSpec {
    id: Uuid,
    user_id: Uuid,
    name: &'static str,
    key: &'static str,
    cwd: &'static str,
    branch: &'static str,
    agent_type: AgentType,
    last_model: Option<&'static str>,
}

#[function_component(DemoPage)]
pub fn demo_page() -> Html {
    let scenarios = use_memo((), |_| demo_scenarios());
    let focused_index = use_state(|| 0usize);
    let revealed_counts = use_state(|| vec![1usize; scenarios.len()]);
    let messages_ref = use_node_ref();
    let current_user_id = current_user_uuid().to_string();

    {
        let revealed_counts = revealed_counts.clone();
        let lengths: Vec<usize> = scenarios.iter().map(|s| s.messages.len()).collect();
        use_effect_with(lengths, move |lengths| {
            let lengths = lengths.clone();
            let interval = Interval::new(PLAYBACK_MS, move || {
                let current = (*revealed_counts).clone();
                let all_done = current
                    .iter()
                    .zip(lengths.iter())
                    .all(|(shown, total)| shown >= total);
                let next = if all_done {
                    vec![1; lengths.len()]
                } else {
                    current
                        .iter()
                        .zip(lengths.iter())
                        .map(|(shown, total)| (*shown + 1).min(*total))
                        .collect()
                };
                revealed_counts.set(next);
            });
            move || drop(interval)
        });
    }

    {
        let messages_ref = messages_ref.clone();
        let focused = *focused_index;
        let revealed = revealed_counts.get(focused).copied().unwrap_or(0);
        use_effect_with((focused, revealed), move |_| {
            if let Some(el) = messages_ref.cast::<HtmlElement>() {
                el.set_scroll_top(el.scroll_height());
            }
            || ()
        });
    }

    let sessions: Vec<SessionInfo> = scenarios.iter().map(|s| s.session.clone()).collect();
    let connected_sessions: HashSet<Uuid> = sessions.iter().map(|s| s.id).collect();
    let awaiting_sessions: HashSet<Uuid> = sessions
        .iter()
        .filter(|s| s.session_name.contains("Review"))
        .map(|s| s.id)
        .collect();
    let hidden_sessions = HashSet::new();
    let on_select = {
        let focused_index = focused_index.clone();
        Callback::from(move |index: usize| focused_index.set(index))
    };

    let focused_index_value = (*focused_index).min(scenarios.len().saturating_sub(1));
    let scenario = &scenarios[focused_index_value];
    let shown = revealed_counts
        .get(focused_index_value)
        .copied()
        .unwrap_or(0)
        .min(scenario.messages.len());
    let rendered_messages = scenario
        .messages
        .iter()
        .take(shown)
        .map(|message| {
            crate::components::message_renderer::RenderedMessage::new(
                message.content.clone(),
                Some(message.meta.clone()),
            )
        })
        .collect::<Vec<_>>();
    let groups = group_messages(
        &rendered_messages,
        scenario.session.agent_type,
        Some(current_user_id.as_str()),
    );
    let thinking_starts = thinking_chip_starts(&groups);

    html! {
        <main class="focus-flow-container demo-portal-page">
            <header class="focus-flow-header demo-portal-header">
                <h1>{ "Agent Portal UI Demo" }</h1>
                <div class="header-actions">
                    <span class="demo-playback-chip">{ "fixture playback" }</span>
                    <span class="demo-playback-chip">{ format!("{}/{} events", shown, scenario.messages.len()) }</span>
                    <a class="header-button" href="/dashboard">{ "Live Portal" }</a>
                </div>
            </header>

            <div class="dashboard-body rail-top demo-dashboard-body">
                <SessionRail
                    sessions={sessions}
                    focused_index={focused_index_value}
                    awaiting_sessions={awaiting_sessions}
                    hidden_sessions={hidden_sessions}
                    inactive_hidden={false}
                    group_by_host={true}
                    connected_sessions={connected_sessions}
                    nav_mode={false}
                    activity_timestamps={ActivityRef::default()}
                    broadcasts={BroadcastRef::default()}
                    rail_position={RailPosition::Top}
                    server_version={"demo".to_string()}
                    on_select={on_select}
                    on_leave={Callback::from(|_: Uuid| {})}
                    on_delete={Callback::from(|_: Uuid| {})}
                    on_toggle_hidden={Callback::from(|_: Uuid| {})}
                    on_toggle_inactive_hidden={Callback::from(|_: MouseEvent| {})}
                    on_stop={Callback::from(|_: Uuid| {})}
                    on_toggle_pause={Callback::from(|_: (Uuid, bool)| {})}
                />

                <div class="session-views-container demo-session-views">
                    <div class="session-view-wrapper focused">
                        <section class="session-view focused demo-session-view">
                            <div class="session-view-header">
                                <span class="session-name">{ &scenario.session.session_name }</span>
                                <span class="session-hostname">{ &scenario.session.hostname }</span>
                                <span class="session-path">{ &scenario.session.working_directory }</span>
                                <span class="session-launcher-version">{ "launcher vdemo" }</span>
                                <span class="status connected">{ "active" }</span>
                            </div>

                            <div class="session-view-scroll-area">
                                <div class="session-view-messages" ref={messages_ref}>
                                    { for groups.into_iter().enumerate().map(|(i, group)| {
                                        let key = group.key(i);
                                        let thinking_start = thinking_starts.get(i).copied().unwrap_or(0);
                                        html! {
                                            <MessageGroupRenderer
                                                {key}
                                                group={group}
                                                session_id={scenario.session.id}
                                                agent_type={scenario.session.agent_type}
                                                current_user_id={Some(current_user_id.clone())}
                                                continuation_statuses={HashMap::<Uuid, String>::new()}
                                                on_schedule_continuation={Callback::from(|_: Uuid| {})}
                                                {thinking_start}
                                            />
                                        }
                                    })}
                                </div>
                            </div>

                            <div class="session-view-input demo-input-preview">
                                <span class="input-prompt">{ "demo" }</span>
                                <textarea
                                    class="message-input"
                                    value="Fixture input is disabled; this page replays canned backend events."
                                    disabled=true
                                />
                                <button class="send-button" disabled=true>{ "Send" }</button>
                            </div>
                        </section>
                    </div>
                </div>
            </div>
        </main>
    }
}

fn demo_scenarios() -> Vec<DemoScenario> {
    let user_id = current_user_uuid();
    vec![
        DemoScenario {
            session: demo_session(DemoSessionSpec {
                id: demo_uuid(1),
                user_id,
                name: "Claude Message Catalog",
                key: "claude-demo-01",
                cwd: "/workspace/agent-portal",
                branch: "demo/claude-catalog",
                agent_type: AgentType::Claude,
                last_model: Some("claude-sonnet-4-5-20250929"),
            }),
            messages: claude_catalog_messages(user_id),
        },
        DemoScenario {
            session: demo_session(DemoSessionSpec {
                id: demo_uuid(2),
                user_id,
                name: "Codex Message Catalog",
                key: "codex-demo-01",
                cwd: "/workspace/rust-sdk",
                branch: "demo/codex-catalog",
                agent_type: AgentType::Codex,
                last_model: Some("gpt-5-codex"),
            }),
            messages: codex_catalog_messages(user_id),
        },
        DemoScenario {
            session: demo_session(DemoSessionSpec {
                id: demo_uuid(3),
                user_id,
                name: "Claude Radio Peer",
                key: "claude-radio-demo",
                cwd: "/workspace/agents/claude",
                branch: "demo/radio-claude",
                agent_type: AgentType::Claude,
                last_model: Some("claude-sonnet-4-5-20250929"),
            }),
            messages: radio_messages(user_id, AgentType::Claude, demo_uuid(4), AgentType::Codex),
        },
        DemoScenario {
            session: demo_session(DemoSessionSpec {
                id: demo_uuid(4),
                user_id,
                name: "Codex Radio Peer",
                key: "codex-radio-demo",
                cwd: "/workspace/agents/codex",
                branch: "demo/radio-codex",
                agent_type: AgentType::Codex,
                last_model: Some("gpt-5-codex-mini"),
            }),
            messages: radio_messages(user_id, AgentType::Codex, demo_uuid(3), AgentType::Claude),
        },
    ]
}

fn demo_session(spec: DemoSessionSpec) -> SessionInfo {
    SessionInfo {
        id: spec.id,
        user_id: spec.user_id,
        session_name: spec.name.to_string(),
        session_key: spec.key.to_string(),
        working_directory: spec.cwd.to_string(),
        status: SessionStatus::Active,
        last_activity: "2026-07-20T04:00:00.000000Z".to_string(),
        created_at: "2026-07-20T03:55:00.000000Z".to_string(),
        updated_at: "2026-07-20T04:00:00.000000Z".to_string(),
        git_branch: Some(spec.branch.to_string()),
        my_role: SessionRole::Owner,
        hostname: "demo-host".to_string(),
        launcher_id: Some(demo_uuid(100)),
        launcher_version: Some("demo".to_string()),
        pr_url: Some("https://github.com/meawoppl/agent-portal/pull/1439".to_string()),
        repo_url: Some("https://github.com/meawoppl/agent-portal".to_string()),
        open_prs: vec![shared::PrRef {
            number: 1439,
            url: "https://github.com/meawoppl/agent-portal/pull/1439".to_string(),
            branch: spec.branch.to_string(),
        }],
        agent_type: spec.agent_type,
        client_version: Some("demo".to_string()),
        scheduled_task_id: None,
        paused: false,
        claude_args: vec![
            "--model".to_string(),
            spec.last_model.unwrap_or("demo").to_string(),
        ],
        last_model: spec.last_model.map(str::to_string),
    }
}

fn claude_catalog_messages(user_id: Uuid) -> Vec<DemoMessage> {
    vec![
        claude_system(
            "2026-07-20T04:00:00.000000Z",
            fixture_json!({
                "subtype": "init",
                "cwd": "/workspace/agent-portal",
                "session_id": demo_uuid(1).to_string(),
                "tools": ["Read", "Write", "Bash", "WebFetch", "AskUserQuestion"],
                "mcp_servers": [{ "name": "demo-files" }],
                "model": "claude-sonnet-4-5-20250929",
                "claude_code_version": "demo",
                "fast_mode_state": "on"
            }),
        ),
        user("2026-07-20T04:00:02.000000Z", user_id, "Show every Claude-side message surface: markdown, math, images, tool calls, system cards, errors, portal cards, and raw fallbacks."),
        claude_system(
            "2026-07-20T04:00:04.000000Z",
            fixture_json!({
                "subtype": "status",
                "status": "compacting",
                "session_id": demo_uuid(1).to_string(),
            }),
        ),
        claude_system(
            "2026-07-20T04:00:06.000000Z",
            fixture_json!({
                "subtype": "compact_boundary",
                "session_id": demo_uuid(1).to_string(),
                "summary": "Condensed prior exploration into fixture requirements.",
                "leaf_message_count": 42,
                "duration_ms": 1300,
            }),
        ),
        claude_system(
            "2026-07-20T04:00:08.000000Z",
            fixture_json!({
                "subtype": "task_started",
                "task_id": "task-demo-renderers",
                "tool_use_id": "toolu_task_demo",
                "description": "Catalog renderer coverage",
                "started_at": "2026-07-20T04:00:08.000000Z",
                "task_type": "general",
                "session_id": demo_uuid(1).to_string(),
            }),
        ),
        claude_system(
            "2026-07-20T04:00:10.000000Z",
            fixture_json!({
                "subtype": "task_notification",
                "task_id": "task-demo-renderers",
                "status": "in_progress",
                "message": "Rendering catalog fixtures",
                "last_tool_name": "Read",
                "usage": {
                    "duration_ms": 2400,
                    "tool_uses": 3,
                    "total_tokens": 1220
                },
                "session_id": demo_uuid(1).to_string(),
            }),
        ),
        claude_system(
            "2026-07-20T04:00:12.000000Z",
            fixture_json!({
                "subtype": "thinking_tokens",
                "estimated_tokens": 512,
                "estimated_tokens_delta": 512,
                "session_id": demo_uuid(1).to_string(),
            }),
        ),
        assistant_text("2026-07-20T04:00:14.000000Z", "Claude Sonnet 4.5", r#"Here is a **demo response** using the production markdown renderer.

Inline math: $E = mc^2$.

Display math:

$$
\int_0^1 x^2\,dx = \frac{1}{3}
$$

| Renderer | Demo signal |
| --- | --- |
| Markdown | tables, code, links |
| KaTeX | inline and display equations |
| Grouping | multi-part assistant turns |"#),
        assistant_blocks(
            "2026-07-20T04:00:16.000000Z",
            "Claude Sonnet 4.5",
            vec![
                fixture_json!({
                    "type": "thinking",
                    "thinking": "Classify every renderer path before emitting fixture examples.",
                    "signature": "demo-signature",
                }),
                fixture_json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": demo_png_base64(),
                    },
                }),
                fixture_json!({
                    "type": "server_tool_use",
                    "id": "srvu_demo_search",
                    "name": "web_search",
                    "input": { "query": "Agent Portal UI demo fixtures" },
                }),
                fixture_json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvu_demo_search",
                    "content": [{ "title": "Demo result", "url": "https://example.test/demo", "snippet": "Fixture result for renderer coverage." }],
                }),
                fixture_json!({
                    "type": "code_execution_tool_result",
                    "tool_use_id": "srvu_demo_code",
                    "content": { "stdout": "plot saved to /tmp/demo.svg", "stderr": "", "exit_code": 0 },
                }),
                fixture_json!({
                    "type": "mcp_tool_use",
                    "id": "mcpu_demo",
                    "name": "lookup_component",
                    "server_name": "demo-files",
                    "input": { "component": "MessageGroupRenderer" },
                }),
                fixture_json!({
                    "type": "mcp_tool_result",
                    "tool_use_id": "mcpu_demo",
                    "content": { "content": [{ "type": "text", "text": "Renderer found." }] },
                    "is_error": false,
                }),
                fixture_json!({
                    "type": "container_upload",
                    "file_id": "file_demo_catalog",
                    "filename": "demo-output.svg",
                    "size_bytes": 2048,
                }),
                fixture_json!({
                    "type": "fallback",
                    "from": { "model": "claude-opus-4-8-20260115" },
                    "to": { "model": "claude-sonnet-4-5-20250929" },
                }),
                fixture_json!({
                    "type": "future_content_block",
                    "note": "Unknown assistant content block fallback.",
                }),
            ],
        ),
        assistant_tool("2026-07-20T04:00:18.000000Z", "toolu_demo_read", "Read", fixture_json!({ "file_path": "/workspace/agent-portal/frontend/src/pages/demo.rs" })),
        tool_result("2026-07-20T04:00:20.000000Z", "toolu_demo_read", "pub fn demo_page() -> Html {\n    html! { <DemoPage /> }\n}\n"),
        assistant_tool("2026-07-20T04:00:22.000000Z", "toolu_demo_bash", "Bash", fixture_json!({ "command": "cargo test -p frontend demo", "description": "Run the demo smoke test" })),
        tool_result("2026-07-20T04:00:24.000000Z", "toolu_demo_bash", "running 1 test\ntest demo_page_renders_fixture_sessions ... ok\n\nresult: ok"),
        assistant_multi("2026-07-20T04:00:26.000000Z", vec![
            "Multi-part assistant content can mix prose and tool affordances.",
            "A later content block lands in the same rendered message, after the tool-completion pair.",
        ]),
        portal_text_message("2026-07-20T04:00:28.000000Z", "Portal text message rendered through markdown.\n\n- reconnect notices\n- local system notices\n- general portal announcements"),
        portal_reminder("2026-07-20T04:00:30.000000Z"),
        portal_image("2026-07-20T04:00:32.000000Z"),
        portal_continuation("2026-07-20T04:00:34.000000Z"),
        rate_limit("2026-07-20T04:00:36.000000Z"),
        claude_api_error("2026-07-20T04:00:38.000000Z"),
        result("2026-07-20T04:00:40.000000Z", "claude-sonnet-4-5-20250929", 1960, 420),
        result_error("2026-07-20T04:00:42.000000Z"),
        raw_unknown("2026-07-20T04:00:44.000000Z", "claude_future_frame"),
    ]
}

fn codex_catalog_messages(user_id: Uuid) -> Vec<DemoMessage> {
    vec![
        user(
            "2026-07-20T04:01:00.000000Z",
            user_id,
            "Show every Codex-side event surface: turn lifecycle, item lifecycle, tool cards, app-server events, errors, and raw fallbacks.",
        ),
        codex_event(
            "2026-07-20T04:01:02.000000Z",
            fixture_json!({ "type": "thread.started", "thread_id": "thread_demo_catalog" }),
        ),
        codex_event(
            "2026-07-20T04:01:04.000000Z",
            fixture_json!({ "type": "turn.started" }),
        ),
        codex_event(
            "2026-07-20T04:01:06.000000Z",
            fixture_json!({
                "type": "item.started",
                "item": {
                    "type": "command_execution",
                    "id": "cmd_demo_running",
                    "command": "rg -n \"TODO|FIXME\" frontend/src",
                    "status": "in_progress"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:08.000000Z",
            fixture_json!({
                "type": "item.updated",
                "item": {
                    "type": "command_execution",
                    "id": "cmd_demo_running",
                    "command": "rg -n \"TODO|FIXME\" frontend/src",
                    "aggregated_output": "frontend/src/pages/demo.rs: demo fixture intentionally exercises command output",
                    "status": "in_progress"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:10.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "command_execution",
                    "id": "cmd_demo_running",
                    "command": "rg -n \"TODO|FIXME\" frontend/src",
                    "aggregated_output": "frontend/src/pages/demo.rs: demo fixture intentionally exercises command output",
                    "exit_code": 0,
                    "status": "completed"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:12.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "agent_message",
                    "id": "msg_demo_codex",
                    "text": "Codex renders **Markdown**, math like $a^2 + b^2 = c^2$, and fenced code:\n\n```rust\nfn demo() -> &'static str { \"portal\" }\n```"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:14.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "reasoning",
                    "id": "rs_demo_1",
                    "text": "Need verify markdown, command execution, web search, todo, MCP, file change, and completion cards."
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:16.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "file_change",
                    "id": "fc_demo",
                    "changes": [{
                        "path": "frontend/src/pages/demo.rs",
                        "kind": { "type": "update" },
                        "diff": "@@ -1 +1 @@\n-old demo\n+expanded demo\n"
                    }],
                    "status": "completed"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:18.000000Z",
            fixture_json!({
                "type": "item/fileChange/patchUpdated",
                "params": {
                    "itemId": "fc_patch_demo",
                    "changes": [{
                        "path": "frontend/styles/demo.css",
                        "kind": { "type": "update" },
                        "diff": "@@ -1 +1 @@\n-old color\n+new color\n"
                    }]
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:20.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "mcp_tool_call",
                    "id": "mcp_demo",
                    "server": "demo-files",
                    "tool": "lookup",
                    "arguments": { "path": "frontend/src/pages/demo.rs" },
                    "status": "completed"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:22.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "web_search",
                    "id": "search_demo",
                    "query": "Agent Portal visual regression examples"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:24.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "todo_list",
                    "id": "todo_demo",
                    "items": [
                        { "text": "Render Claude catalog", "completed": true },
                        { "text": "Render Codex catalog", "completed": true },
                        { "text": "Exercise inter-agent messages", "completed": false }
                    ]
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:26.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "error",
                    "id": "err_demo",
                    "message": "Synthetic item-level error for renderer coverage."
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:28.000000Z",
            fixture_json!({
                "type": "turn/plan/updated",
                "params": {
                    "explanation": "Catalog scenario plan",
                    "plan": [
                        { "step": "Render lifecycle items", "status": "completed" },
                        { "step": "Render app-server notifications", "status": "inProgress" },
                        { "step": "Render terminal events", "status": "pending" }
                    ]
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:30.000000Z",
            fixture_json!({
                "type": "turn/diff/updated",
                "params": {
                    "turnId": "turn_demo_codex",
                    "diff": "diff --git a/demo b/demo\n+hidden cumulative diff sample\n"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:32.000000Z",
            fixture_json!({
                "type": "item/plan/delta",
                "params": {
                    "itemId": "plan_delta_demo",
                    "delta": "streaming plan delta"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:34.000000Z",
            fixture_json!({
                "type": "item/reasoning/summaryPartAdded",
                "params": { "text": "summary delta" }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:36.000000Z",
            fixture_json!({
                "type": "item/reasoning/textDelta",
                "params": { "delta": "reasoning delta" }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:38.000000Z",
            fixture_json!({
                "type": "thread/compacted",
                "params": {
                    "threadId": "thread_demo_catalog",
                    "turnId": "turn_demo_codex"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:40.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "contextCompaction",
                    "id": "ctx_demo"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:42.000000Z",
            fixture_json!({
                "type": "item.completed",
                "item": {
                    "type": "collabAgentToolCall",
                    "id": "agent_demo",
                    "tool": { "type": "spawnAgent" },
                    "model": "gpt-5-codex-mini",
                    "reasoningEffort": "medium",
                    "prompt": "Review the demo fixture coverage.",
                    "status": { "type": "completed" },
                    "agentsStates": {},
                    "receiverThreadIds": ["thread_peer_demo"],
                    "senderThreadId": "thread_demo_catalog"
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:44.000000Z",
            fixture_json!({
                "type": "error",
                "message": "Synthetic top-level Codex error card."
            }),
        ),
        codex_event(
            "2026-07-20T04:01:46.000000Z",
            fixture_json!({
                "type": "turn.completed",
                "duration_ms": 14200,
                "turn_id": "turn_demo_codex",
                "status": "completed",
                "usage": {
                    "last": {
                        "input_tokens": 2048,
                        "cached_input_tokens": 512,
                        "output_tokens": 384,
                        "reasoning_output_tokens": 96,
                        "total_tokens": 3040
                    },
                    "total": {
                        "total_tokens": 18800
                    },
                    "modelContextWindow": 128000
                }
            }),
        ),
        codex_event(
            "2026-07-20T04:01:48.000000Z",
            fixture_json!({
                "type": "turn.failed",
                "error": { "message": "Synthetic failed turn for visual coverage." }
            }),
        ),
        raw_unknown("2026-07-20T04:01:50.000000Z", "codex_future_frame"),
    ]
}

fn radio_messages(
    user_id: Uuid,
    own_agent_type: AgentType,
    peer_session_id: Uuid,
    peer_agent_type: AgentType,
) -> Vec<DemoMessage> {
    let peer_label = match peer_agent_type {
        AgentType::Claude => "Claude",
        AgentType::Codex => "Codex",
    };
    let mut messages = vec![
        user(
            "2026-07-20T04:01:07.000000Z",
            user_id,
            "Pretend this session is exchanging coordination messages with another agent.",
        ),
        agent_message_from_peer(
            "2026-07-20T04:01:11.000000Z",
            peer_session_id,
            peer_agent_type,
            "I see your catalog run. Please verify the image and equation cards after the next playback loop.",
        ),
        portal_agent_message(
            "2026-07-20T04:01:13.000000Z",
            peer_session_id,
            peer_agent_type,
            "Received. I will compare the pill states and transcript cards after replay.",
        ),
    ];

    match own_agent_type {
        AgentType::Claude => {
            messages.push(assistant_text(
                "2026-07-20T04:01:15.000000Z",
                "Claude Sonnet 4.5",
                &format!(
                    "Preparing a short broadcast for the {peer_label} peer. The next cards use the production inter-agent message renderer."
                ),
            ));
            messages.push(assistant_tool(
                "2026-07-20T04:01:17.000000Z",
                "toolu_radio_ack",
                "Bash",
                fixture_json!({ "command": "agent-portal message send peer \"ack\"" }),
            ));
            messages.push(tool_result(
                "2026-07-20T04:01:19.000000Z",
                "toolu_radio_ack",
                "Delivered (seq 42).",
            ));
            messages.push(result(
                "2026-07-20T04:01:21.000000Z",
                "claude-sonnet-4-5-20250929",
                740,
                150,
            ));
        }
        AgentType::Codex => {
            messages.push(codex_event(
                "2026-07-20T04:01:15.000000Z",
                fixture_json!({
                    "type": "item.completed",
                    "item": {
                        "type": "agent_message",
                        "id": "msg_radio_codex",
                        "text": format!("Preparing a short broadcast for the {peer_label} peer. The next cards use the production inter-agent message renderer.")
                    }
                }),
            ));
            messages.push(codex_event(
                "2026-07-20T04:01:17.000000Z",
                fixture_json!({
                    "type": "item.completed",
                    "item": {
                        "type": "command_execution",
                        "id": "cmd_radio_ack",
                        "command": "agent-portal message send peer \"ack\"",
                        "aggregated_output": "Delivered (seq 42).",
                        "exit_code": 0,
                        "status": "completed"
                    }
                }),
            ));
            messages.push(codex_event(
                "2026-07-20T04:01:19.000000Z",
                fixture_json!({
                    "type": "turn.completed",
                    "duration_ms": 3600,
                    "turn_id": "turn_radio_codex",
                    "status": "completed"
                }),
            ));
        }
    }

    messages
}

fn user(created_at: &str, user_id: Uuid, content: &str) -> DemoMessage {
    DemoMessage {
        content: fixture_json!({
            "type": "user",
            "content": content,
        })
        .to_string(),
        meta: meta(
            created_at,
            Some(MessageSource::Human {
                account_id: user_id,
                name: "Demo User".to_string(),
            }),
        ),
    }
}

fn assistant_text(created_at: &str, model_label: &str, text: &str) -> DemoMessage {
    assistant_blocks(
        created_at,
        model_label,
        vec![fixture_json!({
            "type": "text",
            "text": text,
        })],
    )
}

fn assistant_multi(created_at: &str, parts: Vec<&str>) -> DemoMessage {
    let blocks = parts
        .into_iter()
        .map(|text| {
            fixture_json!({
                "type": "text",
                "text": text,
            })
        })
        .collect();
    assistant_blocks(created_at, "Claude Sonnet 4.5", blocks)
}

fn assistant_tool(
    created_at: &str,
    tool_use_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) -> DemoMessage {
    assistant_blocks(
        created_at,
        "Claude Sonnet 4.5",
        vec![fixture_json!({
            "type": "tool_use",
            "id": tool_use_id,
            "name": tool_name,
            "input": input,
        })],
    )
}

fn assistant_blocks(
    created_at: &str,
    model_label: &str,
    blocks: Vec<serde_json::Value>,
) -> DemoMessage {
    let model = match model_label {
        "Claude Opus 4.8" => "claude-opus-4-8-20260115",
        _ => "claude-sonnet-4-5-20250929",
    };
    DemoMessage {
        content: fixture_json!({
            "type": "assistant",
            "message": {
                "id": format!("msg_{}", created_at.replace([':', '.', '-'], "")),
                "role": "assistant",
                "model": model,
                "content": blocks,
                "usage": {
                    "input_tokens": 860,
                    "output_tokens": 180,
                    "cache_creation_input_tokens": 64,
                    "cache_read_input_tokens": 256,
                    "service_tier": "standard"
                }
            },
            "session_id": "20000000-0000-4000-8000-000000000001",
        })
        .to_string(),
        meta: meta(created_at, None),
    }
}

fn claude_system(created_at: &str, mut fields: serde_json::Value) -> DemoMessage {
    if let Some(object) = fields.as_object_mut() {
        object.insert(
            "type".to_string(),
            serde_json::Value::String("system".to_string()),
        );
    }
    DemoMessage {
        content: fields.to_string(),
        meta: meta(created_at, None),
    }
}

fn tool_result(created_at: &str, tool_use_id: &str, content: &str) -> DemoMessage {
    DemoMessage {
        content: fixture_json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                }]
            },
            "session_id": "20000000-0000-4000-8000-000000000001",
        })
        .to_string(),
        meta: meta(created_at, None),
    }
}

fn result(created_at: &str, model: &str, input_tokens: u64, output_tokens: u64) -> DemoMessage {
    DemoMessage {
        content: fixture_json!({
            "type": "result",
            "subtype": "success",
            "duration_ms": 8600,
            "duration_api_ms": 7200,
            "is_error": false,
            "num_turns": 1,
            "result": "Demo turn complete",
            "session_id": "20000000-0000-4000-8000-000000000001",
            "total_cost_usd": 0.0187,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "cache_creation_input_tokens": 64,
                "cache_read_input_tokens": 512,
                "service_tier": "standard"
            },
            "model": model,
        })
        .to_string(),
        meta: meta(created_at, None),
    }
}

fn result_error(created_at: &str) -> DemoMessage {
    DemoMessage {
        content: fixture_json!({
            "type": "result",
            "subtype": "error_during_execution",
            "duration_ms": 2100,
            "duration_api_ms": 1800,
            "is_error": true,
            "num_turns": 1,
            "result": "API Error: 529 {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Synthetic overload for demo coverage\"},\"request_id\":\"req_demo_529\"}",
            "session_id": demo_uuid(1).to_string(),
            "total_cost_usd": 0.0021,
            "usage": {
                "input_tokens": 420,
                "output_tokens": 12,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 64,
                "service_tier": "standard"
            },
            "api_error_status": 529,
            "errors": ["Synthetic execution error for visual coverage"],
            "permission_denials": [{
                "tool_name": "Bash",
                "tool_input": { "command": "rm -rf /tmp/demo" },
                "tool_use_id": "toolu_denied_demo"
            }]
        })
        .to_string(),
        meta: meta(created_at, None),
    }
}

fn portal_image(created_at: &str) -> DemoMessage {
    DemoMessage {
        content: shared::PortalMessage::with_content(vec![shared::PortalContent::Image {
            media_type: "image/png".to_string(),
            data: "/wiggum.png".to_string(),
            file_path: Some("/demo/assets/wiggum.png".to_string()),
            file_size: Some(42_000),
            source_type: Some("url".to_string()),
        }])
        .to_json()
        .to_string(),
        meta: meta(created_at, Some(MessageSource::Portal)),
    }
}

fn portal_text_message(created_at: &str, text: &str) -> DemoMessage {
    DemoMessage {
        content: shared::PortalMessage::text(text.to_string())
            .to_json()
            .to_string(),
        meta: meta(created_at, Some(MessageSource::Portal)),
    }
}

fn portal_reminder(created_at: &str) -> DemoMessage {
    DemoMessage {
        content: shared::PortalMessage::reminder(
            "Demo reminder".to_string(),
            "This collapsible reminder uses the same markdown renderer as the bundled portal feature reminder.\n\n$$x_{next}=x_t+v_t$$".to_string(),
        )
        .to_json()
        .to_string(),
        meta: meta(created_at, Some(MessageSource::Portal)),
    }
}

fn portal_continuation(created_at: &str) -> DemoMessage {
    DemoMessage {
        content: shared::PortalMessage::continuation_prompt(
            demo_uuid(900),
            "2026-07-20T04:10:00.000000Z".to_string(),
            "pending".to_string(),
            "Synthetic max-token stop from the demo harness.".to_string(),
            shared::CONTINUATION_REASON_LIMIT.to_string(),
        )
        .to_json()
        .to_string(),
        meta: meta(created_at, Some(MessageSource::Portal)),
    }
}

fn portal_agent_message(
    created_at: &str,
    from_session_id: Uuid,
    from_agent_type: AgentType,
    text: &str,
) -> DemoMessage {
    DemoMessage {
        content: shared::PortalMessage::agent_message(
            from_agent_type.as_str().to_string(),
            from_session_id.to_string(),
            text.to_string(),
        )
        .to_json()
        .to_string(),
        meta: meta(created_at, Some(MessageSource::Portal)),
    }
}

fn agent_message_from_peer(
    created_at: &str,
    from_session_id: Uuid,
    from_agent_type: AgentType,
    text: &str,
) -> DemoMessage {
    DemoMessage {
        content: serde_json::Value::String(text.to_string()).to_string(),
        meta: meta(
            created_at,
            Some(MessageSource::Agent {
                session_id: from_session_id,
                agent_type: from_agent_type.as_str().to_string(),
            }),
        ),
    }
}

fn rate_limit(created_at: &str) -> DemoMessage {
    DemoMessage {
        content: fixture_json!({
            "type": "rate_limit_event",
            "session_id": demo_uuid(1).to_string(),
            "rate_limit_info": {
                "status": "allowed_warning",
                "resetsAt": 1784592000_u64,
                "rateLimitType": "five_hour",
                "utilization": 0.82,
                "overageStatus": "allowed_warning",
                "isUsingOverage": false,
                "surpassedThreshold": 0.8,
                "canUserPurchaseCredits": true,
                "hasChargeableSavedPaymentMethod": true
            },
            "uuid": "rate_demo_1"
        })
        .to_string(),
        meta: meta(created_at, None),
    }
}

fn claude_api_error(created_at: &str) -> DemoMessage {
    DemoMessage {
        content: fixture_json!({
            "type": "error",
            "error": {
                "type": "api_error",
                "message": "Synthetic Anthropic API error for renderer coverage."
            },
            "request_id": "req_demo_api_error"
        })
        .to_string(),
        meta: meta(created_at, None),
    }
}

fn codex_event(created_at: &str, value: serde_json::Value) -> DemoMessage {
    DemoMessage {
        content: value.to_string(),
        meta: meta(created_at, None),
    }
}

fn raw_unknown(created_at: &str, message_type: &str) -> DemoMessage {
    DemoMessage {
        content: fixture_json!({
            "type": message_type,
            "payload": {
                "note": "Raw fallback renderer coverage for future or unsupported wire frames."
            }
        })
        .to_string(),
        meta: meta(created_at, None),
    }
}

fn meta(created_at: &str, source: Option<MessageSource>) -> PortalMeta {
    PortalMeta {
        created_at: Some(created_at.to_string()),
        source,
        delivery: None,
    }
}

fn current_user_uuid() -> Uuid {
    Uuid::from_u128(0x10000000000040008000000000000001)
}

fn demo_uuid(offset: u128) -> Uuid {
    Uuid::from_u128(0x20000000000040008000000000000000 + offset)
}

fn demo_png_base64() -> &'static str {
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::codex_renderer::CodexEvent;

    #[test]
    fn demo_fixtures_parse_through_real_renderer_shapes() {
        for scenario in demo_scenarios() {
            let current_user_id = current_user_uuid().to_string();
            let rendered = scenario
                .messages
                .iter()
                .map(|message| {
                    crate::components::message_renderer::RenderedMessage::new(
                        message.content.clone(),
                        Some(message.meta.clone()),
                    )
                })
                .collect::<Vec<_>>();
            let groups = group_messages(
                &rendered,
                scenario.session.agent_type,
                Some(current_user_id.as_str()),
            );
            assert!(
                !groups.is_empty(),
                "demo scenario {} should produce renderable groups",
                scenario.session.session_name
            );

            for message in &scenario.messages {
                if matches!(
                    message.meta.source,
                    Some(MessageSource::Agent { .. }) | Some(MessageSource::Portal)
                ) {
                    continue;
                }
                let value = match serde_json::from_str::<serde_json::Value>(&message.content) {
                    Ok(value) => value,
                    Err(err) => panic!("demo fixture message should be JSON: {err}"),
                };
                let message_type = value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();

                match scenario.session.agent_type {
                    AgentType::Claude
                        if matches!(message_type, "assistant" | "result")
                            || (message_type == "user" && value.get("message").is_some()) =>
                    {
                        if let Err(err) = serde_json::from_value::<shared::ClaudeOutput>(value) {
                            panic!("Claude demo fixture should parse as ClaudeOutput: {err}");
                        }
                    }
                    AgentType::Codex if message_type != "user" => {
                        if let Err(err) = serde_json::from_value::<CodexEvent>(value) {
                            panic!("Codex demo fixture should parse as CodexEvent: {err}");
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
