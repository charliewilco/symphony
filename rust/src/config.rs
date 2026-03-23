use std::collections::{BTreeMap, HashMap};
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::workflow::LoadedWorkflow;

#[derive(Clone, Debug, Default)]
pub struct CliOverrides {
    pub logs_root: Option<PathBuf>,
    pub server_port_override: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub tracker: TrackerSettings,
    pub polling: PollingSettings,
    pub workspace: WorkspaceSettings,
    pub worker: WorkerSettings,
    pub agent: AgentSettings,
    pub codex: CodexSettings,
    pub hooks: HookSettings,
    pub observability: ObservabilitySettings,
    pub server: ServerSettings,
}

#[derive(Clone, Debug)]
pub struct TrackerSettings {
    pub kind: Option<String>,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub project_slug: Option<String>,
    pub assignee: Option<String>,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PollingSettings {
    pub interval_ms: u64,
}

#[derive(Clone, Debug)]
pub struct WorkspaceSettings {
    pub root: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct WorkerSettings {
    pub ssh_hosts: Vec<String>,
    pub max_concurrent_agents_per_host: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct AgentSettings {
    pub max_concurrent_agents: usize,
    pub max_turns: usize,
    pub max_retry_backoff_ms: u64,
    pub max_concurrent_agents_by_state: HashMap<String, usize>,
}

#[derive(Clone, Debug)]
pub struct CodexSettings {
    pub command: String,
    pub approval_policy: JsonValue,
    pub thread_sandbox: String,
    pub turn_sandbox_policy: Option<JsonValue>,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub struct HookSettings {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub struct ObservabilitySettings {
    pub dashboard_enabled: bool,
    pub refresh_ms: u64,
    pub render_interval_ms: u64,
}

#[derive(Clone, Debug)]
pub struct ServerSettings {
    pub port: Option<u16>,
    pub host: String,
}

#[derive(Debug, Deserialize, Default)]
struct RawRoot {
    #[serde(default)]
    tracker: RawTracker,
    #[serde(default)]
    polling: RawPolling,
    #[serde(default)]
    workspace: RawWorkspace,
    #[serde(default)]
    worker: RawWorker,
    #[serde(default)]
    agent: RawAgent,
    #[serde(default)]
    codex: RawCodex,
    #[serde(default)]
    hooks: RawHooks,
    #[serde(default)]
    observability: RawObservability,
    #[serde(default)]
    server: RawServer,
}

#[derive(Debug, Deserialize, Default)]
struct RawTracker {
    kind: Option<String>,
    endpoint: Option<String>,
    api_key: Option<String>,
    project_slug: Option<String>,
    assignee: Option<String>,
    active_states: Option<Vec<String>>,
    terminal_states: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct RawPolling {
    interval_ms: Option<serde_yaml::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct RawWorkspace {
    root: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawWorker {
    ssh_hosts: Option<Vec<String>>,
    max_concurrent_agents_per_host: Option<serde_yaml::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct RawAgent {
    max_concurrent_agents: Option<serde_yaml::Value>,
    max_turns: Option<serde_yaml::Value>,
    max_retry_backoff_ms: Option<serde_yaml::Value>,
    max_concurrent_agents_by_state: Option<BTreeMap<String, serde_yaml::Value>>,
}

#[derive(Debug, Deserialize, Default)]
struct RawCodex {
    command: Option<String>,
    approval_policy: Option<serde_yaml::Value>,
    thread_sandbox: Option<String>,
    turn_sandbox_policy: Option<serde_yaml::Value>,
    turn_timeout_ms: Option<serde_yaml::Value>,
    read_timeout_ms: Option<serde_yaml::Value>,
    stall_timeout_ms: Option<serde_yaml::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct RawHooks {
    after_create: Option<String>,
    before_run: Option<String>,
    after_run: Option<String>,
    before_remove: Option<String>,
    timeout_ms: Option<serde_yaml::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct RawObservability {
    dashboard_enabled: Option<bool>,
    refresh_ms: Option<serde_yaml::Value>,
    render_interval_ms: Option<serde_yaml::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct RawServer {
    port: Option<serde_yaml::Value>,
    host: Option<String>,
}

impl Settings {
    pub fn from_workflow(workflow: &LoadedWorkflow, overrides: &CliOverrides) -> Result<Self> {
        let raw: RawRoot = serde_yaml::from_value(workflow.config.clone())?;
        let tracker_kind = raw.tracker.kind.clone();

        let workspace_root = expand_path_like(
            raw.workspace
                .root
                .as_deref()
                .unwrap_or(&default_workspace_root_string()),
        );
        let workspace_root = workspace_root?;

        let tracker_api_key = resolve_env_string(raw.tracker.api_key, Some("LINEAR_API_KEY"));
        let tracker_assignee = resolve_env_string(raw.tracker.assignee, Some("LINEAR_ASSIGNEE"));

        let agent_state_limits = raw
            .agent
            .max_concurrent_agents_by_state
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(state, value)| {
                parse_u64(&value)
                    .ok()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .map(|n| (normalize_issue_state(&state), n))
            })
            .collect::<HashMap<_, _>>();

        let turn_sandbox_policy = match raw.codex.turn_sandbox_policy {
            Some(value) => Some(serde_json::to_value(value)?),
            None => None,
        };

        let settings = Settings {
            tracker: TrackerSettings {
                kind: tracker_kind.clone(),
                endpoint: raw
                    .tracker
                    .endpoint
                    .unwrap_or_else(|| "https://api.linear.app/graphql".to_string()),
                api_key: tracker_api_key,
                project_slug: raw.tracker.project_slug,
                assignee: tracker_assignee,
                active_states: raw
                    .tracker
                    .active_states
                    .unwrap_or_else(|| vec!["Todo".to_string(), "In Progress".to_string()]),
                terminal_states: raw.tracker.terminal_states.unwrap_or_else(|| {
                    vec![
                        "Closed".to_string(),
                        "Cancelled".to_string(),
                        "Canceled".to_string(),
                        "Duplicate".to_string(),
                        "Done".to_string(),
                    ]
                }),
            },
            polling: PollingSettings {
                interval_ms: parse_u64_opt(raw.polling.interval_ms.as_ref())?.unwrap_or(30_000),
            },
            workspace: WorkspaceSettings {
                root: workspace_root,
            },
            worker: WorkerSettings {
                ssh_hosts: raw
                    .worker
                    .ssh_hosts
                    .unwrap_or_default()
                    .into_iter()
                    .map(|host| host.trim().to_string())
                    .filter(|host| !host.is_empty())
                    .collect(),
                max_concurrent_agents_per_host: parse_u64_opt(
                    raw.worker.max_concurrent_agents_per_host.as_ref(),
                )?
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0),
            },
            agent: AgentSettings {
                max_concurrent_agents: parse_u64_opt(raw.agent.max_concurrent_agents.as_ref())?
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(10),
                max_turns: parse_u64_opt(raw.agent.max_turns.as_ref())?
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(20),
                max_retry_backoff_ms: parse_u64_opt(raw.agent.max_retry_backoff_ms.as_ref())?
                    .unwrap_or(300_000),
                max_concurrent_agents_by_state: agent_state_limits,
            },
            codex: CodexSettings {
                command: raw
                    .codex
                    .command
                    .unwrap_or_else(|| "codex app-server".to_string()),
                approval_policy: raw
                    .codex
                    .approval_policy
                    .map(serde_json::to_value)
                    .transpose()?
                    .unwrap_or_else(default_approval_policy),
                thread_sandbox: raw
                    .codex
                    .thread_sandbox
                    .unwrap_or_else(|| "workspace-write".to_string()),
                turn_sandbox_policy,
                turn_timeout_ms: parse_u64_opt(raw.codex.turn_timeout_ms.as_ref())?
                    .unwrap_or(3_600_000),
                read_timeout_ms: parse_u64_opt(raw.codex.read_timeout_ms.as_ref())?
                    .unwrap_or(5_000),
                stall_timeout_ms: parse_u64_opt(raw.codex.stall_timeout_ms.as_ref())?
                    .unwrap_or(300_000),
            },
            hooks: HookSettings {
                after_create: raw.hooks.after_create,
                before_run: raw.hooks.before_run,
                after_run: raw.hooks.after_run,
                before_remove: raw.hooks.before_remove,
                timeout_ms: parse_positive_or_default(raw.hooks.timeout_ms.as_ref(), 60_000)?,
            },
            observability: ObservabilitySettings {
                dashboard_enabled: raw.observability.dashboard_enabled.unwrap_or(true),
                refresh_ms: parse_positive_or_default(
                    raw.observability.refresh_ms.as_ref(),
                    1_000,
                )?,
                render_interval_ms: parse_positive_or_default(
                    raw.observability.render_interval_ms.as_ref(),
                    16,
                )?,
            },
            server: ServerSettings {
                port: overrides
                    .server_port_override
                    .or(parse_u64_opt(raw.server.port.as_ref())?
                        .and_then(|n| u16::try_from(n).ok())),
                host: raw.server.host.unwrap_or_else(|| "127.0.0.1".to_string()),
            },
        };

        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<()> {
        match self.tracker.kind.as_deref() {
            Some("linear") | Some("memory") => {}
            Some(other) => bail!("unsupported_tracker_kind: {other}"),
            None => bail!("missing_tracker_kind"),
        }

        if self.tracker.kind.as_deref() == Some("linear") && self.tracker.api_key.is_none() {
            bail!("missing_linear_api_token");
        }

        if self.tracker.kind.as_deref() == Some("linear") && self.tracker.project_slug.is_none() {
            bail!("missing_linear_project_slug");
        }

        if self.agent.max_concurrent_agents == 0 {
            bail!("agent.max_concurrent_agents must be > 0");
        }
        if self.agent.max_turns == 0 {
            bail!("agent.max_turns must be > 0");
        }
        if self.codex.command.is_empty() {
            bail!("codex.command must be present and non-empty");
        }
        if self.codex.turn_timeout_ms == 0 {
            bail!("codex.turn_timeout_ms must be > 0");
        }
        if self.codex.read_timeout_ms == 0 {
            bail!("codex.read_timeout_ms must be > 0");
        }
        Ok(())
    }

    pub fn max_concurrent_agents_for_state(&self, state: &str) -> usize {
        self.agent
            .max_concurrent_agents_by_state
            .get(&normalize_issue_state(state))
            .copied()
            .unwrap_or(self.agent.max_concurrent_agents)
    }

    pub fn effective_logs_root(&self, overrides: &CliOverrides) -> PathBuf {
        overrides
            .logs_root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    pub fn default_turn_sandbox_policy(&self, workspace: Option<&Path>) -> JsonValue {
        if let Some(policy) = &self.codex.turn_sandbox_policy {
            return policy.clone();
        }

        let writable_root = workspace
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| self.workspace.root.to_string_lossy().to_string());

        json!({
            "type": "workspaceWrite",
            "writableRoots": [writable_root]
        })
    }
}

pub fn default_prompt_template() -> String {
    [
        "You are working on a Linear issue.",
        "",
        "Identifier: {{ issue.identifier }}",
        "Title: {{ issue.title }}",
        "",
        "Body:",
        "{% if issue.description %}",
        "{{ issue.description }}",
        "{% else %}",
        "No description provided.",
        "{% endif %}",
    ]
    .join("\n")
}

pub fn normalize_issue_state(state: &str) -> String {
    state.trim().to_ascii_lowercase()
}

fn default_workspace_root_string() -> String {
    env::temp_dir()
        .join("symphony_workspaces")
        .to_string_lossy()
        .to_string()
}

fn resolve_env_string(value: Option<String>, canonical_env: Option<&str>) -> Option<String> {
    match value {
        Some(raw) => {
            let trimmed = raw.trim();
            if let Some(rest) = trimmed.strip_prefix('$') {
                env::var(rest).ok().filter(|value| !value.is_empty())
            } else if trimmed.is_empty() {
                canonical_env
                    .and_then(|name| env::var(name).ok())
                    .filter(|value| !value.is_empty())
            } else {
                Some(raw)
            }
        }
        None => canonical_env
            .and_then(|name| env::var(name).ok())
            .filter(|value| !value.is_empty()),
    }
}

fn expand_path_like(value: &str) -> Result<PathBuf> {
    let expanded_env = if let Some(rest) = value.strip_prefix('$') {
        env::var(rest)
            .map(PathBuf::from)
            .map_err(|_| anyhow!("missing env var for path: {rest}"))?
    } else if value == "~" || value.starts_with("~/") {
        let home = env::var("HOME").map_err(|_| anyhow!("HOME is not set"))?;
        if value == "~" {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(value.trim_start_matches("~/"))
        }
    } else {
        PathBuf::from(value)
    };

    Ok(expanded_env)
}

fn parse_positive_or_default(value: Option<&serde_yaml::Value>, default: u64) -> Result<u64> {
    match parse_u64_opt(value)? {
        Some(0) | None => Ok(default),
        Some(value) => Ok(value),
    }
}

fn parse_u64_opt(value: Option<&serde_yaml::Value>) -> Result<Option<u64>> {
    match value {
        Some(value) => parse_u64(value).map(Some),
        None => Ok(None),
    }
}

fn parse_u64(value: &serde_yaml::Value) -> Result<u64> {
    match value {
        serde_yaml::Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| anyhow!("expected positive integer")),
        serde_yaml::Value::String(text) => text
            .trim()
            .parse::<u64>()
            .map_err(|_| anyhow!("expected integer string")),
        _ => bail!("expected integer or integer string"),
    }
}

fn default_approval_policy() -> JsonValue {
    json!({
        "reject": {
            "sandbox_approval": true,
            "rules": true,
            "mcp_elicitations": true
        }
    })
}

pub fn issue_to_liquid_object(
    issue: &crate::tracker::Issue,
    attempt: Option<u32>,
) -> liquid::Object {
    let mut object = liquid::Object::new();
    object.insert(
        "attempt".into(),
        match attempt {
            Some(value) => liquid::model::Value::scalar(value as i64),
            None => liquid::model::Value::Nil,
        },
    );
    object.insert(
        "issue".into(),
        liquid::model::Value::Object(issue.to_liquid_object()),
    );
    object
}

pub fn render_prompt(
    template: &str,
    issue: &crate::tracker::Issue,
    attempt: Option<u32>,
) -> Result<String> {
    let parser = liquid::ParserBuilder::with_stdlib().build()?;
    let parsed = parser.parse(template)?;
    Ok(parsed.render(&issue_to_liquid_object(issue, attempt))?)
}

pub fn summarize_json_for_dashboard(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Null => "null".to_string(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "<invalid-json>".to_string()),
    }
}

pub fn json_object() -> JsonMap<String, JsonValue> {
    JsonMap::new()
}

pub fn millis_duration(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

#[derive(Clone, Debug, Serialize)]
pub struct RefreshPayload {
    pub queued: bool,
    pub coalesced: bool,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub operations: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::LoadedWorkflow;

    fn config_value(yaml: &str) -> LoadedWorkflow {
        let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        LoadedWorkflow {
            config: value,
            prompt_template: "".to_string(),
            prompt: "".to_string(),
        }
    }

    #[test]
    fn parses_defaults() {
        let settings = Settings::from_workflow(
            &config_value("tracker:\n  kind: memory\n"),
            &CliOverrides::default(),
        )
        .unwrap();

        assert_eq!(settings.polling.interval_ms, 30_000);
        assert_eq!(settings.agent.max_turns, 20);
        assert_eq!(settings.tracker.active_states, vec!["Todo", "In Progress"]);
    }

    #[test]
    fn linear_tracker_requires_token() {
        let err = Settings::from_workflow(
            &config_value(
                "tracker:\n  kind: linear\n  project_slug: test\n  api_key: ${LINEAR_API_KEY_MISSING}\n",
            ),
            &CliOverrides::default(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("missing_linear_api_token"));
    }

    #[test]
    fn normalizes_state_limits() {
        let settings = Settings::from_workflow(
            &config_value(
                "tracker:\n  kind: memory\nagent:\n  max_concurrent_agents_by_state:\n    In Progress: 2\n",
            ),
            &CliOverrides::default(),
        )
        .unwrap();

        assert_eq!(settings.max_concurrent_agents_for_state("in progress"), 2);
    }

    #[test]
    fn rejects_empty_codex_command() {
        let err = Settings::from_workflow(
            &config_value("tracker:\n  kind: memory\ncodex:\n  command: \"\"\n"),
            &CliOverrides::default(),
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("codex.command"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn allows_whitespace_codex_command() {
        // Matches Elixir: validate_required only rejects empty string, not whitespace.
        let settings = Settings::from_workflow(
            &config_value("tracker:\n  kind: memory\ncodex:\n  command: \"   \"\n"),
            &CliOverrides::default(),
        )
        .unwrap();

        assert_eq!(settings.codex.command, "   ");
    }
}
