use serde_json::Value as JsonValue;

use crate::config::Settings;
use crate::orchestrator::Snapshot;

const RUNNING_ID_WIDTH: usize = 8;
const RUNNING_STAGE_WIDTH: usize = 14;
const RUNNING_PID_WIDTH: usize = 8;
const RUNNING_AGE_WIDTH: usize = 12;
const RUNNING_TOKENS_WIDTH: usize = 10;
const RUNNING_SESSION_WIDTH: usize = 14;
const RUNNING_EVENT_DEFAULT_WIDTH: usize = 44;
const RUNNING_EVENT_MIN_WIDTH: usize = 12;
const RUNNING_ROW_CHROME_WIDTH: usize = 10;

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
    let running_event_width = running_event_width(Some(width));
    let mut lines = vec!["╭─ SYMPHONY STATUS".to_string()];

    match snapshot {
        Some(snapshot) => {
            lines.push("├─ Status".to_string());
            lines.push("│".to_string());
            lines.push(format_three_columns(
                &format!(
                    "Agents: {}/{}",
                    snapshot.running.len(),
                    settings.agent.max_concurrent_agents
                ),
                &format!("Throughput: {} tps", format_tps(tps)),
                &format!(
                    "Runtime: {}",
                    format_runtime_seconds(snapshot.codex_totals.seconds_running)
                ),
                width,
            ));
            lines.push(format_three_columns(
                &format!(
                    "Tokens: in {} | out {} | total {}",
                    format_count(snapshot.codex_totals.input_tokens),
                    format_count(snapshot.codex_totals.output_tokens),
                    format_count(snapshot.codex_totals.total_tokens)
                ),
                &format!(
                    "Rate Limits: {}",
                    format_rate_limits(snapshot.rate_limits.as_ref())
                ),
                &format_project_refresh(Some(&snapshot.polling)),
                width,
            ));
            lines.extend(format_project_link_lines(settings, dashboard_url));
            lines.push("│".to_string());
            lines.push("├─ Running".to_string());
            lines.push("│".to_string());
            lines.push(running_table_header_row(running_event_width));
            lines.push(running_table_separator_row(running_event_width));

            if snapshot.running.is_empty() {
                lines.push("│  No active agents".to_string());
            } else {
                let mut running = snapshot.running.clone();
                running.sort_by(|left, right| left.identifier.cmp(&right.identifier));
                lines.extend(
                    running
                        .iter()
                        .map(|entry| format_running_summary(entry, running_event_width)),
                );
            }

            lines.push("│".to_string());
            lines.push("├─ Backoff queue".to_string());
            lines.push("│".to_string());

            if snapshot.retrying.is_empty() {
                lines.push("│  No queued retries".to_string());
            } else {
                let mut retrying = snapshot.retrying.clone();
                retrying.sort_by_key(|entry| entry.due_in_ms);
                lines.extend(retrying.iter().map(format_retry_summary));
            }
        }
        None => {
            lines.push("│ Orchestrator snapshot unavailable".to_string());
            lines.push(format_three_columns(
                "│ Status unknown",
                &format!("Throughput: {} tps", format_tps(tps)),
                &format_project_refresh(None),
                width,
            ));
            lines.push("│".to_string());
            lines.extend(format_project_link_lines(settings, dashboard_url));
        }
    }

    lines.push(closing_border(width));
    lines.join("\n")
}

pub fn render_snapshot_html_for_test(
    snapshot: Option<&Snapshot>,
    settings: &Settings,
    tps: f64,
    terminal_columns: Option<usize>,
) -> String {
    render_snapshot_html(
        snapshot,
        settings,
        dashboard_url(settings),
        tps,
        terminal_columns,
    )
}

pub fn render_snapshot_html(
    snapshot: Option<&Snapshot>,
    settings: &Settings,
    dashboard_url: Option<String>,
    tps: f64,
    terminal_columns: Option<usize>,
) -> String {
    let width = terminal_columns.unwrap_or(115).max(80);
    let running_event_width = running_event_width(Some(width));
    let mut lines = vec![term("term-strong", "╭─ SYMPHONY STATUS")];

    match snapshot {
        Some(snapshot) => {
            lines.push(term_line([
                term("term-strong", "│ Agents: "),
                term("term-green", &snapshot.running.len().to_string()),
                term("term-muted", "/"),
                term(
                    "term-muted",
                    &settings.agent.max_concurrent_agents.to_string(),
                ),
            ]));
            lines.push(term_line([
                term("term-strong", "│ Throughput: "),
                term("term-cyan", &format!("{} tps", format_tps(tps))),
            ]));
            lines.push(term_line([
                term("term-strong", "│ Runtime: "),
                term(
                    "term-magenta",
                    &format_runtime_seconds(snapshot.codex_totals.seconds_running),
                ),
            ]));
            lines.push(term_line([
                term("term-strong", "│ Tokens: "),
                term(
                    "term-yellow",
                    &format!("in {}", format_count(snapshot.codex_totals.input_tokens)),
                ),
                term("term-muted", " | "),
                term(
                    "term-yellow",
                    &format!("out {}", format_count(snapshot.codex_totals.output_tokens)),
                ),
                term("term-muted", " | "),
                term(
                    "term-yellow",
                    &format!("total {}", format_count(snapshot.codex_totals.total_tokens)),
                ),
            ]));
            lines.push(term_line([
                term("term-strong", "│ Rate Limits: "),
                format_rate_limits_html(snapshot.rate_limits.as_ref()),
            ]));
            lines.extend(format_project_link_html_lines(settings, dashboard_url));
            lines.push(format_project_refresh_html_line(Some(&snapshot.polling)));
            lines.push(term("term-strong", "├─ Running"));
            lines.push("│".to_string());
            lines.push(term(
                "term-muted",
                &running_table_header_row(running_event_width),
            ));
            lines.push(term(
                "term-muted",
                &running_table_separator_row(running_event_width),
            ));

            if snapshot.running.is_empty() {
                lines.push(term("term-muted", "│  No active agents"));
            } else {
                let mut running = snapshot.running.clone();
                running.sort_by(|left, right| left.identifier.cmp(&right.identifier));
                lines.extend(
                    running
                        .iter()
                        .map(|entry| render_running_summary_html(entry, running_event_width)),
                );
            }

            lines.push("│".to_string());
            lines.push(term("term-strong", "├─ Backoff queue"));
            lines.push("│".to_string());

            if snapshot.retrying.is_empty() {
                lines.push(term("term-muted", "│  No queued retries"));
            } else {
                let mut retrying = snapshot.retrying.clone();
                retrying.sort_by_key(|entry| entry.due_in_ms);
                lines.extend(retrying.iter().map(render_retry_summary_html));
            }
        }
        None => {
            lines.push(term("term-red", "│ Orchestrator snapshot unavailable"));
            lines.push(term_line([
                term("term-strong", "│ Throughput: "),
                term("term-cyan", &format!("{} tps", format_tps(tps))),
            ]));
            lines.extend(format_project_link_html_lines(settings, dashboard_url));
            lines.push(term_line([
                term("term-strong", "│ Next refresh: "),
                term("term-muted", "n/a"),
            ]));
        }
    }

    lines.push(term("term-strong", &closing_border(width)));
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

fn format_project_link_html_lines(
    settings: &Settings,
    dashboard_url: Option<String>,
) -> Vec<String> {
    let project_value = settings
        .tracker
        .project_slug
        .as_deref()
        .map(linear_project_url);
    let mut lines = vec![term_line([
        term("term-strong", "│ Project: "),
        project_value
            .map(|value| term("term-cyan", &value))
            .unwrap_or_else(|| term("term-muted", "n/a")),
    ])];
    if let Some(dashboard_url) = dashboard_url {
        lines.push(term_line([
            term("term-strong", "│ Dashboard: "),
            term("term-cyan", &dashboard_url),
        ]));
    }
    lines
}

fn format_project_refresh_html_line(
    polling: Option<&crate::orchestrator::PollingSnapshot>,
) -> String {
    match polling {
        Some(polling) if polling.checking => term_line([
            term("term-strong", "│ Next refresh: "),
            term("term-cyan", "checking now..."),
        ]),
        Some(polling) => match polling.next_poll_in_ms {
            Some(next_poll_in_ms) => {
                let seconds = next_poll_in_ms.saturating_add(999) / 1000;
                term_line([
                    term("term-strong", "│ Next refresh: "),
                    term("term-cyan", &format!("{seconds}s")),
                ])
            }
            None => term_line([
                term("term-strong", "│ Next refresh: "),
                term("term-muted", "n/a"),
            ]),
        },
        None => term_line([
            term("term-strong", "│ Next refresh: "),
            term("term-muted", "n/a"),
        ]),
    }
}

fn format_rate_limits_html(rate_limits: Option<&JsonValue>) -> String {
    let Some(rate_limits) = rate_limits else {
        return term("term-muted", "unavailable");
    };
    let limit_id = map_value(rate_limits, &["limit_id", "limit_name"]).unwrap_or("n/a");
    let primary = rate_limits
        .get("primary")
        .map(format_rate_limit_bucket)
        .unwrap_or_else(|| "n/a".to_string());
    let secondary = rate_limits
        .get("secondary")
        .map(format_rate_limit_bucket)
        .unwrap_or_else(|| "n/a".to_string());
    let credits = rate_limits
        .get("credits")
        .map(format_rate_limit_credits)
        .unwrap_or_else(|| "n/a".to_string());
    term_line([
        term("term-yellow", limit_id),
        term("term-muted", " | "),
        term("term-cyan", &format!("primary {primary}")),
        term("term-muted", " | "),
        term("term-cyan", &format!("secondary {secondary}")),
        term("term-muted", " | "),
        term("term-green", &format!("credits {credits}")),
    ])
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

fn format_three_columns(left: &str, middle: &str, right: &str, width: usize) -> String {
    let inner_width = width.saturating_sub(2).max(40);
    let left_width = (inner_width / 3).max(14);
    let middle_width = (inner_width / 3).max(14);
    let right_width = inner_width
        .saturating_sub(left_width)
        .saturating_sub(middle_width)
        .max(12);

    let left = truncate_plain(left, left_width);
    let middle = truncate_plain(middle, middle_width);
    let right = truncate_plain(right, right_width.max(12));

    format!(
        "│ {:<left_width$} {:<middle_width$} {:<right_width$}",
        left, middle, right
    )
}

fn format_project_refresh(polling: Option<&crate::orchestrator::PollingSnapshot>) -> String {
    match polling {
        Some(polling) if polling.checking => "Next refresh: checking now...".to_string(),
        Some(polling) => match polling.next_poll_in_ms {
            Some(next_poll_in_ms) => {
                let seconds = next_poll_in_ms.saturating_add(999) / 1000;
                format!("Next refresh: {seconds}s")
            }
            None => "Next refresh: n/a".to_string(),
        },
        None => "Next refresh: n/a".to_string(),
    }
}

fn dashboard_url(settings: &Settings) -> Option<String> {
    settings.server.port.map(|port| {
        format!(
            "http://{}:{port}/",
            dashboard_url_host(&settings.server.host)
        )
    })
}

fn linear_project_url(project_slug: &str) -> String {
    format!("https://linear.app/project/{project_slug}/issues")
}

fn dashboard_url_host(host: &str) -> String {
    let trimmed = host.trim();
    match trimmed {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1".to_string(),
        _ if trimmed.starts_with('[') && trimmed.ends_with(']') => trimmed.to_string(),
        _ if trimmed.contains(':') => format!("[{trimmed}]"),
        _ => trimmed.to_string(),
    }
}

fn format_rate_limits(rate_limits: Option<&JsonValue>) -> String {
    let Some(rate_limits) = rate_limits else {
        return "unavailable".to_string();
    };
    let limit_id = map_value(rate_limits, &["limit_id", "limit_name"]).unwrap_or("n/a");
    let primary = rate_limits
        .get("primary")
        .map(format_rate_limit_bucket)
        .unwrap_or_else(|| "n/a".to_string());
    let secondary = rate_limits
        .get("secondary")
        .map(format_rate_limit_bucket)
        .unwrap_or_else(|| "n/a".to_string());
    let credits = rate_limits
        .get("credits")
        .map(format_rate_limit_credits)
        .unwrap_or_else(|| "credits n/a".to_string());
    format!("{limit_id} | primary {primary} | secondary {secondary} | credits {credits}")
}

fn format_runtime_seconds(seconds: u64) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    format!("{minutes}m {seconds}s")
}

fn format_tps(value: f64) -> String {
    format_count(value.max(0.0).trunc() as u64)
}

fn format_count(value: u64) -> String {
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

fn format_rate_limit_bucket(bucket: &JsonValue) -> String {
    if bucket.is_null() {
        return "n/a".to_string();
    }
    if let Some(remaining) = bucket.get("remaining").and_then(as_u64_like)
        && let Some(limit) = bucket.get("limit").and_then(as_u64_like)
    {
        return format!("{remaining}/{limit}");
    }
    if let Some(has_credits) = bucket.get("has_credits").and_then(JsonValue::as_bool) {
        return if has_credits {
            "yes".to_string()
        } else {
            "no".to_string()
        };
    }
    sanitize_line(&bucket.to_string())
}

fn format_rate_limit_credits(credits: &JsonValue) -> String {
    if credits.is_null() {
        return "n/a".to_string();
    }
    if credits
        .get("unlimited")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return "unlimited".to_string();
    }
    if credits
        .get("has_credits")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return credits
            .get("balance")
            .and_then(as_u64_like)
            .map(format_count)
            .unwrap_or_else(|| "available".to_string());
    }
    if let Some(balance) = credits.get("balance").and_then(as_u64_like) {
        return format_count(balance);
    }
    "none".to_string()
}

fn as_u64_like(value: &JsonValue) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_i64()
                .filter(|value| *value >= 0)
                .map(|value| value as u64)
        })
        .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
}

fn running_table_header_row(running_event_width: usize) -> String {
    let header = [
        format_cell("ID", RUNNING_ID_WIDTH, Alignment::Left),
        format_cell("STAGE", RUNNING_STAGE_WIDTH, Alignment::Left),
        format_cell("PID", RUNNING_PID_WIDTH, Alignment::Left),
        format_cell("AGE / TURN", RUNNING_AGE_WIDTH, Alignment::Left),
        format_cell("TOKENS", RUNNING_TOKENS_WIDTH, Alignment::Left),
        format_cell("SESSION", RUNNING_SESSION_WIDTH, Alignment::Left),
        format_cell("EVENT", running_event_width, Alignment::Left),
    ]
    .join(" ");
    format!("│   {header}")
}

fn running_table_separator_row(running_event_width: usize) -> String {
    let separator_width = RUNNING_ID_WIDTH
        + RUNNING_STAGE_WIDTH
        + RUNNING_PID_WIDTH
        + RUNNING_AGE_WIDTH
        + RUNNING_TOKENS_WIDTH
        + RUNNING_SESSION_WIDTH
        + running_event_width
        + 6;
    format!("│   {}", "─".repeat(separator_width))
}

fn format_running_summary(
    entry: &crate::orchestrator::RunningSnapshot,
    running_event_width: usize,
) -> String {
    let issue = format_cell(&entry.identifier, RUNNING_ID_WIDTH, Alignment::Left);
    let state = format_cell(&entry.state, RUNNING_STAGE_WIDTH, Alignment::Left);
    let pid = format_cell(
        entry.codex_app_server_pid.as_deref().unwrap_or("n/a"),
        RUNNING_PID_WIDTH,
        Alignment::Left,
    );
    let age = format_cell(
        &format_runtime_and_turns(entry.runtime_seconds, entry.turn_count),
        RUNNING_AGE_WIDTH,
        Alignment::Left,
    );
    let tokens = format_cell(
        &format_count(entry.codex_total_tokens),
        RUNNING_TOKENS_WIDTH,
        Alignment::Right,
    );
    let session = format_cell(
        &compact_session_id(entry.session_id.as_deref()),
        RUNNING_SESSION_WIDTH,
        Alignment::Left,
    );
    let event = format_cell(
        &summarize_message(entry.last_codex_message.as_ref()),
        running_event_width,
        Alignment::Left,
    );
    format!("│ ● {issue} {state} {pid} {age} {tokens} {session} {event}")
}

fn render_running_summary_html(
    entry: &crate::orchestrator::RunningSnapshot,
    running_event_width: usize,
) -> String {
    let issue = format_cell(&entry.identifier, RUNNING_ID_WIDTH, Alignment::Left);
    let state = format_cell(&entry.state, RUNNING_STAGE_WIDTH, Alignment::Left);
    let pid = format_cell(
        entry.codex_app_server_pid.as_deref().unwrap_or("n/a"),
        RUNNING_PID_WIDTH,
        Alignment::Left,
    );
    let age = format_cell(
        &format_runtime_and_turns(entry.runtime_seconds, entry.turn_count),
        RUNNING_AGE_WIDTH,
        Alignment::Left,
    );
    let tokens = format_cell(
        &format_count(entry.codex_total_tokens),
        RUNNING_TOKENS_WIDTH,
        Alignment::Right,
    );
    let session = format_cell(
        &compact_session_id(entry.session_id.as_deref()),
        RUNNING_SESSION_WIDTH,
        Alignment::Left,
    );
    let event = format_cell(
        &summarize_message(entry.last_codex_message.as_ref()),
        running_event_width,
        Alignment::Left,
    );
    let status_class = running_status_class(entry.last_codex_event.as_deref());

    term_line([
        raw("│ "),
        term(status_class, "●"),
        raw(" "),
        term("term-cyan", &issue),
        raw(" "),
        term(status_class, &state),
        raw(" "),
        term("term-yellow", &pid),
        raw(" "),
        term("term-magenta", &age),
        raw(" "),
        term("term-yellow", &tokens),
        raw(" "),
        term("term-cyan", &session),
        raw(" "),
        term(status_class, &event),
    ])
}

fn format_retry_summary(entry: &crate::orchestrator::RetrySnapshot) -> String {
    let identifier = entry.identifier.as_deref().unwrap_or(&entry.issue_id);
    let mut row = format!(
        "│  ↻ {} attempt={} in {}",
        identifier,
        entry.attempt,
        next_in_words(entry.due_in_ms)
    );
    if let Some(error) = entry.error.as_deref().map(sanitize_line)
        && !error.is_empty()
    {
        row.push(' ');
        row.push_str("error=");
        row.push_str(&truncate(&error, 96));
    }
    row
}

fn render_retry_summary_html(entry: &crate::orchestrator::RetrySnapshot) -> String {
    let identifier = entry.identifier.as_deref().unwrap_or(&entry.issue_id);
    let mut parts = vec![
        raw("│  "),
        term("term-orange", "↻"),
        raw(" "),
        term("term-red", identifier),
        raw(" "),
        term("term-yellow", &format!("attempt={}", entry.attempt)),
        term("term-dim", " in "),
        term("term-cyan", &next_in_words(entry.due_in_ms)),
    ];
    if let Some(error) = entry.error.as_deref().map(sanitize_line)
        && !error.is_empty()
    {
        parts.push(raw(" "));
        parts.push(term("term-dim", &format!("error={}", truncate(&error, 96))));
    }
    term_line(parts)
}

fn next_in_words(due_in_ms: u64) -> String {
    let secs = due_in_ms / 1000;
    let millis = due_in_ms % 1000;
    format!("{secs}.{:03}s", millis)
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

fn summarize_message(message: Option<&JsonValue>) -> String {
    humanize_codex_message(message)
}

fn running_event_width(terminal_columns: Option<usize>) -> usize {
    let terminal_columns = terminal_columns.unwrap_or(115);
    let fixed_width = RUNNING_ID_WIDTH
        + RUNNING_STAGE_WIDTH
        + RUNNING_PID_WIDTH
        + RUNNING_AGE_WIDTH
        + RUNNING_TOKENS_WIDTH
        + RUNNING_SESSION_WIDTH;
    terminal_columns
        .saturating_sub(fixed_width + RUNNING_ROW_CHROME_WIDTH)
        .max(RUNNING_EVENT_MIN_WIDTH)
        .max(RUNNING_EVENT_DEFAULT_WIDTH.min(terminal_columns))
}

fn closing_border(width: usize) -> String {
    let dash_count = width.saturating_sub(1).max(79);
    format!("╰{}", "─".repeat(dash_count))
}

#[derive(Clone, Copy)]
enum Alignment {
    Left,
    Right,
}

fn format_cell(value: &str, width: usize, alignment: Alignment) -> String {
    let value = truncate_plain(&sanitize_line(value), width);
    match alignment {
        Alignment::Left => format!("{value:<width$}"),
        Alignment::Right => format!("{value:>width$}"),
    }
}

fn truncate_plain(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    if width <= 3 {
        return value.chars().take(width).collect();
    }
    let mut truncated = value.chars().take(width - 3).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn running_status_class(event: Option<&str>) -> &'static str {
    match event {
        None => "term-red",
        Some("codex/event/token_count") => "term-yellow",
        Some("codex/event/task_started") => "term-green",
        Some("turn_completed") => "term-magenta",
        _ => "term-blue",
    }
}

fn term(class: &str, value: &str) -> String {
    format!("<span class=\"{class}\">{}</span>", escape_html(value))
}

fn raw(value: &str) -> String {
    escape_html(value)
}

fn term_line<I>(parts: I) -> String
where
    I: IntoIterator<Item = String>,
{
    parts.into_iter().collect::<Vec<_>>().join("")
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
            format_snapshot_content_for_test(Some(&snapshot()), &settings(), 658_875.2, Some(115));
        assert!(rendered.contains("SYMPHONY STATUS"));
        assert!(rendered.contains("ID       STAGE"));
        assert!(rendered.contains("AGE / TURN"));
        assert!(rendered.contains("SESSION"));
        assert!(rendered.contains("MT-101"));
        assert!(rendered.contains("MT-202"));
        assert!(rendered.contains("approval"));
        assert!(rendered.contains("error=error with newline"));
        assert!(rendered.contains("Throughput: 658,875 tps"));
        assert!(rendered.contains("https://linear.app/project/demo/issues"));
        assert!(rendered.contains("http://127.0.0.1:4000/"));
        assert!(rendered.contains("1m 30s / 3"));
        assert!(rendered.contains("thread-1-turn-1") || rendered.contains("thre...turn-1"));
    }

    #[test]
    fn renders_terminal_html_with_semantic_color_classes() {
        let rendered =
            render_snapshot_html_for_test(Some(&snapshot()), &settings(), 658_875.2, Some(115));
        assert!(rendered.contains("term-strong"));
        assert!(rendered.contains("term-green"));
        assert!(rendered.contains("term-yellow"));
        assert!(rendered.contains("term-cyan"));
        assert!(rendered.contains("MT-101"));
        assert!(rendered.contains("Backoff queue"));
    }
}
