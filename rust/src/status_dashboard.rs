use serde_json::Value as JsonValue;

use crate::config::Settings;
use crate::orchestrator::Snapshot;

pub fn humanize_codex_message(message: Option<&JsonValue>) -> String {
    match message {
        None => "no codex message yet".to_string(),
        Some(message) => truncate(&humanize_message_inner(message), 140),
    }
}

pub fn format_snapshot_content_for_test(
    snapshot: Option<&Snapshot>,
    settings: &Settings,
    tps: f64,
    terminal_columns: Option<usize>,
) -> String {
    format_snapshot_content(
        snapshot,
        settings,
        dashboard_url(settings),
        tps,
        terminal_columns,
    )
}

pub fn format_snapshot_content(
    snapshot: Option<&Snapshot>,
    settings: &Settings,
    dashboard_url: Option<String>,
    tps: f64,
    terminal_columns: Option<usize>,
) -> String {
    let width = terminal_columns.unwrap_or(115).max(80);
    let mut lines = vec!["╭─ SYMPHONY STATUS".to_string()];

    match snapshot {
        Some(snapshot) => {
            lines.push(format!(
                "│ Agents: {}/{}",
                snapshot.running.len(),
                settings.agent.max_concurrent_agents
            ));
            lines.push(format!("│ Throughput: {:.1} tps", tps));
            lines.push(format!(
                "│ Runtime: {}",
                format_runtime_seconds(snapshot.codex_totals.seconds_running)
            ));
            lines.push(format!(
                "│ Tokens: in {} | out {} | total {}",
                format_count(snapshot.codex_totals.input_tokens),
                format_count(snapshot.codex_totals.output_tokens),
                format_count(snapshot.codex_totals.total_tokens)
            ));
            lines.push(format!(
                "│ Rate Limits: {}",
                format_rate_limits(snapshot.rate_limits.as_ref())
            ));
            lines.extend(format_project_link_lines(settings, dashboard_url));
            lines.push(format_project_refresh_line(Some(&snapshot.polling)));
            lines.push("├─ Running".to_string());
            lines.push("│".to_string());

            if snapshot.running.is_empty() {
                lines.push("│ (none)".to_string());
            } else {
                for entry in &snapshot.running {
                    let age = format_runtime_seconds(entry.runtime_seconds);
                    let event = humanize_codex_message(entry.last_codex_message.as_ref());
                    let row = format!(
                        "│ {:<8} {:<14} turns={:<3} age={:<10} tokens={:<8} {}",
                        entry.identifier,
                        entry.state,
                        entry.turn_count,
                        age,
                        format_count(entry.codex_total_tokens),
                        event
                    );
                    lines.push(truncate(&sanitize_line(&row), width));
                }
            }

            lines.push("│".to_string());
            lines.push("├─ Backoff queue".to_string());
            lines.push("│".to_string());

            if snapshot.retrying.is_empty() {
                lines.push("│ (none)".to_string());
            } else {
                for entry in &snapshot.retrying {
                    let identifier = entry.identifier.as_deref().unwrap_or(&entry.issue_id);
                    let error = entry
                        .error
                        .as_deref()
                        .map(sanitize_line)
                        .unwrap_or_else(|| "retry scheduled".to_string());
                    let row = format!(
                        "│ {:<8} attempt={} due_in={} error={}",
                        identifier,
                        entry.attempt,
                        format_due_in(entry.due_in_ms),
                        error
                    );
                    lines.push(truncate(&row, width));
                }
            }
        }
        None => {
            lines.push("│ Orchestrator snapshot unavailable".to_string());
            lines.push(format!("│ Throughput: {:.1} tps", tps));
            lines.extend(format_project_link_lines(settings, dashboard_url));
            lines.push("│ Next refresh: n/a".to_string());
        }
    }

    lines.push(
        "╰──────────────────────────────────────────────────────────────────────────────"
            .to_string(),
    );
    lines.join("\n")
}

fn humanize_message_inner(message: &JsonValue) -> String {
    if let Some(object) = message.as_object() {
        let event = object.get("event").and_then(JsonValue::as_str);
        let payload = object.get("message").or_else(|| object.get("payload"));

        if let Some(text) = event.and_then(|event| humanize_codex_event(event, object, payload)) {
            return text;
        }
        if let Some(payload) = payload {
            return humanize_codex_payload(payload);
        }
        if let Some(method) = object.get("method").and_then(JsonValue::as_str) {
            return humanize_codex_method(method, message);
        }
    }
    humanize_codex_payload(message)
}

fn humanize_codex_event(
    event: &str,
    object: &serde_json::Map<String, JsonValue>,
    payload: Option<&JsonValue>,
) -> Option<String> {
    match event {
        "session_started" => {
            let session_id = object
                .get("session_id")
                .and_then(JsonValue::as_str)
                .or_else(|| payload.and_then(|payload| map_value(payload, &["session_id"])));
            Some(match session_id {
                Some(session_id) => format!("session started ({session_id})"),
                None => "session started".to_string(),
            })
        }
        "turn_input_required" => Some("turn blocked: waiting for user input".to_string()),
        "approval_auto_approved" => {
            let method = payload.and_then(|payload| {
                map_value(payload, &["method"])
                    .or_else(|| map_path(payload, &["payload", "method"]))
            });
            let decision = object.get("decision").and_then(JsonValue::as_str);
            let base = method
                .map(|method| {
                    format!(
                        "{} (auto-approved)",
                        humanize_codex_method(method, payload.unwrap_or(&JsonValue::Null))
                    )
                })
                .unwrap_or_else(|| "approval request auto-approved".to_string());
            Some(match decision {
                Some(decision) => format!("{base}: {decision}"),
                None => base,
            })
        }
        "tool_input_auto_answered" => {
            let answer = object.get("answer").and_then(JsonValue::as_str);
            let base = "tool input requested (auto-answered)";
            Some(match answer {
                Some(answer) => format!("{base}: {}", sanitize_line(answer)),
                None => base.to_string(),
            })
        }
        "tool_call_completed" => Some(humanize_dynamic_tool_event(
            "dynamic tool call completed",
            payload,
        )),
        "tool_call_failed" => Some(humanize_dynamic_tool_event(
            "dynamic tool call failed",
            payload,
        )),
        "unsupported_tool_call" => Some(humanize_dynamic_tool_event(
            "unsupported dynamic tool call rejected",
            payload,
        )),
        "turn_ended_with_error" => Some(format!(
            "turn ended with error: {}",
            format_reason(object.get("reason"))
        )),
        "startup_failed" => Some(format!(
            "startup failed: {}",
            format_reason(object.get("reason"))
        )),
        "turn_failed" => Some("turn failed".to_string()),
        "turn_cancelled" => Some("turn cancelled".to_string()),
        "malformed" => Some("malformed JSON event from codex".to_string()),
        _ => None,
    }
}

fn humanize_dynamic_tool_event(prefix: &str, payload: Option<&JsonValue>) -> String {
    let tool_name = payload.and_then(|payload| {
        map_path(payload, &["params", "tool"]).or_else(|| map_path(payload, &["params", "name"]))
    });
    match tool_name {
        Some(tool_name) => format!("{prefix}: {tool_name}"),
        None => prefix.to_string(),
    }
}

fn humanize_codex_payload(payload: &JsonValue) -> String {
    match payload {
        JsonValue::String(text) => sanitize_line(text),
        JsonValue::Object(_) => {
            if let Some(method) = map_value(payload, &["method"]) {
                humanize_codex_method(method, payload)
            } else if let Some(error) = payload.get("error") {
                format!("error: {}", sanitize_line(&error.to_string()))
            } else {
                sanitize_line(&payload.to_string())
            }
        }
        _ => sanitize_line(&payload.to_string()),
    }
}

fn humanize_codex_method(method: &str, payload: &JsonValue) -> String {
    match method {
        "thread/started" => map_path(payload, &["params", "thread", "id"])
            .map(|thread_id| format!("thread started ({thread_id})"))
            .unwrap_or_else(|| "thread started".to_string()),
        "turn/started" => map_path(payload, &["params", "turn", "id"])
            .map(|turn_id| format!("turn started ({turn_id})"))
            .unwrap_or_else(|| "turn started".to_string()),
        "turn/completed" => {
            let status = map_path(payload, &["params", "turn", "status"]).unwrap_or("completed");
            let usage = payload
                .pointer("/params/usage")
                .or_else(|| payload.pointer("/params/tokenUsage"))
                .or_else(|| payload.pointer("/usage"));
            match usage.and_then(format_usage_counts) {
                Some(usage) => format!("turn completed ({status}) ({usage})"),
                None => format!("turn completed ({status})"),
            }
        }
        "thread/tokenUsage/updated" => payload
            .pointer("/params/tokenUsage/total")
            .and_then(format_usage_counts)
            .map(|usage| format!("thread token usage updated ({usage})"))
            .unwrap_or_else(|| "thread token usage updated".to_string()),
        "codex/event/token_count" => "token count updated".to_string(),
        "item/tool/requestUserInput" => "tool input requested".to_string(),
        "item/commandExecution/requestApproval" => "command approval requested".to_string(),
        "item/fileChange/requestApproval" => "file change approval requested".to_string(),
        "turn/failed" => "turn failed".to_string(),
        other => sanitize_line(other),
    }
}

fn format_usage_counts(usage: &JsonValue) -> Option<String> {
    let input = get_usage_value(usage, &["input_tokens", "prompt_tokens", "inputTokens"])?;
    let output = get_usage_value(
        usage,
        &["output_tokens", "completion_tokens", "outputTokens"],
    )?;
    let total = get_usage_value(usage, &["total_tokens", "totalTokens", "total"])?;
    Some(format!(
        "in {} | out {} | total {}",
        format_count(input),
        format_count(output),
        format_count(total)
    ))
}

fn get_usage_value(usage: &JsonValue, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        usage.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| {
                    value
                        .as_i64()
                        .filter(|value| *value >= 0)
                        .map(|value| value as u64)
                })
                .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
        })
    })
}

fn map_value<'a>(payload: &'a JsonValue, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(JsonValue::as_str))
}

fn map_path<'a>(payload: &'a JsonValue, path: &[&str]) -> Option<&'a str> {
    let mut current = payload;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str()
}

fn format_reason(reason: Option<&JsonValue>) -> String {
    reason
        .map(|reason| sanitize_line(&reason.to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn format_project_link_lines(settings: &Settings, dashboard_url: Option<String>) -> Vec<String> {
    let mut lines = vec![format!(
        "│ Project: {}",
        settings
            .tracker
            .project_slug
            .as_deref()
            .map(linear_project_url)
            .unwrap_or("n/a".to_string())
    )];
    if let Some(dashboard_url) = dashboard_url {
        lines.push(format!("│ Dashboard: {dashboard_url}"));
    }
    lines
}

fn dashboard_url(settings: &Settings) -> Option<String> {
    settings
        .server
        .port
        .map(|port| format!("http://{}:{port}/", settings.server.host))
}

fn linear_project_url(project_slug: &str) -> String {
    format!("https://linear.app/project/{project_slug}/issues")
}

fn format_project_refresh_line(polling: Option<&crate::orchestrator::PollingSnapshot>) -> String {
    match polling {
        Some(polling) if polling.checking => "│ Next refresh: checking now...".to_string(),
        Some(polling) => match polling.next_poll_in_ms {
            Some(next_poll_in_ms) => format!("│ Next refresh: {}s", next_poll_in_ms / 1000),
            None => "│ Next refresh: n/a".to_string(),
        },
        None => "│ Next refresh: n/a".to_string(),
    }
}

fn format_due_in(due_in_ms: u64) -> String {
    if due_in_ms < 1000 {
        format!("{due_in_ms}ms")
    } else {
        format!("{}s", due_in_ms / 1000)
    }
}

fn format_rate_limits(rate_limits: Option<&JsonValue>) -> String {
    let Some(rate_limits) = rate_limits else {
        return "unavailable".to_string();
    };
    let limit_id = map_value(rate_limits, &["limit_id", "limit_name"]).unwrap_or("n/a");
    let primary = rate_limits
        .get("primary")
        .map(|bucket| sanitize_line(&bucket.to_string()))
        .unwrap_or_else(|| "n/a".to_string());
    let secondary = rate_limits
        .get("secondary")
        .map(|bucket| sanitize_line(&bucket.to_string()))
        .unwrap_or_else(|| "n/a".to_string());
    let credits = rate_limits
        .get("credits")
        .map(|bucket| sanitize_line(&bucket.to_string()))
        .unwrap_or_else(|| "n/a".to_string());
    format!("{limit_id} | primary {primary} | secondary {secondary} | credits {credits}")
}

fn format_runtime_seconds(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn format_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn sanitize_line(value: &str) -> String {
    value
        .replace("\\n", " ")
        .replace(['\n', '\r'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        value.to_string()
    } else {
        let mut truncated = value
            .chars()
            .take(width.saturating_sub(3))
            .collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CliOverrides, Settings};
    use crate::orchestrator::{
        PollingSnapshot, RetrySnapshot, RunningSnapshot, Snapshot, TokenTotals,
    };
    use crate::workflow::LoadedWorkflow;
    use chrono::Utc;
    use serde_json::json;

    fn settings() -> Settings {
        Settings::from_workflow(
            &LoadedWorkflow {
                config: serde_yaml::from_str(
                    "tracker:\n  kind: memory\n  project_slug: demo\nserver:\n  port: 4000\n",
                )
                .unwrap(),
                prompt_template: String::new(),
                prompt: String::new(),
            },
            &CliOverrides::default(),
        )
        .unwrap()
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            running: vec![RunningSnapshot {
                issue_id: "issue-1".to_string(),
                identifier: "MT-101".to_string(),
                state: "In Progress".to_string(),
                worker_host: None,
                workspace_path: None,
                session_id: Some("thread-1-turn-1".to_string()),
                codex_app_server_pid: Some("4242".to_string()),
                codex_input_tokens: 100,
                codex_output_tokens: 25,
                codex_total_tokens: 125,
                turn_count: 3,
                started_at: Utc::now(),
                last_codex_timestamp: None,
                last_codex_message: Some(json!({
                    "event": "approval_auto_approved",
                    "message": { "method": "item/commandExecution/requestApproval" },
                    "decision": "acceptForSession"
                })),
                last_codex_event: Some("approval_auto_approved".to_string()),
                runtime_seconds: 90,
            }],
            retrying: vec![RetrySnapshot {
                issue_id: "issue-2".to_string(),
                attempt: 2,
                due_in_ms: 1_500,
                identifier: Some("MT-202".to_string()),
                error: Some("error with \\nnewline".to_string()),
                worker_host: None,
                workspace_path: None,
            }],
            codex_totals: TokenTotals {
                input_tokens: 100,
                output_tokens: 25,
                total_tokens: 125,
                seconds_running: 90,
            },
            rate_limits: Some(json!({
                "limit_id": "gpt-5",
                "primary": { "remaining": 10, "limit": 20 },
                "credits": { "unlimited": true }
            })),
            polling: PollingSnapshot {
                checking: false,
                next_poll_in_ms: Some(5_000),
                poll_interval_ms: 30_000,
            },
        }
    }

    #[test]
    fn humanizes_key_codex_messages() {
        assert_eq!(humanize_codex_message(None), "no codex message yet");
        assert_eq!(
            humanize_codex_message(Some(&json!({
                "event": "session_started",
                "message": { "session_id": "thread-1-turn-1" }
            }))),
            "session started (thread-1-turn-1)"
        );
        assert!(
            humanize_codex_message(Some(&json!({
                "event": "tool_call_failed",
                "message": { "params": { "tool": "linear_graphql" } }
            })))
            .contains("dynamic tool call failed")
        );
        assert_eq!(
            humanize_codex_message(Some(&json!({
                "event": "malformed",
                "message": "{\"method\":\"turn/completed\""
            }))),
            "malformed JSON event from codex"
        );
    }

    #[test]
    fn formats_snapshot_content_with_running_and_retry_rows() {
        let rendered =
            format_snapshot_content_for_test(Some(&snapshot()), &settings(), 42.0, Some(115));
        assert!(rendered.contains("SYMPHONY STATUS"));
        assert!(rendered.contains("MT-101"));
        assert!(rendered.contains("MT-202"));
        assert!(rendered.contains("approval"));
        assert!(rendered.contains("error=error with newline"));
        assert!(rendered.contains("https://linear.app/project/demo/issues"));
        assert!(rendered.contains("http://127.0.0.1:4000/"));
    }
}
