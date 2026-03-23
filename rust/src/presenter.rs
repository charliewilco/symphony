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
    let terminal_view =
        status_dashboard::render_snapshot_html_for_test(Some(snapshot), settings, 0.0, Some(115));
    let rate_limits = pretty_json(snapshot.rate_limits.as_ref());
    let running_rows = render_running_rows(snapshot);
    let retry_rows = render_retry_rows(snapshot);
    let runtime = status_dashboard_runtime(snapshot);
    let mut html = String::new();
    html.push_str("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str("<title>Symphony Observability</title>");
    html.push_str("<link rel=\"stylesheet\" href=\"/dashboard.css\">");
    html.push_str("<script defer src=\"/vendor/phoenix_html/phoenix_html.js\"></script>");
    html.push_str("<script defer src=\"/vendor/phoenix/phoenix.js\"></script>");
    html.push_str("<script defer src=\"/vendor/phoenix_live_view/phoenix_live_view.js\"></script>");
    html.push_str("</head><body>");
    html.push_str(
        "<main class=\"app-shell phx-connected\" data-phx-main><section class=\"dashboard-shell\">",
    );
    html.push_str("<header class=\"hero-card\"><div class=\"hero-grid\"><div>");
    html.push_str("<p class=\"eyebrow\">Symphony Observability</p>");
    html.push_str("<h1 class=\"hero-title\">Operations Dashboard</h1>");
    html.push_str("<p class=\"hero-copy\">Current state, retry pressure, token usage, and orchestration health for the active Symphony runtime.</p>");
    html.push_str("</div><div class=\"status-stack\">");
    html.push_str("<span class=\"status-badge status-badge-live\"><span class=\"status-badge-dot\"></span>Live</span>");
    html.push_str("<span class=\"status-badge status-badge-offline\"><span class=\"status-badge-dot\"></span>Offline</span>");
    html.push_str("</div></div></header>");
    html.push_str("<section class=\"metric-grid\">");
    html.push_str(&format!(
        "<article class=\"metric-card\"><p class=\"metric-label\">Running</p><p class=\"metric-value numeric\">{}</p><p class=\"metric-detail\">Active issue sessions in the current runtime.</p></article>",
        snapshot.running.len()
    ));
    html.push_str(&format!(
        "<article class=\"metric-card\"><p class=\"metric-label\">Retrying</p><p class=\"metric-value numeric\">{}</p><p class=\"metric-detail\">Issues waiting for the next retry window.</p></article>",
        snapshot.retrying.len()
    ));
    html.push_str(&format!(
        "<article class=\"metric-card\"><p class=\"metric-label\">Total tokens</p><p class=\"metric-value numeric\">{}</p><p class=\"metric-detail numeric\">In {} / Out {}</p></article>",
        format_int(snapshot.codex_totals.total_tokens),
        format_int(snapshot.codex_totals.input_tokens),
        format_int(snapshot.codex_totals.output_tokens)
    ));
    html.push_str(&format!(
        "<article class=\"metric-card\"><p class=\"metric-label\">Runtime</p><p class=\"metric-value numeric\">{}</p><p class=\"metric-detail\">Total Codex runtime across completed and active sessions.</p></article>",
        runtime
    ));
    html.push_str("</section>");
    html.push_str("<section class=\"section-card\"><div class=\"section-header\"><div><h2 class=\"section-title\">Terminal status</h2><p class=\"section-copy\">Terminal-first observability view aligned with the Elixir runtime.</p></div></div><div class=\"terminal-frame\"><pre class=\"terminal-dashboard\">");
    html.push_str(&terminal_view);
    html.push_str("</pre></div></section>");
    html.push_str("<section class=\"section-card\"><div class=\"section-header\"><div><h2 class=\"section-title\">Rate limits</h2><p class=\"section-copy\">Latest upstream rate-limit snapshot, when available.</p></div></div><pre class=\"code-panel\">");
    html.push_str(&escape_html(&rate_limits));
    html.push_str("</pre></section>");
    html.push_str("<section class=\"section-card\"><div class=\"section-header\"><div><h2 class=\"section-title\">Running sessions</h2><p class=\"section-copy\">Active issues, last known agent activity, and token usage.</p></div></div>");
    html.push_str(&running_rows);
    html.push_str("</section>");
    html.push_str("<section class=\"section-card\"><div class=\"section-header\"><div><h2 class=\"section-title\">Retry queue</h2><p class=\"section-copy\">Issues waiting for the next retry window.</p></div></div>");
    html.push_str(&retry_rows);
    html.push_str("</section>");
    html.push_str("</section></main></body></html>");
    html
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

fn render_running_rows(snapshot: &Snapshot) -> String {
    if snapshot.running.is_empty() {
        return "<p class=\"empty-state\">No active sessions.</p>".to_string();
    }

    let mut rows = snapshot.running.clone();
    rows.sort_by(|left, right| left.identifier.cmp(&right.identifier));

    let body = rows
        .iter()
        .map(render_running_row)
        .collect::<Vec<_>>()
        .join("");

    format!(
        concat!(
            "<div class=\"table-wrap\"><table class=\"data-table data-table-running\">",
            "<colgroup><col style=\"width: 12rem;\"><col style=\"width: 9rem;\"><col style=\"width: 10rem;\"><col style=\"width: 9rem;\"><col><col style=\"width: 11rem;\"></colgroup>",
            "<thead><tr><th>Issue</th><th>State</th><th>Session</th><th>Runtime / turns</th><th>Codex update</th><th>Tokens</th></tr></thead>",
            "<tbody>{}</tbody></table></div>"
        ),
        body
    )
}

fn render_running_row(entry: &RunningSnapshot) -> String {
    let last_message = status_dashboard::humanize_codex_message(entry.last_codex_message.as_ref());
    let event = entry.last_codex_event.as_deref().unwrap_or("n/a");
    let session = entry.session_id.as_deref().unwrap_or("n/a");
    let session_compact = compact_session_id(entry.session_id.as_deref());
    let last_event_at = iso8601(entry.last_codex_timestamp).unwrap_or_else(|| "n/a".to_string());
    format!(
        concat!(
            "<tr>",
            "<td class=\"running-col running-col-issue\"><div class=\"issue-stack\"><span class=\"issue-id\">{}</span><a class=\"issue-link\" href=\"/api/v1/{}\">JSON details</a></div></td>",
            "<td class=\"running-col running-col-state\"><span class=\"state-badge {}\">{}</span></td>",
            "<td class=\"running-col running-col-session\"><div class=\"session-stack\"><span class=\"mono\" title=\"{}\">{}</span><span class=\"muted\">{}</span></div></td>",
            "<td class=\"running-col running-col-runtime numeric\">{}</td>",
            "<td class=\"running-col running-col-event\"><div class=\"detail-stack\"><span class=\"event-text\" title=\"{}\">{}</span><span class=\"muted event-meta\">{} · <span class=\"mono numeric\">{}</span></span></div></td>",
            "<td class=\"running-col running-col-tokens numeric\"><div class=\"token-stack numeric\"><span>Total: {}</span><span class=\"muted\">In {} / Out {}</span></div></td>",
            "</tr>"
        ),
        escape_html(&entry.identifier),
        escape_html(&entry.identifier),
        state_badge_class(&entry.state),
        escape_html(&entry.state),
        escape_html(session),
        escape_html(&session_compact),
        escape_html(session),
        escape_html(&format_runtime_and_turns(
            entry.runtime_seconds,
            entry.turn_count
        )),
        escape_html(&last_message),
        escape_html(&last_message),
        escape_html(event),
        escape_html(&last_event_at),
        format_int(entry.codex_total_tokens),
        format_int(entry.codex_input_tokens),
        format_int(entry.codex_output_tokens),
    )
}

fn render_retry_rows(snapshot: &Snapshot) -> String {
    if snapshot.retrying.is_empty() {
        return "<p class=\"empty-state\">No issues are currently backing off.</p>".to_string();
    }

    let mut rows = snapshot.retrying.clone();
    rows.sort_by_key(|entry| entry.due_in_ms);
    let body = rows
        .iter()
        .map(render_retry_row)
        .collect::<Vec<_>>()
        .join("");

    format!(
        concat!(
            "<div class=\"table-wrap\"><table class=\"data-table\">",
            "<thead><tr><th>Issue</th><th>Attempt</th><th>Due in</th><th>Error</th></tr></thead>",
            "<tbody>{}</tbody></table></div>"
        ),
        body
    )
}

fn render_retry_row(entry: &RetrySnapshot) -> String {
    let identifier = entry.identifier.as_deref().unwrap_or(&entry.issue_id);
    format!(
        concat!(
            "<tr>",
            "<td><div class=\"issue-stack\"><span class=\"issue-id\">{}</span><a class=\"issue-link\" href=\"/api/v1/{}\">JSON details</a></div></td>",
            "<td class=\"numeric\">{}</td>",
            "<td class=\"mono\">{}</td>",
            "<td>{}</td>",
            "</tr>"
        ),
        escape_html(identifier),
        escape_html(identifier),
        entry.attempt,
        escape_html(&next_in_words(entry.due_in_ms)),
        escape_html(entry.error.as_deref().unwrap_or("n/a")),
    )
}

fn pretty_json(value: Option<&JsonValue>) -> String {
    value
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()))
        .unwrap_or_else(|| "null".to_string())
}

fn state_badge_class(state: &str) -> &'static str {
    match state {
        "Todo" => "state-badge--todo",
        "In Progress" => "state-badge--active",
        "Rework" => "state-badge--rework",
        "Done" => "state-badge--done",
        _ => "state-badge--neutral",
    }
}

fn format_int(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped.chars().rev().collect()
}

fn status_dashboard_runtime(snapshot: &Snapshot) -> String {
    format_runtime_seconds(snapshot.codex_totals.seconds_running)
}

fn format_runtime_seconds(seconds: u64) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    format!("{minutes}m {seconds}s")
}

fn format_runtime_and_turns(seconds: u64, turn_count: u64) -> String {
    if turn_count > 0 {
        format!("{} / {}", format_runtime_seconds(seconds), turn_count)
    } else {
        format_runtime_seconds(seconds)
    }
}

fn compact_session_id(session_id: Option<&str>) -> String {
    match session_id {
        None => "n/a".to_string(),
        Some(session_id) if session_id.chars().count() > 10 => {
            let start = session_id.chars().take(4).collect::<String>();
            let end = session_id
                .chars()
                .rev()
                .take(6)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            format!("{start}...{end}")
        }
        Some(session_id) => session_id.to_string(),
    }
}

fn next_in_words(due_in_ms: u64) -> String {
    let secs = due_in_ms / 1000;
    let millis = due_in_ms % 1000;
    format!("{secs}.{:03}s", millis)
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
        assert!(html.contains("hero-card"));
        assert!(html.contains("metric-card"));
        assert!(html.contains("data-table-running"));
        assert!(html.contains("Retry queue"));
    }
}
