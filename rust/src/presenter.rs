use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value as JsonValue, json};

use crate::config::Settings;
use crate::orchestrator::{RetrySnapshot, RunningSnapshot, Snapshot};
use crate::status_dashboard;

pub fn state_payload(snapshot: Result<&Snapshot, SnapshotError>) -> JsonValue {
    let generated_at = generated_at();
    match snapshot {
        Ok(snapshot) => json!({
            "generated_at": generated_at,
            "counts": {
                "running": snapshot.running.len(),
                "retrying": snapshot.retrying.len()
            },
            "running": snapshot.running.iter().map(project_running_entry).collect::<Vec<_>>(),
            "retrying": snapshot.retrying.iter().map(project_retry_entry).collect::<Vec<_>>(),
            "codex_totals": snapshot.codex_totals,
            "rate_limits": snapshot.rate_limits
        }),
        Err(SnapshotError::Timeout) => json!({
            "generated_at": generated_at,
            "error": {
                "code": "snapshot_timeout",
                "message": "Snapshot timed out"
            }
        }),
        Err(SnapshotError::Unavailable) => json!({
            "generated_at": generated_at,
            "error": {
                "code": "snapshot_unavailable",
                "message": "Snapshot unavailable"
            }
        }),
    }
}

pub fn issue_payload(
    snapshot: &Snapshot,
    issue_identifier: &str,
    settings: Option<&Settings>,
) -> Option<JsonValue> {
    let running = snapshot
        .running
        .iter()
        .find(|entry| entry.identifier == issue_identifier);
    let retry = snapshot
        .retrying
        .iter()
        .find(|entry| entry.identifier.as_deref() == Some(issue_identifier));

    if running.is_none() && retry.is_none() {
        return None;
    }

    let workspace_path = running
        .and_then(|entry| entry.workspace_path.clone())
        .or_else(|| retry.and_then(|entry| entry.workspace_path.clone()))
        .or_else(|| {
            settings.map(|settings| {
                settings
                    .workspace
                    .root
                    .join(issue_identifier)
                    .to_string_lossy()
                    .to_string()
            })
        })
        .unwrap_or_else(|| issue_identifier.to_string());

    Some(json!({
        "issue_identifier": issue_identifier,
        "issue_id": running.map(|entry| entry.issue_id.clone()).or_else(|| retry.map(|entry| entry.issue_id.clone())),
        "status": if running.is_some() { "running" } else { "retrying" },
        "workspace": {
            "path": workspace_path,
            "host": running.and_then(|entry| entry.worker_host.clone()).or_else(|| retry.and_then(|entry| entry.worker_host.clone()))
        },
        "attempts": {
            "restart_count": retry.map(|entry| entry.attempt.saturating_sub(1)).unwrap_or(0),
            "current_retry_attempt": retry.map(|entry| entry.attempt).unwrap_or(0)
        },
        "running": running.map(project_running_issue_body),
        "retry": retry.map(project_retry_issue_body),
        "logs": { "codex_session_logs": [] },
        "recent_events": running.map(project_recent_events).unwrap_or_else(|| json!([])),
        "last_error": retry.and_then(|entry| entry.error.clone()),
        "tracked": {}
    }))
}

pub fn render_dashboard_html(snapshot: &Snapshot, settings: &Settings) -> String {
    let terminal_view = status_dashboard::format_snapshot_content_for_test(
        Some(snapshot),
        settings,
        0.0,
        Some(115),
    );
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Symphony</title><link rel=\"stylesheet\" href=\"/dashboard.css\"><script defer src=\"/vendor/phoenix_html/phoenix_html.js\"></script><script defer src=\"/vendor/phoenix/phoenix.js\"></script><script defer src=\"/vendor/phoenix_live_view/phoenix_live_view.js\"></script></head><body><main data-phx-main><header><h1>Symphony</h1><p class=\"status-badge-live\">Live</p><p class=\"status-badge-offline\">Offline</p></header><section><pre class=\"terminal-dashboard\">{}</pre></section></main></body></html>",
        escape_html(&terminal_view)
    )
}

#[derive(Clone, Copy, Debug)]
pub enum SnapshotError {
    Timeout,
    Unavailable,
}

fn project_running_entry(entry: &RunningSnapshot) -> JsonValue {
    json!({
        "issue_id": entry.issue_id,
        "issue_identifier": entry.identifier,
        "state": entry.state,
        "worker_host": entry.worker_host,
        "workspace_path": entry.workspace_path,
        "session_id": entry.session_id,
        "turn_count": entry.turn_count,
        "last_event": entry.last_codex_event,
        "last_message": status_dashboard::humanize_codex_message(entry.last_codex_message.as_ref()),
        "started_at": iso8601(Some(entry.started_at)),
        "last_event_at": iso8601(entry.last_codex_timestamp),
        "tokens": {
            "input_tokens": entry.codex_input_tokens,
            "output_tokens": entry.codex_output_tokens,
            "total_tokens": entry.codex_total_tokens
        }
    })
}

fn project_retry_entry(entry: &RetrySnapshot) -> JsonValue {
    json!({
        "issue_id": entry.issue_id,
        "issue_identifier": entry.identifier,
        "attempt": entry.attempt,
        "due_at": due_at_iso8601(entry.due_in_ms),
        "error": entry.error,
        "worker_host": entry.worker_host,
        "workspace_path": entry.workspace_path
    })
}

fn project_running_issue_body(entry: &RunningSnapshot) -> JsonValue {
    json!({
        "worker_host": entry.worker_host,
        "workspace_path": entry.workspace_path,
        "session_id": entry.session_id,
        "turn_count": entry.turn_count,
        "state": entry.state,
        "started_at": iso8601(Some(entry.started_at)),
        "last_event": entry.last_codex_event,
        "last_message": status_dashboard::humanize_codex_message(entry.last_codex_message.as_ref()),
        "last_event_at": iso8601(entry.last_codex_timestamp),
        "tokens": {
            "input_tokens": entry.codex_input_tokens,
            "output_tokens": entry.codex_output_tokens,
            "total_tokens": entry.codex_total_tokens
        }
    })
}

fn project_retry_issue_body(entry: &RetrySnapshot) -> JsonValue {
    json!({
        "attempt": entry.attempt,
        "due_at": due_at_iso8601(entry.due_in_ms),
        "error": entry.error,
        "worker_host": entry.worker_host,
        "workspace_path": entry.workspace_path
    })
}

fn project_recent_events(entry: &RunningSnapshot) -> JsonValue {
    match entry.last_codex_timestamp {
        Some(timestamp) => json!([{
            "at": iso8601(Some(timestamp)),
            "event": entry.last_codex_event,
            "message": status_dashboard::humanize_codex_message(entry.last_codex_message.as_ref())
        }]),
        None => json!([]),
    }
}

fn due_at_iso8601(due_in_ms: u64) -> String {
    iso8601(Some(Utc::now() + TimeDelta::milliseconds(due_in_ms as i64))).unwrap()
}

fn iso8601(timestamp: Option<DateTime<Utc>>) -> Option<String> {
    timestamp.map(|value| value.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

fn generated_at() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CliOverrides, Settings};
    use crate::orchestrator::{PollingSnapshot, TokenTotals};
    use crate::workflow::LoadedWorkflow;

    fn settings() -> Settings {
        Settings::from_workflow(
            &LoadedWorkflow {
                config: serde_yaml::from_str("tracker:\n  kind: memory\n").unwrap(),
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
                issue_id: "issue-http".to_string(),
                identifier: "MT-HTTP".to_string(),
                state: "In Progress".to_string(),
                worker_host: None,
                workspace_path: None,
                session_id: Some("thread-http".to_string()),
                codex_app_server_pid: None,
                codex_input_tokens: 4,
                codex_output_tokens: 8,
                codex_total_tokens: 12,
                turn_count: 7,
                started_at: Utc::now(),
                last_codex_timestamp: None,
                last_codex_message: Some(JsonValue::String("rendered".to_string())),
                last_codex_event: Some("notification".to_string()),
                runtime_seconds: 42,
            }],
            retrying: vec![RetrySnapshot {
                issue_id: "issue-retry".to_string(),
                attempt: 2,
                due_in_ms: 5000,
                identifier: Some("MT-RETRY".to_string()),
                error: Some("boom".to_string()),
                worker_host: None,
                workspace_path: None,
            }],
            codex_totals: TokenTotals {
                input_tokens: 4,
                output_tokens: 8,
                total_tokens: 12,
                seconds_running: 42,
            },
            rate_limits: Some(json!({"primary": {"remaining": 11}})),
            polling: PollingSnapshot {
                checking: false,
                next_poll_in_ms: Some(5000),
                poll_interval_ms: 30000,
            },
        }
    }

    #[test]
    fn state_and_issue_payloads_humanize_messages() {
        let snapshot = snapshot();
        let state = state_payload(Ok(&snapshot));
        assert_eq!(state["running"][0]["last_message"], "rendered");

        let issue = issue_payload(&snapshot, "MT-HTTP", Some(&settings())).unwrap();
        assert_eq!(issue["running"]["last_message"], "rendered");
    }

    #[test]
    fn dashboard_html_embeds_terminal_dashboard_and_assets() {
        let html = render_dashboard_html(&snapshot(), &settings());
        assert!(html.contains("/dashboard.css"));
        assert!(html.contains("/vendor/phoenix_html/phoenix_html.js"));
        assert!(html.contains("terminal-dashboard"));
        assert!(html.contains("SYMPHONY STATUS"));
    }
}
