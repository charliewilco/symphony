use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use tokio::sync::mpsc;

use super::{AgentUpdate, AgentUsage, TurnResult, validate_workspace_cwd};
use crate::config::{ProviderKind, Settings};
use crate::dynamic_tool;
use crate::tracker::Issue;

pub struct OllamaSession {
    _workspace: String,
    client: reqwest::Client,
    transcript: Vec<JsonValue>,
    turn_counter: u64,
}

pub async fn start_session(
    workspace: &str,
    worker_host: Option<&str>,
    settings: &Settings,
) -> Result<OllamaSession> {
    if worker_host.is_some() {
        bail!("ollama_remote_workers_not_supported");
    }
    Ok(OllamaSession {
        _workspace: validate_workspace_cwd(workspace, worker_host, settings)?,
        client: reqwest::Client::new(),
        transcript: Vec::new(),
        turn_counter: 0,
    })
}

pub async fn run_turn(
    session: &mut OllamaSession,
    prompt: &str,
    _issue: &Issue,
    settings: &Settings,
    updates_tx: &mpsc::Sender<AgentUpdate>,
) -> Result<TurnResult> {
    session.turn_counter += 1;
    session
        .transcript
        .push(json!({ "role": "user", "content": prompt }));

    for _ in 0..8 {
        let request = json!({
            "model": settings.provider.ollama.model,
            "stream": settings.provider.ollama.stream,
            "think": settings.provider.ollama.think,
            "messages": session.transcript,
            "tools": tool_specs_for_ollama()
        });
        let response = session
            .client
            .post(format!("{}/api/chat", settings.provider.ollama.base_url))
            .json(&request)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("ollama_chat_failed: {}", response.status());
        }
        let payload: JsonValue = response.json().await?;
        let usage = extract_usage(&payload);
        let _ = updates_tx
            .send(AgentUpdate {
                provider: ProviderKind::Ollama,
                event: "notification".to_string(),
                timestamp: Utc::now(),
                payload: payload.clone(),
                session_id: Some(format!("ollama-{}", session.turn_counter)),
                provider_pid: None,
                usage: usage.clone(),
                rate_limits: None,
            })
            .await;

        let message = payload
            .get("message")
            .cloned()
            .ok_or_else(|| anyhow!("ollama_missing_message"))?;
        if let Some(tool_calls) = message.get("tool_calls").and_then(JsonValue::as_array) {
            session
                .transcript
                .push(json!({ "role": "assistant", "tool_calls": tool_calls }));
            for tool_call in tool_calls {
                let function = tool_call
                    .get("function")
                    .cloned()
                    .unwrap_or(JsonValue::Null);
                let name = function.get("name").and_then(JsonValue::as_str);
                let arguments = function
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = dynamic_tool::execute(name, arguments, settings).await;
                let event = if result
                    .get("success")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(false)
                {
                    "tool_call_completed"
                } else {
                    "tool_call_failed"
                };
                let _ = updates_tx
                    .send(AgentUpdate {
                        provider: ProviderKind::Ollama,
                        event: event.to_string(),
                        timestamp: Utc::now(),
                        payload: result.clone(),
                        session_id: Some(format!("ollama-{}", session.turn_counter)),
                        provider_pid: None,
                        usage: None,
                        rate_limits: None,
                    })
                    .await;
                session.transcript.push(json!({
                    "role": "tool",
                    "name": name.unwrap_or("unknown"),
                    "content": result
                        .get("output")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                }));
            }
            continue;
        }

        session.transcript.push(json!({
            "role": "assistant",
            "content": message.get("content").cloned().unwrap_or(JsonValue::Null)
        }));
        return Ok(TurnResult {
            session_id: Some(format!("ollama-{}", session.turn_counter)),
            thread_id: None,
            turn_id: Some(format!("turn-{}", session.turn_counter)),
        });
    }

    bail!("ollama_tool_loop_exhausted")
}

pub async fn stop_session(_session: OllamaSession) -> Result<()> {
    Ok(())
}

fn extract_usage(payload: &JsonValue) -> Option<AgentUsage> {
    let input_tokens = payload
        .get("prompt_eval_count")
        .and_then(JsonValue::as_u64)?;
    let output_tokens = payload.get("eval_count").and_then(JsonValue::as_u64)?;
    Some(AgentUsage {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
    })
}

fn tool_specs_for_ollama() -> Vec<JsonValue> {
    dynamic_tool::tool_specs()
        .into_iter()
        .map(|spec| {
            let mut function = JsonMap::new();
            function.insert(
                "name".to_string(),
                spec.get("name").cloned().unwrap_or(JsonValue::Null),
            );
            function.insert(
                "description".to_string(),
                spec.get("description").cloned().unwrap_or(JsonValue::Null),
            );
            function.insert(
                "parameters".to_string(),
                spec.get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            );
            json!({
                "type": "function",
                "function": JsonValue::Object(function)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_tool_schema_matches_function_format() {
        let specs = tool_specs_for_ollama();
        assert_eq!(specs[0]["type"], "function");
    }
}
