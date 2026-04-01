use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use serde_json::{Value as JsonValue, json};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::{AgentUpdate, TurnResult, validate_workspace_cwd};
use crate::config::{ProviderKind, Settings};
use crate::ssh;
use crate::tracker::Issue;

const BUNDLED_SIDECAR_MJS: &str = include_str!("../../provider/claude_sidecar.mjs");

pub struct ClaudeSidecarSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    workspace: String,
    provider_pid: Option<String>,
    session_id: Option<String>,
}

pub async fn start_session(
    workspace: &str,
    worker_host: Option<&str>,
    settings: &Settings,
) -> Result<ClaudeSidecarSession> {
    let workspace = validate_workspace_cwd(workspace, worker_host, settings)?;
    let entrypoint = resolve_sidecar_entrypoint(settings)?;
    let launch = format!(
        "{} {}",
        settings.provider.claude.node_command,
        ssh::shell_escape(&entrypoint.to_string_lossy())
    );
    let mut child = match worker_host {
        Some(host) => ssh::start_ssh_child(
            host,
            &format!("cd {} && exec {}", ssh::shell_escape(&workspace), launch),
        )?,
        None => {
            let mut command = Command::new("bash");
            command.kill_on_drop(true);
            command.arg("-lc").arg(&launch);
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
        .ok_or_else(|| anyhow!("missing claude sidecar stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("missing claude sidecar stdout"))?;
    if let Some(stderr) = child.stderr.take() {
        spawn_log_stream(stderr, "claude sidecar");
    }
    Ok(ClaudeSidecarSession {
        provider_pid: child.id().map(|id| id.to_string()),
        child,
        stdin,
        stdout: BufReader::new(stdout),
        workspace,
        session_id: None,
    })
}

pub async fn run_turn(
    session: &mut ClaudeSidecarSession,
    prompt: &str,
    issue: &Issue,
    settings: &Settings,
    updates_tx: &mpsc::Sender<AgentUpdate>,
) -> Result<TurnResult> {
    send_message(
        &mut session.stdin,
        &json!({
            "method": "turn/start",
            "id": 1,
            "params": {
                "prompt": prompt,
                "cwd": session.workspace,
                "issue": {
                    "identifier": issue.identifier,
                    "title": issue.title
                },
                "session_id": session.session_id,
                "allowed_tools": settings.provider.claude.allowed_tools,
                "permission_mode": settings.provider.claude.permission_mode,
                "setting_sources": settings.provider.claude.setting_sources
            }
        }),
    )
    .await?;

    loop {
        let mut line = String::new();
        let read = timeout(
            std::time::Duration::from_millis(settings.provider.read_timeout_ms),
            session.stdout.read_line(&mut line),
        )
        .await
        .map_err(|_| anyhow!("provider_read_timeout"))??;
        if read == 0 {
            bail!("provider_exit");
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let payload: JsonValue = serde_json::from_str(trimmed)
            .map_err(|_| anyhow!("provider_invalid_json: {trimmed}"))?;
        if payload.get("id").and_then(JsonValue::as_u64) == Some(1) {
            let result = payload.get("result").cloned().unwrap_or(JsonValue::Null);
            let session_id = result
                .pointer("/session/id")
                .or_else(|| result.pointer("/session_id"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string);
            session.session_id = session_id.clone();
            return Ok(TurnResult {
                session_id,
                thread_id: None,
                turn_id: result
                    .pointer("/turn/id")
                    .or_else(|| result.pointer("/turn_id"))
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string),
            });
        }

        let event = payload
            .pointer("/params/event")
            .or_else(|| payload.get("event"))
            .and_then(JsonValue::as_str)
            .unwrap_or("notification")
            .to_string();
        let update_payload = payload
            .pointer("/params/payload")
            .cloned()
            .or_else(|| payload.get("payload").cloned())
            .unwrap_or_else(|| payload.clone());
        let usage = payload
            .pointer("/params/usage")
            .or_else(|| payload.get("usage"))
            .and_then(parse_usage);
        let _ = updates_tx
            .send(AgentUpdate {
                provider: ProviderKind::Claude,
                event,
                timestamp: Utc::now(),
                payload: update_payload,
                session_id: session.session_id.clone(),
                provider_pid: session.provider_pid.clone(),
                usage,
                rate_limits: None,
            })
            .await;
    }
}

pub async fn stop_session(mut session: ClaudeSidecarSession) -> Result<()> {
    if let Err(error) = session.child.kill().await {
        tracing::debug!("Failed to kill claude sidecar: {error}");
    }
    Ok(())
}

fn resolve_sidecar_entrypoint(settings: &Settings) -> Result<PathBuf> {
    if let Some(entrypoint) = settings.provider.claude.entrypoint.as_ref() {
        return Ok(entrypoint.clone());
    }
    let root = std::env::temp_dir().join("symphony-provider-sidecars");
    std::fs::create_dir_all(&root)?;
    let path = root.join("claude_sidecar.mjs");
    if !path.exists() {
        std::fs::write(&path, BUNDLED_SIDECAR_MJS)?;
    }
    Ok(path)
}

async fn send_message(stdin: &mut ChildStdin, payload: &JsonValue) -> Result<()> {
    stdin
        .write_all(serde_json::to_string(payload)?.as_bytes())
        .await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

fn parse_usage(value: &JsonValue) -> Option<super::AgentUsage> {
    let input_tokens = value.get("input_tokens").and_then(JsonValue::as_u64)?;
    let output_tokens = value.get("output_tokens").and_then(JsonValue::as_u64)?;
    let total_tokens = value
        .get("total_tokens")
        .and_then(JsonValue::as_u64)
        .unwrap_or(input_tokens + output_tokens);
    Some(super::AgentUsage {
        input_tokens,
        output_tokens,
        total_tokens,
    })
}

fn spawn_log_stream(stderr: ChildStderr, stream_label: &'static str) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => tracing::debug!("{}: {}", stream_label, line.trim()),
                Err(error) => {
                    tracing::debug!("Failed to read {stream_label}: {error}");
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings_from_toml_str;
    use tempfile::tempdir;

    #[test]
    fn bundled_entrypoint_can_be_materialized() {
        let temp = tempdir().unwrap();
        let settings = settings_from_toml_str(&format!(
            "[tracker]\nkind = \"memory\"\n[workspace]\nroot = \"{}\"\n[provider]\nkind = \"claude\"\n",
            temp.path().display()
        ));
        let path = resolve_sidecar_entrypoint(&settings).unwrap();
        assert!(path.exists());
    }
}
