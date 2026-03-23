use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value as JsonValue, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::config::Settings;
use crate::dynamic_tool;
use crate::ssh;
use crate::tracker::Issue;

const NON_INTERACTIVE_TOOL_INPUT_ANSWER: &str =
    "This is a non-interactive session. Operator input is unavailable.";

#[derive(Clone, Debug)]
pub struct CodexUpdate {
    pub event: String,
    pub timestamp: DateTime<Utc>,
    pub payload: JsonValue,
    pub session_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
}

pub struct AppServerSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    thread_id: String,
    workspace: String,
    auto_approve_requests: bool,
    approval_policy: JsonValue,
    turn_sandbox_policy: JsonValue,
    codex_app_server_pid: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TurnResult {
    pub session_id: String,
    pub thread_id: String,
    pub turn_id: String,
}

pub async fn start_session(
    workspace: &str,
    worker_host: Option<&str>,
    settings: &Settings,
) -> Result<AppServerSession> {
    let workspace = validate_workspace_cwd(workspace, worker_host, settings)?;
    let mut child = match worker_host {
        Some(host) => ssh::start_ssh_child(
            host,
            &format!(
                "cd {} && exec {}",
                ssh::shell_escape(&workspace),
                settings.codex.command
            ),
        )?,
        None => {
            let mut command = Command::new("bash");
            command.kill_on_drop(true);
            command.arg("-lc").arg(&settings.codex.command);
            command.current_dir(&workspace);
            command.stdin(Stdio::piped());
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
            command.spawn()?
        }
    };

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("missing codex stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("missing codex stdout"))?;
    if let Some(stderr) = child.stderr.take() {
        spawn_log_stream(stderr, "turn stream");
    }
    let pid = child.id().map(|id| id.to_string());

    let mut session = AppServerSession {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        thread_id: String::new(),
        workspace: workspace.clone(),
        auto_approve_requests: settings.codex.approval_policy
            == JsonValue::String("never".to_string()),
        approval_policy: settings.codex.approval_policy.clone(),
        turn_sandbox_policy: settings
            .default_turn_sandbox_policy(Some(std::path::Path::new(&workspace))),
        codex_app_server_pid: pid,
    };

    send_message(
        &mut session.stdin,
        &json!({
            "method": "initialize",
            "id": 1,
            "params": {
                "capabilities": { "experimentalApi": true },
                "clientInfo": {
                    "name": "symphony-orchestrator",
                    "title": "Symphony Orchestrator",
                    "version": "0.1.0"
                }
            }
        }),
    )
    .await?;
    let _ = await_response(&mut session.stdout, 1, settings).await?;
    send_message(
        &mut session.stdin,
        &json!({ "method": "initialized", "params": {} }),
    )
    .await?;

    send_message(
        &mut session.stdin,
        &json!({
            "method": "thread/start",
            "id": 2,
            "params": {
                "approvalPolicy": session.approval_policy,
                "sandbox": settings.codex.thread_sandbox,
                "cwd": workspace,
                "dynamicTools": dynamic_tool::tool_specs()
            }
        }),
    )
    .await?;
    let response = await_response(&mut session.stdout, 2, settings).await?;
    session.thread_id = response
        .pointer("/thread/id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| anyhow!("invalid_thread_payload"))?
        .to_string();
    Ok(session)
}

pub async fn run_turn(
    session: &mut AppServerSession,
    prompt: &str,
    issue: &Issue,
    settings: &Settings,
    updates_tx: &mpsc::Sender<CodexUpdate>,
) -> Result<TurnResult> {
    send_message(
        &mut session.stdin,
        &json!({
            "method": "turn/start",
            "id": 3,
            "params": {
                "threadId": session.thread_id,
                "input": [{ "type": "text", "text": prompt }],
                "cwd": session.workspace,
                "title": format!("{}: {}", issue.identifier, issue.title),
                "approvalPolicy": session.approval_policy,
                "sandboxPolicy": session.turn_sandbox_policy
            }
        }),
    )
    .await?;

    let response = await_response(&mut session.stdout, 3, settings).await?;
    let turn_id = response
        .pointer("/turn/id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| anyhow!("invalid_turn_payload"))?
        .to_string();
    let session_id = format!("{}-{turn_id}", session.thread_id);
    let _ = updates_tx
        .send(CodexUpdate {
            event: "session_started".to_string(),
            timestamp: Utc::now(),
            payload: json!({
                "thread_id": session.thread_id,
                "turn_id": turn_id,
                "session_id": session_id
            }),
            session_id: Some(session_id.clone()),
            codex_app_server_pid: session.codex_app_server_pid.clone(),
        })
        .await;

    loop {
        let mut line = String::new();
        let read = timeout(
            std::time::Duration::from_millis(settings.codex.turn_timeout_ms),
            session.stdout.read_line(&mut line),
        )
        .await
        .map_err(|_| anyhow!("turn_timeout"))??;

        if read == 0 {
            bail!("port_exit");
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let payload = match serde_json::from_str::<JsonValue>(trimmed) {
            Ok(payload) => payload,
            Err(_) => {
                log_non_json_stream_line(trimmed, "turn stream");
                if protocol_message_candidate(trimmed) {
                    let _ = updates_tx
                        .send(CodexUpdate {
                            event: "malformed".to_string(),
                            timestamp: Utc::now(),
                            payload: JsonValue::String(trimmed.to_string()),
                            session_id: Some(session_id.clone()),
                            codex_app_server_pid: session.codex_app_server_pid.clone(),
                        })
                        .await;
                }
                continue;
            }
        };

        if let Some(method) = payload.get("method").and_then(JsonValue::as_str) {
            match method {
                "turn/completed" => {
                    let _ = updates_tx
                        .send(CodexUpdate {
                            event: "turn_completed".to_string(),
                            timestamp: Utc::now(),
                            payload: payload.clone(),
                            session_id: Some(session_id.clone()),
                            codex_app_server_pid: session.codex_app_server_pid.clone(),
                        })
                        .await;
                    return Ok(TurnResult {
                        session_id,
                        thread_id: session.thread_id.clone(),
                        turn_id,
                    });
                }
                method if needs_input(method, &payload) => {
                    bail!("turn_input_required: {payload}");
                }
                "turn/failed" => bail!(
                    "turn_failed: {}",
                    payload.get("params").cloned().unwrap_or(payload.clone())
                ),
                "turn/cancelled" => bail!(
                    "turn_cancelled: {}",
                    payload.get("params").cloned().unwrap_or(payload.clone())
                ),
                "item/tool/call" => {
                    if let Some(id) = payload.get("id").cloned() {
                        let params = payload.get("params").cloned().unwrap_or_else(|| json!({}));
                        let tool_name = tool_call_name(&params);
                        let arguments = tool_call_arguments(&params);
                        let result = normalize_dynamic_tool_result(
                            dynamic_tool::execute(tool_name.as_deref(), arguments, settings).await,
                        );
                        send_message(&mut session.stdin, &json!({ "id": id, "result": result }))
                            .await?;
                        let event = match (
                            tool_name.as_deref(),
                            result.get("success").and_then(JsonValue::as_bool),
                        ) {
                            (Some(dynamic_tool::LINEAR_GRAPHQL_TOOL), Some(true)) => {
                                "tool_call_completed"
                            }
                            (Some(dynamic_tool::LINEAR_GRAPHQL_TOOL), _) => "tool_call_failed",
                            _ => "unsupported_tool_call",
                        };
                        let _ = updates_tx
                            .send(CodexUpdate {
                                event: event.to_string(),
                                timestamp: Utc::now(),
                                payload: json!({
                                    "payload": payload,
                                    "raw": trimmed
                                }),
                                session_id: Some(session_id.clone()),
                                codex_app_server_pid: session.codex_app_server_pid.clone(),
                            })
                            .await;
                    }
                }
                "item/tool/requestUserInput" => {
                    let Some(id) = payload.get("id").cloned() else {
                        bail!("turn_input_required: {payload}");
                    };
                    let params = payload.get("params").cloned().unwrap_or_else(|| json!({}));
                    if session.auto_approve_requests
                        && let Some((answers, decision)) =
                            tool_request_user_input_approval_answers(&params)
                    {
                        send_message(
                            &mut session.stdin,
                            &json!({ "id": id, "result": { "answers": answers } }),
                        )
                        .await?;
                        let _ = updates_tx
                            .send(CodexUpdate {
                                event: "approval_auto_approved".to_string(),
                                timestamp: Utc::now(),
                                payload: json!({
                                    "payload": payload,
                                    "raw": trimmed,
                                    "decision": decision
                                }),
                                session_id: Some(session_id.clone()),
                                codex_app_server_pid: session.codex_app_server_pid.clone(),
                            })
                            .await;
                        continue;
                    }

                    let Some(answers) = tool_request_user_input_unavailable_answers(&params) else {
                        bail!("turn_input_required: {payload}");
                    };
                    send_message(
                        &mut session.stdin,
                        &json!({ "id": id, "result": { "answers": answers } }),
                    )
                    .await?;
                    let _ = updates_tx
                        .send(CodexUpdate {
                            event: "tool_input_auto_answered".to_string(),
                            timestamp: Utc::now(),
                            payload: json!({
                                "payload": payload,
                                "raw": trimmed,
                                "answer": NON_INTERACTIVE_TOOL_INPUT_ANSWER
                            }),
                            session_id: Some(session_id.clone()),
                            codex_app_server_pid: session.codex_app_server_pid.clone(),
                        })
                        .await;
                }
                "item/commandExecution/requestApproval"
                | "execCommandApproval"
                | "applyPatchApproval"
                | "item/fileChange/requestApproval" => {
                    if session.auto_approve_requests {
                        if let Some(id) = payload.get("id").cloned() {
                            let decision = approval_decision(method);
                            send_message(
                                &mut session.stdin,
                                &json!({ "id": id, "result": { "decision": decision } }),
                            )
                            .await?;
                            let _ = updates_tx
                                .send(CodexUpdate {
                                    event: "approval_auto_approved".to_string(),
                                    timestamp: Utc::now(),
                                    payload: json!({
                                        "payload": payload,
                                        "raw": trimmed,
                                        "decision": decision
                                    }),
                                    session_id: Some(session_id.clone()),
                                    codex_app_server_pid: session.codex_app_server_pid.clone(),
                                })
                                .await;
                        }
                    } else {
                        bail!("approval_required: {payload}");
                    }
                }
                _ => {
                    let _ = updates_tx
                        .send(CodexUpdate {
                            event: "notification".to_string(),
                            timestamp: Utc::now(),
                            payload: payload.clone(),
                            session_id: Some(session_id.clone()),
                            codex_app_server_pid: session.codex_app_server_pid.clone(),
                        })
                        .await;
                }
            }
        }
    }
}

fn validate_workspace_cwd(
    workspace: &str,
    worker_host: Option<&str>,
    settings: &Settings,
) -> Result<String> {
    match worker_host {
        Some(_) => {
            if workspace.trim().is_empty() {
                bail!("invalid_workspace_cwd: empty_remote_workspace");
            }
            if workspace.contains(['\n', '\r', '\0']) {
                bail!("invalid_workspace_cwd: invalid_remote_workspace");
            }
            Ok(workspace.to_string())
        }
        None => validate_local_workspace_cwd(workspace, settings),
    }
}

fn validate_local_workspace_cwd(workspace: &str, settings: &Settings) -> Result<String> {
    let expanded_workspace = PathBuf::from(workspace);
    let expanded_root = settings.workspace.root.clone();
    let canonical_workspace = expanded_workspace
        .canonicalize()
        .map_err(|error| anyhow!("invalid_workspace_cwd: path_unreadable: {error}"))?;
    let canonical_root = expanded_root
        .canonicalize()
        .map_err(|error| anyhow!("invalid_workspace_cwd: path_unreadable: {error}"))?;

    if canonical_workspace == canonical_root {
        bail!(
            "invalid_workspace_cwd: workspace_root: {}",
            canonical_workspace.display()
        );
    }

    if canonical_workspace.starts_with(&canonical_root) {
        return Ok(canonical_workspace.to_string_lossy().to_string());
    }

    if Path::new(workspace).starts_with(&expanded_root) {
        bail!(
            "invalid_workspace_cwd: symlink_escape: {} {}",
            workspace,
            canonical_root.display()
        );
    }

    bail!(
        "invalid_workspace_cwd: outside_workspace_root: {} {}",
        canonical_workspace.display(),
        canonical_root.display()
    );
}

pub async fn stop_session(mut session: AppServerSession) -> Result<()> {
    if let Err(error) = session.child.kill().await {
        tracing::debug!("Failed to kill codex child: {error}");
    }
    Ok(())
}

async fn send_message(stdin: &mut ChildStdin, payload: &JsonValue) -> Result<()> {
    stdin
        .write_all(serde_json::to_string(payload)?.as_bytes())
        .await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

async fn await_response(
    stdout: &mut BufReader<ChildStdout>,
    expected_id: u64,
    settings: &Settings,
) -> Result<JsonValue> {
    loop {
        let mut line = String::new();
        let read = timeout(
            std::time::Duration::from_millis(settings.codex.read_timeout_ms),
            stdout.read_line(&mut line),
        )
        .await
        .map_err(|_| anyhow!("codex_read_timeout"))??;
        if read == 0 {
            bail!("port_exit");
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let payload: JsonValue = match serde_json::from_str(trimmed) {
            Ok(payload) => payload,
            Err(_) => {
                log_non_json_stream_line(trimmed, "response stream");
                continue;
            }
        };
        if payload.get("id").and_then(JsonValue::as_u64) == Some(expected_id) {
            if let Some(error) = payload.get("error") {
                bail!("codex_response_error: {error}");
            }
            return Ok(payload.get("result").cloned().unwrap_or(JsonValue::Null));
        }
    }
}

fn spawn_log_stream(stderr: ChildStderr, stream_label: &'static str) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => log_non_json_stream_line(line.trim(), stream_label),
                Err(error) => {
                    tracing::debug!("Failed to read codex {stream_label}: {error}");
                    break;
                }
            }
        }
    });
}

fn approval_decision(method: &str) -> &'static str {
    match method {
        "execCommandApproval" | "applyPatchApproval" => "approved_for_session",
        _ => "acceptForSession",
    }
}

fn tool_call_name(params: &JsonValue) -> Option<String> {
    params
        .get("tool")
        .or_else(|| params.get("name"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
}

fn tool_call_arguments(params: &JsonValue) -> JsonValue {
    params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}))
}

fn normalize_dynamic_tool_result(result: JsonValue) -> JsonValue {
    let success = result
        .get("success")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let output = result
        .get("output")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| dynamic_tool_output(&result));
    let content_items = result
        .get("contentItems")
        .cloned()
        .filter(JsonValue::is_array)
        .unwrap_or_else(|| json!([{ "type": "inputText", "text": output }]));
    json!({
        "success": success,
        "output": output,
        "contentItems": content_items
    })
}

fn dynamic_tool_output(result: &JsonValue) -> String {
    result
        .get("contentItems")
        .and_then(JsonValue::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
        })
}

fn tool_request_user_input_approval_answers(
    params: &JsonValue,
) -> Option<(JsonValue, &'static str)> {
    let questions = params.get("questions")?.as_array()?;
    let mut answers = serde_json::Map::new();
    for question in questions {
        let question_id = question.get("id")?.as_str()?;
        let options = question.get("options")?.as_array()?;
        let answer = tool_request_user_input_approval_option_label(options)?;
        answers.insert(question_id.to_string(), json!({ "answers": [answer] }));
    }
    if answers.is_empty() {
        None
    } else {
        Some((JsonValue::Object(answers), "Approve this Session"))
    }
}

fn tool_request_user_input_approval_option_label(options: &[JsonValue]) -> Option<String> {
    let labels = options
        .iter()
        .filter_map(|option| option.get("label").and_then(JsonValue::as_str))
        .collect::<Vec<_>>();
    labels
        .iter()
        .find(|label| **label == "Approve this Session")
        .or_else(|| labels.iter().find(|label| **label == "Approve Once"))
        .or_else(|| {
            labels.iter().find(|label| {
                let normalized = label.trim().to_ascii_lowercase();
                normalized.starts_with("approve") || normalized.starts_with("allow")
            })
        })
        .map(|label| (*label).to_string())
}

fn tool_request_user_input_unavailable_answers(params: &JsonValue) -> Option<JsonValue> {
    let questions = params.get("questions")?.as_array()?;
    let mut answers = serde_json::Map::new();
    for question in questions {
        let question_id = question.get("id")?.as_str()?;
        answers.insert(
            question_id.to_string(),
            json!({ "answers": [NON_INTERACTIVE_TOOL_INPUT_ANSWER] }),
        );
    }
    if answers.is_empty() {
        None
    } else {
        Some(JsonValue::Object(answers))
    }
}

fn needs_input(method: &str, payload: &JsonValue) -> bool {
    method.starts_with("turn/")
        && (matches!(
            method,
            "turn/input_required"
                | "turn/needs_input"
                | "turn/need_input"
                | "turn/request_input"
                | "turn/request_response"
                | "turn/provide_input"
                | "turn/approval_required"
        ) || request_payload_requires_input(payload))
}

fn request_payload_requires_input(payload: &JsonValue) -> bool {
    needs_input_field(payload) || payload.get("params").is_some_and(needs_input_field)
}

fn needs_input_field(payload: &JsonValue) -> bool {
    payload.get("requiresInput").and_then(JsonValue::as_bool) == Some(true)
        || payload.get("needsInput").and_then(JsonValue::as_bool) == Some(true)
        || payload.get("input_required").and_then(JsonValue::as_bool) == Some(true)
        || payload.get("inputRequired").and_then(JsonValue::as_bool) == Some(true)
        || payload.get("type").and_then(JsonValue::as_str) == Some("input_required")
        || payload.get("type").and_then(JsonValue::as_str) == Some("needs_input")
}

fn protocol_message_candidate(data: &str) -> bool {
    data.trim_start().starts_with('{')
}

fn log_non_json_stream_line(data: &str, stream_label: &str) {
    let text = summarize_stream_line(data);
    if text.is_empty() {
        return;
    }
    let lower = text.to_ascii_lowercase();
    if [
        "error",
        "warn",
        "warning",
        "failed",
        "fatal",
        "panic",
        "exception",
    ]
    .iter()
    .any(|term| lower.contains(term))
    {
        tracing::warn!("Codex {stream_label} output: {text}");
    } else {
        tracing::debug!("Codex {stream_label} output: {text}");
    }
}

fn summarize_stream_line(data: &str) -> String {
    let text = strip_ansi_escape_sequences(data.trim());
    if text.is_empty() {
        return text;
    }

    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(summary) = summarize_tracing_style_line(&collapsed) {
        return summary;
    }

    truncate_text(&collapsed, 320)
}

fn summarize_tracing_style_line(line: &str) -> Option<String> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let level_index = tokens.iter().position(|token| {
        matches!(
            *token,
            "ERROR"
                | "WARN"
                | "INFO"
                | "DEBUG"
                | "TRACE"
                | "error"
                | "warn"
                | "info"
                | "debug"
                | "trace"
        )
    })?;
    let level = tokens.get(level_index)?;
    let target = tokens.get(level_index + 1)?.trim_end_matches(':');
    let message = tokens.get(level_index + 2..)?.join(" ").trim().to_string();
    if message.is_empty() {
        return Some(format!("{level} {target}"));
    }
    Some(format!(
        "{level} {target}: {}",
        truncate_text(&message, 240)
    ))
}

#[allow(clippy::while_let_on_iterator)]
fn strip_ansi_escape_sequences(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                let _ = chars.next();
                while let Some(next) = chars.next() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                let _ = chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{07}' {
                        break;
                    }
                    if next == '\u{1b}' && matches!(chars.peek().copied(), Some('\\')) {
                        let _ = chars.next();
                        break;
                    }
                }
            }
            Some(_) => {
                let _ = chars.next();
            }
            None => break,
        }
    }
    output
}

fn truncate_text(value: &str, width: usize) -> String {
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CliOverrides, Settings};
    use crate::tracker::Issue;
    use crate::workflow::parse;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    fn settings(workspace_root: &Path) -> Settings {
        settings_with_command(workspace_root, "sh fake", None)
    }

    fn settings_with_command(
        workspace_root: &Path,
        command: &str,
        approval_policy: Option<&str>,
    ) -> Settings {
        let mut workflow_source = format!(
            "---\ntracker:\n  kind: memory\nworkspace:\n  root: {}\ncodex:\n  command: '{}'\n",
            workspace_root.display(),
            command.replace('\'', "'\"'\"'")
        );
        if let Some(policy) = approval_policy {
            workflow_source.push_str(&format!("  approval_policy: {policy}\n"));
        }
        workflow_source.push_str("---\n");
        let workflow = parse(&workflow_source).unwrap();
        Settings::from_workflow(&workflow, &CliOverrides::default()).unwrap()
    }

    fn issue() -> Issue {
        Issue {
            id: "issue-1".to_string(),
            identifier: "MT-1".to_string(),
            title: "Test issue".to_string(),
            description: Some("Test".to_string()),
            priority: None,
            state: "In Progress".to_string(),
            branch_name: None,
            url: Some("https://example.com/issues/MT-1".to_string()),
            labels: vec![],
            blocked_by: vec![],
            assigned_to_worker: true,
            created_at: None,
            updated_at: None,
            assignee_id: None,
            assignee_email: None,
        }
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        path
    }

    #[test]
    fn rejects_workspace_root_and_outside_root() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("workspaces");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&workspace_root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let settings = settings(&workspace_root);
        assert!(validate_workspace_cwd(workspace_root.to_str().unwrap(), None, &settings).is_err());
        assert!(validate_workspace_cwd(outside.to_str().unwrap(), None, &settings).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_root = temp.path().join("workspaces");
        let outside = temp.path().join("outside");
        let symlink_workspace = workspace_root.join("MT-1000");
        std::fs::create_dir_all(&workspace_root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &symlink_workspace).unwrap();

        let settings = settings(&workspace_root);
        assert!(
            validate_workspace_cwd(symlink_workspace.to_str().unwrap(), None, &settings).is_err()
        );
    }

    #[test]
    fn approval_decisions_match_elixir() {
        assert_eq!(
            approval_decision("item/commandExecution/requestApproval"),
            "acceptForSession"
        );
        assert_eq!(
            approval_decision("execCommandApproval"),
            "approved_for_session"
        );
        assert_eq!(
            approval_decision("applyPatchApproval"),
            "approved_for_session"
        );
        assert_eq!(
            approval_decision("item/fileChange/requestApproval"),
            "acceptForSession"
        );
    }

    #[test]
    fn summarizes_stream_lines_without_ansi_noise() {
        let line = "\u{1b}[2m2026-03-23T07:48:39.727186Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mcodex_core::compact_remote\u{1b}[0m\u{1b}[2m:\u{1b}[0m remote compaction failed turn_id=019d19aa-d318-7330-90e4-61a80979f064";
        let summary = summarize_stream_line(line);
        assert!(!summary.contains("\u{1b}"));
        assert!(summary.starts_with("ERROR codex_core::compact_remote: remote compaction failed"));
        assert!(summary.contains("turn_id=019d19aa-d318-7330-90e4-61a80979f064"));
    }

    #[test]
    fn tool_request_user_input_helpers_match_expected_answers() {
        let params = json!({
            "questions": [{
                "id": "mcp_tool_call_approval_call-717",
                "options": [
                    { "label": "Approve Once" },
                    { "label": "Approve this Session" },
                    { "label": "Deny" }
                ]
            }]
        });
        let (answers, decision) = tool_request_user_input_approval_answers(&params).unwrap();
        assert_eq!(decision, "Approve this Session");
        assert_eq!(
            answers["mcp_tool_call_approval_call-717"]["answers"][0],
            "Approve this Session"
        );

        let fallback = tool_request_user_input_unavailable_answers(&json!({
            "questions": [{ "id": "freeform-718", "options": null }]
        }))
        .unwrap();
        assert_eq!(
            fallback["freeform-718"]["answers"][0],
            NON_INTERACTIVE_TOOL_INPUT_ANSWER
        );
    }

    #[test]
    fn detects_turn_input_required_and_protocol_candidates() {
        assert!(needs_input(
            "turn/input_required",
            &json!({ "params": { "requiresInput": true } })
        ));
        assert!(protocol_message_candidate("{\"method\":\"turn/completed\""));
        assert!(!protocol_message_candidate("warning: plain stderr line"));
    }

    #[tokio::test]
    async fn auto_approves_command_execution_requests_and_emits_event() {
        let temp = tempdir().unwrap();
        let workspace_root = temp.path().join("workspaces");
        let workspace = workspace_root.join("MT-1");
        std::fs::create_dir_all(&workspace).unwrap();
        let trace_file = temp.path().join("trace.log");
        let script = write_script(
            temp.path(),
            "fake-codex.sh",
            &format!(
                "#!/bin/sh\ncount=0\ntrace='{}'\nwhile IFS= read -r line; do\n  count=$((count + 1))\n  printf '%s\\n' \"$line\" >> \"$trace\"\n  case \"$count\" in\n    1) printf '%s\\n' '{{\"id\":1,\"result\":{{}}}}' ;;\n    2) : ;;\n    3) printf '%s\\n' '{{\"id\":2,\"result\":{{\"thread\":{{\"id\":\"thread-89\"}}}}}}' ;;\n    4) printf '%s\\n' '{{\"id\":3,\"result\":{{\"turn\":{{\"id\":\"turn-89\"}}}}}}'\n       printf '%s\\n' '{{\"id\":99,\"method\":\"item/commandExecution/requestApproval\",\"params\":{{\"command\":\"gh pr view\"}}}}' ;;\n    5) printf '%s\\n' '{{\"method\":\"turn/completed\"}}'\n       exit 0 ;;\n  esac\ndone\n",
                trace_file.display()
            ),
        );
        let settings =
            settings_with_command(&workspace_root, script.to_str().unwrap(), Some("never"));
        let mut session = start_session(workspace.to_str().unwrap(), None, &settings)
            .await
            .unwrap();
        let (updates_tx, mut updates_rx) = mpsc::channel(16);

        let result = run_turn(
            &mut session,
            "Handle approval request",
            &issue(),
            &settings,
            &updates_tx,
        )
        .await
        .unwrap();
        assert_eq!(result.turn_id, "turn-89");

        let trace = std::fs::read_to_string(trace_file).unwrap();
        assert!(trace.contains("\"id\":99"));
        assert!(trace.contains("\"decision\":\"acceptForSession\""));

        let mut saw_auto_approval = false;
        while let Ok(update) = updates_rx.try_recv() {
            if update.event == "approval_auto_approved" {
                saw_auto_approval = true;
                assert_eq!(update.payload["decision"], "acceptForSession");
            }
        }
        assert!(saw_auto_approval);
        stop_session(session).await.unwrap();
    }

    #[tokio::test]
    async fn command_execution_approval_requires_manual_confirmation_by_default() {
        let temp = tempdir().unwrap();
        let workspace_root = temp.path().join("workspaces");
        let workspace = workspace_root.join("MT-1");
        std::fs::create_dir_all(&workspace).unwrap();
        let script = write_script(
            temp.path(),
            "fake-codex.sh",
            "#!/bin/sh\ncount=0\nwhile IFS= read -r _line; do\n  count=$((count + 1))\n  case \"$count\" in\n    1) printf '%s\\n' '{\"id\":1,\"result\":{}}' ;;\n    2) printf '%s\\n' '{\"id\":2,\"result\":{\"thread\":{\"id\":\"thread-89\"}}}' ;;\n    3) printf '%s\\n' '{\"id\":3,\"result\":{\"turn\":{\"id\":\"turn-89\"}}}'\n       printf '%s\\n' '{\"id\":99,\"method\":\"item/commandExecution/requestApproval\",\"params\":{\"command\":\"gh pr view\"}}' ;;\n    *) sleep 1 ;;\n  esac\ndone\n",
        );
        let settings = settings_with_command(&workspace_root, script.to_str().unwrap(), None);
        let mut session = start_session(workspace.to_str().unwrap(), None, &settings)
            .await
            .unwrap();
        let (updates_tx, _updates_rx) = mpsc::channel(16);

        let error = run_turn(
            &mut session,
            "Handle approval request",
            &issue(),
            &settings,
            &updates_tx,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("approval_required"));
        stop_session(session).await.unwrap();
    }

    #[tokio::test]
    async fn auto_answers_tool_input_and_emits_malformed_events() {
        let temp = tempdir().unwrap();
        let workspace_root = temp.path().join("workspaces");
        let workspace = workspace_root.join("MT-1");
        std::fs::create_dir_all(&workspace).unwrap();
        let trace_file = temp.path().join("trace.log");
        let script = write_script(
            temp.path(),
            "fake-codex.sh",
            &format!(
                "#!/bin/sh\ncount=0\ntrace='{}'\nwhile IFS= read -r line; do\n  count=$((count + 1))\n  printf '%s\\n' \"$line\" >> \"$trace\"\n  case \"$count\" in\n    1) printf '%s\\n' '{{\"id\":1,\"result\":{{}}}}' ;;\n    2) : ;;\n    3) printf '%s\\n' '{{\"id\":2,\"result\":{{\"thread\":{{\"id\":\"thread-718\"}}}}}}' ;;\n    4) printf '%s\\n' '{{\"id\":3,\"result\":{{\"turn\":{{\"id\":\"turn-718\"}}}}}}'\n       printf '%s\\n' '{{\"method\":\"turn/completed\"'\n       printf '%s\\n' '{{\"id\":111,\"method\":\"item/tool/requestUserInput\",\"params\":{{\"questions\":[{{\"id\":\"freeform-718\",\"options\":null}}]}}}}' ;;\n    5) printf '%s\\n' '{{\"method\":\"turn/completed\"}}'\n       exit 0 ;;\n  esac\ndone\n",
                trace_file.display()
            ),
        );
        let settings = settings_with_command(&workspace_root, script.to_str().unwrap(), None);
        let mut session = start_session(workspace.to_str().unwrap(), None, &settings)
            .await
            .unwrap();
        let (updates_tx, mut updates_rx) = mpsc::channel(16);

        run_turn(
            &mut session,
            "Handle generic tool input",
            &issue(),
            &settings,
            &updates_tx,
        )
        .await
        .unwrap();

        let trace = std::fs::read_to_string(trace_file).unwrap();
        assert!(trace.contains("\"id\":111"));
        assert!(trace.contains(NON_INTERACTIVE_TOOL_INPUT_ANSWER));

        let mut saw_malformed = false;
        let mut saw_auto_answer = false;
        while let Ok(update) = updates_rx.try_recv() {
            match update.event.as_str() {
                "malformed" => {
                    saw_malformed = true;
                    assert_eq!(
                        update.payload,
                        JsonValue::String("{\"method\":\"turn/completed\"".to_string())
                    );
                }
                "tool_input_auto_answered" => {
                    saw_auto_answer = true;
                    assert_eq!(update.payload["answer"], NON_INTERACTIVE_TOOL_INPUT_ANSWER);
                }
                _ => {}
            }
        }
        assert!(saw_malformed);
        assert!(saw_auto_answer);
        stop_session(session).await.unwrap();
    }

    #[tokio::test]
    async fn auto_approves_mcp_tool_prompts_and_distinguishes_tool_failures() {
        let temp = tempdir().unwrap();
        let workspace_root = temp.path().join("workspaces");
        let workspace = workspace_root.join("MT-1");
        std::fs::create_dir_all(&workspace).unwrap();
        let trace_file = temp.path().join("trace.log");
        let script = write_script(
            temp.path(),
            "fake-codex.sh",
            &format!(
                "#!/bin/sh\ncount=0\ntrace='{}'\nwhile IFS= read -r line; do\n  count=$((count + 1))\n  printf '%s\\n' \"$line\" >> \"$trace\"\n  case \"$count\" in\n    1) printf '%s\\n' '{{\"id\":1,\"result\":{{}}}}' ;;\n    2) : ;;\n    3) printf '%s\\n' '{{\"id\":2,\"result\":{{\"thread\":{{\"id\":\"thread-717\"}}}}}}' ;;\n    4) printf '%s\\n' '{{\"id\":3,\"result\":{{\"turn\":{{\"id\":\"turn-717\"}}}}}}'\n       printf '%s\\n' '{{\"id\":110,\"method\":\"item/tool/requestUserInput\",\"params\":{{\"questions\":[{{\"id\":\"mcp_tool_call_approval_call-717\",\"options\":[{{\"label\":\"Approve Once\"}},{{\"label\":\"Approve this Session\"}},{{\"label\":\"Deny\"}}]}}]}}}}'\n       printf '%s\\n' '{{\"id\":101,\"method\":\"item/tool/call\",\"params\":{{\"tool\":\"unsupported_tool\",\"arguments\":{{}}}}}}'\n       printf '%s\\n' '{{\"id\":102,\"method\":\"item/tool/call\",\"params\":{{\"tool\":\"linear_graphql\",\"arguments\":{{}}}}}}' ;;\n    5) : ;;\n    6) : ;;\n    7) printf '%s\\n' '{{\"method\":\"turn/completed\"}}'\n       exit 0 ;;\n  esac\ndone\n",
                trace_file.display()
            ),
        );
        let settings =
            settings_with_command(&workspace_root, script.to_str().unwrap(), Some("never"));
        let mut session = start_session(workspace.to_str().unwrap(), None, &settings)
            .await
            .unwrap();
        let (updates_tx, mut updates_rx) = mpsc::channel(16);

        run_turn(
            &mut session,
            "Handle approval prompt",
            &issue(),
            &settings,
            &updates_tx,
        )
        .await
        .unwrap();

        let trace = std::fs::read_to_string(trace_file).unwrap();
        assert!(trace.contains("\"Approve this Session\""));

        let mut saw_auto_approval = false;
        let mut saw_unsupported_tool = false;
        let mut saw_tool_failure = false;
        while let Ok(update) = updates_rx.try_recv() {
            match update.event.as_str() {
                "approval_auto_approved" => saw_auto_approval = true,
                "unsupported_tool_call" => saw_unsupported_tool = true,
                "tool_call_failed" => saw_tool_failure = true,
                _ => {}
            }
        }
        assert!(saw_auto_approval);
        assert!(saw_unsupported_tool);
        assert!(saw_tool_failure);
        stop_session(session).await.unwrap();
    }

    #[tokio::test]
    async fn turn_input_required_fails_the_turn() {
        let temp = tempdir().unwrap();
        let workspace_root = temp.path().join("workspaces");
        let workspace = workspace_root.join("MT-1");
        std::fs::create_dir_all(&workspace).unwrap();
        let script = write_script(
            temp.path(),
            "fake-codex.sh",
            "#!/bin/sh\ncount=0\nwhile IFS= read -r _line; do\n  count=$((count + 1))\n  case \"$count\" in\n    1) printf '%s\\n' '{\"id\":1,\"result\":{}}' ;;\n    2) printf '%s\\n' '{\"id\":2,\"result\":{\"thread\":{\"id\":\"thread-88\"}}}' ;;\n    3) printf '%s\\n' '{\"id\":3,\"result\":{\"turn\":{\"id\":\"turn-88\"}}}'\n       printf '%s\\n' '{\"method\":\"turn/input_required\",\"id\":\"resp-1\",\"params\":{\"requiresInput\":true,\"reason\":\"blocked\"}}' ;;\n    *) exit 0 ;;\n  esac\ndone\n",
        );
        let settings = settings_with_command(&workspace_root, script.to_str().unwrap(), None);
        let mut session = start_session(workspace.to_str().unwrap(), None, &settings)
            .await
            .unwrap();
        let (updates_tx, _updates_rx) = mpsc::channel(16);

        let error = run_turn(
            &mut session,
            "Needs input",
            &issue(),
            &settings,
            &updates_tx,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("turn_input_required"));
        stop_session(session).await.unwrap();
    }
}
