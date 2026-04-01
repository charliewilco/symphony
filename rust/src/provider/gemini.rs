use anyhow::{Result, bail};
use chrono::Utc;
use serde_json::{Value as JsonValue, json};
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

use super::{AgentUpdate, AgentUsage, TurnResult, validate_workspace_cwd};
use crate::config::{GeminiOutputFormat, ProviderKind, Settings};
use crate::ssh;
use crate::tracker::Issue;

pub struct GeminiSession {
    workspace: String,
    worker_host: Option<String>,
    turn_counter: u64,
}

pub async fn start_session(
    workspace: &str,
    worker_host: Option<&str>,
    settings: &Settings,
) -> Result<GeminiSession> {
    Ok(GeminiSession {
        workspace: validate_workspace_cwd(workspace, worker_host, settings)?,
        worker_host: worker_host.map(ToString::to_string),
        turn_counter: 0,
    })
}

pub async fn run_turn(
    session: &mut GeminiSession,
    prompt: &str,
    _issue: &Issue,
    settings: &Settings,
    updates_tx: &mpsc::Sender<AgentUpdate>,
) -> Result<TurnResult> {
    session.turn_counter += 1;
    let requested = settings.provider.gemini.output_format;
    let first = run_gemini_once(session, prompt, settings, requested).await;
    let response = match first {
        Ok(output) => output,
        Err(error)
            if matches!(requested, GeminiOutputFormat::StreamJson)
                && error.to_string().contains("output-format") =>
        {
            run_gemini_once(session, prompt, settings, GeminiOutputFormat::Json).await?
        }
        Err(error) => return Err(error),
    };

    for event in response.events {
        let _ = updates_tx
            .send(AgentUpdate {
                provider: ProviderKind::Gemini,
                event: event
                    .get("event")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("notification")
                    .to_string(),
                timestamp: Utc::now(),
                payload: event,
                session_id: Some(response.session_id.clone()),
                provider_pid: None,
                usage: response.usage.clone(),
                rate_limits: None,
            })
            .await;
    }

    Ok(TurnResult {
        session_id: Some(response.session_id),
        thread_id: None,
        turn_id: Some(format!("turn-{}", session.turn_counter)),
    })
}

pub async fn stop_session(_session: GeminiSession) -> Result<()> {
    Ok(())
}

struct GeminiResponse {
    session_id: String,
    events: Vec<JsonValue>,
    usage: Option<AgentUsage>,
}

async fn run_gemini_once(
    session: &GeminiSession,
    prompt: &str,
    settings: &Settings,
    output_format: GeminiOutputFormat,
) -> Result<GeminiResponse> {
    let command = format!(
        "{} -p {} --output-format {} --yolo",
        settings.provider.gemini.command,
        ssh::shell_escape(prompt),
        output_format.as_str(),
    );
    let output = match session.worker_host.as_deref() {
        Some(host) => {
            let wrapped = format!(
                "cd {} && {}",
                ssh::shell_escape(&session.workspace),
                command
            );
            let (stdout, status) = ssh::run(host, &wrapped).await?;
            if status != 0 {
                bail!("gemini_command_failed: {stdout}");
            }
            stdout
        }
        None => {
            let mut child = Command::new("bash");
            child.kill_on_drop(true);
            child.arg("-lc").arg(&command);
            child.current_dir(&session.workspace);
            child.stdin(Stdio::null());
            child.stdout(Stdio::piped());
            child.stderr(Stdio::piped());
            let mut child = child.spawn()?;
            let mut stdout = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                pipe.read_to_string(&mut stdout).await?;
            }
            let output = child.wait_with_output().await?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                bail!("gemini_command_failed: {stderr}");
            }
            stdout
        }
    };

    let mut events = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        if let Ok(payload) = serde_json::from_str::<JsonValue>(line) {
            events.push(payload);
        } else {
            events.push(json!({
                "event": "output",
                "text": line
            }));
        }
    }
    if events.is_empty() {
        events.push(json!({
            "event": "output",
            "text": output
        }));
    }
    let usage = events.iter().find_map(extract_usage);
    Ok(GeminiResponse {
        session_id: format!("gemini-{}", Utc::now().timestamp_millis()),
        events,
        usage,
    })
}

fn extract_usage(payload: &JsonValue) -> Option<AgentUsage> {
    let usage = payload.get("usage")?;
    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(JsonValue::as_u64)?;
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(JsonValue::as_u64)?;
    Some(AgentUsage {
        input_tokens,
        output_tokens,
        total_tokens: usage
            .get("total_tokens")
            .and_then(JsonValue::as_u64)
            .unwrap_or(input_tokens + output_tokens),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_extracted_from_gemini_payloads() {
        let usage = extract_usage(&json!({
            "usage": {
                "input_tokens": 3,
                "output_tokens": 4,
                "total_tokens": 7
            }
        }))
        .unwrap();
        assert_eq!(usage.total_tokens, 7);
    }
}
