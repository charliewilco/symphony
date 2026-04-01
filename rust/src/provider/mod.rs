pub mod claude;
pub mod codex;
pub mod gemini;
pub mod ollama;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use crate::config::{ProviderKind, Settings};
use crate::tracker::Issue;

#[derive(Clone, Debug, Default, Serialize)]
pub struct AgentUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ProviderCapabilities {
    pub persistent_session: bool,
    pub rate_limits: bool,
    pub process_id: bool,
    pub streaming_events: bool,
}

#[derive(Clone, Debug)]
pub struct AgentUpdate {
    pub provider: ProviderKind,
    pub event: String,
    pub timestamp: DateTime<Utc>,
    pub payload: JsonValue,
    pub session_id: Option<String>,
    pub provider_pid: Option<String>,
    pub usage: Option<AgentUsage>,
    pub rate_limits: Option<JsonValue>,
}

#[derive(Clone, Debug, Default)]
pub struct TurnResult {
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
}

pub enum AgentSessionHandle {
    Codex(codex::AppServerSession),
    Claude(claude::ClaudeSidecarSession),
    Gemini(gemini::GeminiSession),
    Ollama(ollama::OllamaSession),
}

pub async fn start_session(
    workspace: &str,
    worker_host: Option<&str>,
    settings: &Settings,
) -> Result<AgentSessionHandle> {
    match settings.provider.kind {
        ProviderKind::Codex => Ok(AgentSessionHandle::Codex(
            codex::start_session(workspace, worker_host, settings).await?,
        )),
        ProviderKind::Claude => Ok(AgentSessionHandle::Claude(
            claude::start_session(workspace, worker_host, settings).await?,
        )),
        ProviderKind::Gemini => Ok(AgentSessionHandle::Gemini(
            gemini::start_session(workspace, worker_host, settings).await?,
        )),
        ProviderKind::Ollama => Ok(AgentSessionHandle::Ollama(
            ollama::start_session(workspace, worker_host, settings).await?,
        )),
    }
}

pub async fn run_turn(
    session: &mut AgentSessionHandle,
    prompt: &str,
    issue: &Issue,
    settings: &Settings,
    updates_tx: &mpsc::Sender<AgentUpdate>,
) -> Result<TurnResult> {
    match session {
        AgentSessionHandle::Codex(session) => {
            codex::run_turn(session, prompt, issue, settings, updates_tx).await
        }
        AgentSessionHandle::Claude(session) => {
            claude::run_turn(session, prompt, issue, settings, updates_tx).await
        }
        AgentSessionHandle::Gemini(session) => {
            gemini::run_turn(session, prompt, issue, settings, updates_tx).await
        }
        AgentSessionHandle::Ollama(session) => {
            ollama::run_turn(session, prompt, issue, settings, updates_tx).await
        }
    }
}

pub async fn stop_session(session: AgentSessionHandle) -> Result<()> {
    match session {
        AgentSessionHandle::Codex(session) => codex::stop_session(session).await,
        AgentSessionHandle::Claude(session) => claude::stop_session(session).await,
        AgentSessionHandle::Gemini(session) => gemini::stop_session(session).await,
        AgentSessionHandle::Ollama(session) => ollama::stop_session(session).await,
    }
}

pub fn capabilities(kind: ProviderKind) -> ProviderCapabilities {
    match kind {
        ProviderKind::Codex => ProviderCapabilities {
            persistent_session: true,
            rate_limits: true,
            process_id: true,
            streaming_events: true,
        },
        ProviderKind::Claude => ProviderCapabilities {
            persistent_session: true,
            rate_limits: false,
            process_id: true,
            streaming_events: true,
        },
        ProviderKind::Gemini => ProviderCapabilities {
            persistent_session: false,
            rate_limits: false,
            process_id: false,
            streaming_events: true,
        },
        ProviderKind::Ollama => ProviderCapabilities {
            persistent_session: true,
            rate_limits: false,
            process_id: false,
            streaming_events: true,
        },
    }
}

pub(crate) fn validate_workspace_cwd(
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
