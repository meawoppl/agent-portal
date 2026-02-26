//! Poke a message into an active portal session via WebSocket.
//!
//! Connects as a web client, sends a message, and prints all responses
//! with detailed output for task-related system messages.

use anyhow::{bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub async fn poke_session(server: &str, session_id: &str, message: &str) -> Result<()> {
    let ws_url = server
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let url = format!("{}/ws/client", ws_url);

    eprintln!("Connecting to {}...", url);
    let (ws_stream, _) = connect_async(&url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Register for the session
    let register = serde_json::json!({
        "type": "Register",
        "session_id": session_id,
        "session_name": session_id,
        "auth_token": null,
        "working_directory": "",
        "resuming": false,
        "git_branch": null,
        "replay_after": null,
        "client_version": null,
        "replaces_session_id": null,
        "hostname": null,
        "launcher_id": null,
        "agent_type": "claude",
    });
    write.send(Message::Text(register.to_string())).await?;
    eprintln!("Registered for session {}", session_id);

    // Drain history batch
    let mut got_history = false;
    while let Some(Ok(msg)) = read.next().await {
        if let Message::Text(text) = msg {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                let msg_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                if msg_type == "HistoryBatch" {
                    let count = parsed
                        .get("messages")
                        .and_then(|m| m.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    eprintln!("← HistoryBatch ({} messages)", count);
                    got_history = true;
                    break;
                }
                eprintln!("← {}", msg_type);
            }
        }
    }
    if !got_history {
        bail!("Connection closed before receiving history");
    }

    // Send the input
    let input = serde_json::json!({
        "type": "ClaudeInput",
        "content": {
            "type": "human_turn_input",
            "content": message,
        },
    });
    write.send(Message::Text(input.to_string())).await?;
    eprintln!("→ Sent: {}", message);
    eprintln!("---");

    // Listen for responses
    while let Some(Ok(msg)) = read.next().await {
        let Message::Text(text) = msg else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
            continue;
        };

        let msg_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("?");

        if msg_type == "ClaudeOutput" {
            let content = parsed.get("content").cloned().unwrap_or(Value::Null);
            let inner_type = content.get("type").and_then(|t| t.as_str()).unwrap_or("?");
            let subtype = content
                .get("subtype")
                .and_then(|s| s.as_str())
                .unwrap_or("");

            match inner_type {
                "system" => {
                    let label = if subtype.is_empty() {
                        "system".to_string()
                    } else {
                        format!("system/{}", subtype)
                    };
                    // Show task messages in full detail
                    if subtype.starts_with("task_") {
                        println!("← {}: {}", label, serde_json::to_string_pretty(&content)?);

                        // Also try parsing as ClaudeOutput to test roundtrip
                        match serde_json::from_value::<shared::ClaudeOutput>(content.clone()) {
                            Ok(shared::ClaudeOutput::System(ref sys)) => {
                                if sys.is_task_started() {
                                    match sys.as_task_started() {
                                        Some(task) => println!(
                                            "   ✓ as_task_started OK: id={} type={:?}",
                                            task.task_id, task.task_type
                                        ),
                                        None => println!(
                                            "   ✗ as_task_started FAILED (subtype matched, struct parse failed)"
                                        ),
                                    }
                                }
                                if sys.is_task_progress() {
                                    match sys.as_task_progress() {
                                        Some(p) => {
                                            println!("   ✓ as_task_progress OK: id={}", p.task_id)
                                        }
                                        None => println!("   ✗ as_task_progress FAILED"),
                                    }
                                }
                                if sys.is_task_notification() {
                                    match sys.as_task_notification() {
                                        Some(n) => println!(
                                            "   ✓ as_task_notification OK: id={} status={:?}",
                                            n.task_id, n.status
                                        ),
                                        None => println!("   ✗ as_task_notification FAILED"),
                                    }
                                }
                            }
                            Ok(_) => println!("   ? parsed as non-System ClaudeOutput"),
                            Err(e) => println!("   ✗ ClaudeOutput parse failed: {}", e),
                        }
                    } else {
                        eprintln!("← {}", label);
                    }
                }
                "assistant" => {
                    let blocks = content
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array());
                    if let Some(blocks) = blocks {
                        for block in blocks {
                            let block_type =
                                block.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                            match block_type {
                                "text" => {
                                    let text =
                                        block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                    let preview: String = text.chars().take(120).collect();
                                    eprintln!("← assistant/text: {}", preview);
                                }
                                "tool_use" => {
                                    let name =
                                        block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                    eprintln!("← assistant/tool_use: {}", name);
                                }
                                _ => eprintln!("← assistant/{}", block_type),
                            }
                        }
                    }
                }
                "result" => {
                    let cost = content.get("total_cost_usd").and_then(|c| c.as_f64());
                    eprintln!("← result (cost: ${:.4})", cost.unwrap_or(0.0));
                    break;
                }
                _ => {
                    eprintln!("← {}", inner_type);
                }
            }
        } else {
            eprintln!("← {}", msg_type);
        }
    }

    Ok(())
}
