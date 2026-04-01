use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::workflow;

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
    pub provider: ProviderSettings,
    pub hooks: HookSettings,
    pub observability: ObservabilitySettings,
    pub server: ServerSettings,
}

#[derive(Clone, Debug)]
pub struct TrackerSettings {
    pub kind: Option<String>,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub workspace_slug: Option<String>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Codex,
    Claude,
    Gemini,
    Ollama,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Ollama => "ollama",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GeminiOutputFormat {
    StreamJson,
    Json,
}

impl GeminiOutputFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::StreamJson => "stream-json",
            Self::Json => "json",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderSettings {
    pub kind: ProviderKind,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: u64,
    pub codex: CodexProviderSettings,
    pub claude: ClaudeProviderSettings,
    pub gemini: GeminiProviderSettings,
    pub ollama: OllamaProviderSettings,
}

#[derive(Clone, Debug)]
pub struct CodexProviderSettings {
    pub command: String,
    pub approval_policy: JsonValue,
    pub thread_sandbox: String,
    pub turn_sandbox_policy: Option<JsonValue>,
}

#[derive(Clone, Debug)]
pub struct ClaudeProviderSettings {
    pub node_command: String,
    pub entrypoint: Option<PathBuf>,
    pub allowed_tools: Vec<String>,
    pub permission_mode: String,
    pub setting_sources: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct GeminiProviderSettings {
    pub command: String,
    pub output_format: GeminiOutputFormat,
}

#[derive(Clone, Debug)]
pub struct OllamaProviderSettings {
    pub base_url: String,
    pub model: String,
    pub stream: bool,
    pub think: bool,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFormat {
    Toml,
    LegacyWorkflowFrontMatter,
}

impl fmt::Display for ConfigFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml => write!(f, "toml"),
            Self::LegacyWorkflowFrontMatter => write!(f, "legacy_workflow_front_matter"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LoadedConfig {
    pub path: PathBuf,
    pub format: ConfigFormat,
    pub settings: Settings,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigDiagnostic {
    pub code: String,
    pub message: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigDiagnostics {
    pub format: ConfigFormat,
    pub file: String,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

impl ConfigDiagnostics {
    fn single(format: ConfigFormat, file: &Path, diagnostic: ConfigDiagnostic) -> Self {
        Self {
            format,
            file: file.display().to_string(),
            diagnostics: vec![diagnostic],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

impl fmt::Display for ConfigDiagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Configuration invalid in {} ({})",
            self.file, self.format
        )?;
        for diagnostic in &self.diagnostics {
            write!(f, "- {}", diagnostic.message)?;
            if let Some(field_path) = &diagnostic.field_path {
                write!(f, " [{}]", field_path)?;
            }
            write!(f, " ({})", diagnostic.code)?;
            if let Some(line) = diagnostic.line {
                if let Some(column) = diagnostic.column {
                    write!(f, " at {line}:{column}")?;
                } else {
                    write!(f, " at line {line}")?;
                }
            }
            writeln!(f)?;
            if let Some(hint) = &diagnostic.hint {
                writeln!(f, "  hint: {hint}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for ConfigDiagnostics {}

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
    provider: RawProvider,
    #[serde(default)]
    codex: RawCodexProvider,
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
    workspace_slug: Option<String>,
    project_slug: Option<String>,
    assignee: Option<String>,
    active_states: Option<Vec<String>>,
    terminal_states: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct RawPolling {
    interval_ms: Option<FlexibleU64>,
}

#[derive(Debug, Deserialize, Default)]
struct RawWorkspace {
    root: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawWorker {
    ssh_hosts: Option<Vec<String>>,
    max_concurrent_agents_per_host: Option<FlexibleU64>,
}

#[derive(Debug, Deserialize, Default)]
struct RawAgent {
    max_concurrent_agents: Option<FlexibleU64>,
    max_turns: Option<FlexibleU64>,
    max_retry_backoff_ms: Option<FlexibleU64>,
    max_concurrent_agents_by_state: Option<BTreeMap<String, FlexibleU64>>,
}

#[derive(Debug, Deserialize, Default)]
struct RawProvider {
    kind: Option<String>,
    turn_timeout_ms: Option<FlexibleU64>,
    read_timeout_ms: Option<FlexibleU64>,
    stall_timeout_ms: Option<FlexibleU64>,
    #[serde(default)]
    codex: RawCodexProvider,
    #[serde(default)]
    claude: RawClaudeProvider,
    #[serde(default)]
    gemini: RawGeminiProvider,
    #[serde(default)]
    ollama: RawOllamaProvider,
}

#[derive(Debug, Deserialize, Default)]
struct RawCodexProvider {
    command: Option<String>,
    approval_policy: Option<JsonValue>,
    thread_sandbox: Option<String>,
    turn_sandbox_policy: Option<JsonValue>,
}

#[derive(Debug, Deserialize, Default)]
struct RawClaudeProvider {
    node_command: Option<String>,
    entrypoint: Option<String>,
    allowed_tools: Option<Vec<String>>,
    permission_mode: Option<String>,
    setting_sources: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct RawGeminiProvider {
    command: Option<String>,
    output_format: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawOllamaProvider {
    base_url: Option<String>,
    model: Option<String>,
    stream: Option<bool>,
    think: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
struct RawHooks {
    after_create: Option<String>,
    before_run: Option<String>,
    after_run: Option<String>,
    before_remove: Option<String>,
    timeout_ms: Option<FlexibleU64>,
}

#[derive(Debug, Deserialize, Default)]
struct RawObservability {
    dashboard_enabled: Option<bool>,
    refresh_ms: Option<FlexibleU64>,
    render_interval_ms: Option<FlexibleU64>,
}

#[derive(Debug, Deserialize, Default)]
struct RawServer {
    port: Option<FlexibleU64>,
    host: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum FlexibleU64 {
    Number(u64),
    String(String),
}

impl FlexibleU64 {
    fn parse(&self) -> std::result::Result<u64, &'static str> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::String(text) => text
                .trim()
                .parse::<u64>()
                .map_err(|_| "expected integer string"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ValidateOutput {
    pub valid: bool,
    pub config_path: String,
    pub config_format: ConfigFormat,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_path: Option<String>,
}

pub fn config_file_path(explicit: Option<&Path>) -> Result<PathBuf> {
    match explicit {
        Some(path) => Ok(path.canonicalize().unwrap_or_else(|_| path.to_path_buf())),
        None => Ok(std::env::current_dir()?.join(".symphony.toml")),
    }
}

impl Settings {
    pub fn load(
        config_path: &Path,
        workflow_path: Option<&Path>,
        overrides: &CliOverrides,
    ) -> std::result::Result<LoadedConfig, ConfigDiagnostics> {
        if config_path.exists() {
            return load_toml_config(config_path, workflow_path, overrides);
        }

        if let Some(workflow_path) = workflow_path {
            return load_legacy_workflow_config(workflow_path, config_path, overrides);
        }

        Err(ConfigDiagnostics::single(
            ConfigFormat::Toml,
            config_path,
            ConfigDiagnostic {
                code: "missing_config_file".to_string(),
                message: format!("Config file not found: {}", config_path.display()),
                file: config_path.display().to_string(),
                field_path: None,
                line: None,
                column: None,
                hint: Some("Create .symphony.toml or pass --config <path>.".to_string()),
            },
        ))
    }

    pub fn validate(&self) -> Result<()> {
        let diagnostics = self.validation_diagnostics(Path::new("."));
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(ConfigDiagnostics {
                format: ConfigFormat::Toml,
                file: ".".to_string(),
                diagnostics,
            }))
        }
    }

    fn from_raw_root(
        raw: RawRoot,
        config_path: &Path,
        format: ConfigFormat,
        overrides: &CliOverrides,
    ) -> std::result::Result<Self, ConfigDiagnostics> {
        let workspace_root = match expand_path_like(
            raw.workspace
                .root
                .as_deref()
                .unwrap_or(&default_workspace_root_string()),
        ) {
            Ok(path) => path,
            Err(error) => {
                return Err(ConfigDiagnostics::single(
                    format,
                    config_path,
                    ConfigDiagnostic {
                        code: "invalid_workspace_root".to_string(),
                        message: error.to_string(),
                        file: config_path.display().to_string(),
                        field_path: Some("workspace.root".to_string()),
                        line: None,
                        column: None,
                        hint: None,
                    },
                ));
            }
        };

        let tracker_kind = raw.tracker.kind.clone();
        let tracker_api_key = resolve_env_string(raw.tracker.api_key, Some("LINEAR_API_KEY"));
        let tracker_assignee = resolve_env_string(raw.tracker.assignee, Some("LINEAR_ASSIGNEE"));
        let mut parse_diagnostics = Vec::new();
        let provider_kind = parse_provider_kind(
            raw.provider.kind.as_deref(),
            raw_legacy_codex_present(&raw.codex),
            config_path,
            &mut parse_diagnostics,
        );
        let claude_entrypoint = match raw.provider.claude.entrypoint.as_deref() {
            Some(value) => match expand_path_like(value) {
                Ok(path) => Some(path),
                Err(error) => {
                    parse_diagnostics.push(ConfigDiagnostic {
                        code: "invalid_provider_entrypoint".to_string(),
                        message: error.to_string(),
                        file: config_path.display().to_string(),
                        field_path: Some("provider.claude.entrypoint".to_string()),
                        line: None,
                        column: None,
                        hint: None,
                    });
                    None
                }
            },
            None => None,
        };
        let effective_codex = merge_codex_provider_settings(&raw.provider.codex, &raw.codex);
        let agent_state_limits = raw
            .agent
            .max_concurrent_agents_by_state
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(state, value)| match parse_flexible_u64(&value) {
                Ok(n) => usize::try_from(n)
                    .ok()
                    .filter(|n| *n > 0)
                    .map(|n| (normalize_issue_state(&state), n)),
                Err(message) => {
                    parse_diagnostics.push(ConfigDiagnostic {
                        code: "invalid_integer".to_string(),
                        message: message.to_string(),
                        file: config_path.display().to_string(),
                        field_path: Some(format!("agent.max_concurrent_agents_by_state.{state}")),
                        line: None,
                        column: None,
                        hint: Some("Use a positive integer.".to_string()),
                    });
                    None
                }
            })
            .collect::<HashMap<_, _>>();

        let settings = Settings {
            tracker: TrackerSettings {
                kind: tracker_kind,
                endpoint: raw
                    .tracker
                    .endpoint
                    .unwrap_or_else(|| "https://api.linear.app/graphql".to_string()),
                api_key: tracker_api_key,
                workspace_slug: raw.tracker.workspace_slug,
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
                interval_ms: parse_u64_or_default(
                    raw.polling.interval_ms.as_ref(),
                    30_000,
                    "polling.interval_ms",
                    config_path,
                    &mut parse_diagnostics,
                ),
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
                max_concurrent_agents_per_host: parse_usize_opt(
                    raw.worker.max_concurrent_agents_per_host.as_ref(),
                    "worker.max_concurrent_agents_per_host",
                    config_path,
                    &mut parse_diagnostics,
                ),
            },
            agent: AgentSettings {
                max_concurrent_agents: parse_usize_or_default(
                    raw.agent.max_concurrent_agents.as_ref(),
                    10,
                    "agent.max_concurrent_agents",
                    config_path,
                    &mut parse_diagnostics,
                ),
                max_turns: parse_usize_or_default(
                    raw.agent.max_turns.as_ref(),
                    20,
                    "agent.max_turns",
                    config_path,
                    &mut parse_diagnostics,
                ),
                max_retry_backoff_ms: parse_u64_or_default(
                    raw.agent.max_retry_backoff_ms.as_ref(),
                    300_000,
                    "agent.max_retry_backoff_ms",
                    config_path,
                    &mut parse_diagnostics,
                ),
                max_concurrent_agents_by_state: agent_state_limits,
            },
            provider: ProviderSettings {
                kind: provider_kind,
                turn_timeout_ms: parse_u64_or_default(
                    raw.provider.turn_timeout_ms.as_ref(),
                    3_600_000,
                    "provider.turn_timeout_ms",
                    config_path,
                    &mut parse_diagnostics,
                ),
                read_timeout_ms: parse_u64_or_default(
                    raw.provider.read_timeout_ms.as_ref(),
                    5_000,
                    "provider.read_timeout_ms",
                    config_path,
                    &mut parse_diagnostics,
                ),
                stall_timeout_ms: parse_u64_or_default(
                    raw.provider.stall_timeout_ms.as_ref(),
                    300_000,
                    "provider.stall_timeout_ms",
                    config_path,
                    &mut parse_diagnostics,
                ),
                codex: CodexProviderSettings {
                    command: effective_codex
                        .command
                        .unwrap_or_else(|| "codex app-server".to_string()),
                    approval_policy: effective_codex
                        .approval_policy
                        .unwrap_or_else(default_approval_policy),
                    thread_sandbox: effective_codex
                        .thread_sandbox
                        .unwrap_or_else(|| "workspace-write".to_string()),
                    turn_sandbox_policy: effective_codex.turn_sandbox_policy,
                },
                claude: ClaudeProviderSettings {
                    node_command: raw
                        .provider
                        .claude
                        .node_command
                        .unwrap_or_else(|| "node".to_string()),
                    entrypoint: claude_entrypoint,
                    allowed_tools: raw.provider.claude.allowed_tools.unwrap_or_default(),
                    permission_mode: raw
                        .provider
                        .claude
                        .permission_mode
                        .unwrap_or_else(|| "default".to_string()),
                    setting_sources: raw
                        .provider
                        .claude
                        .setting_sources
                        .unwrap_or_else(|| vec!["project".to_string()]),
                },
                gemini: GeminiProviderSettings {
                    command: raw
                        .provider
                        .gemini
                        .command
                        .unwrap_or_else(|| "gemini".to_string()),
                    output_format: parse_gemini_output_format(
                        raw.provider.gemini.output_format.as_deref(),
                        config_path,
                        &mut parse_diagnostics,
                    ),
                },
                ollama: OllamaProviderSettings {
                    base_url: raw
                        .provider
                        .ollama
                        .base_url
                        .unwrap_or_else(|| "http://127.0.0.1:11434".to_string()),
                    model: raw
                        .provider
                        .ollama
                        .model
                        .unwrap_or_else(|| "qwen2.5-coder:latest".to_string()),
                    stream: raw.provider.ollama.stream.unwrap_or(false),
                    think: raw.provider.ollama.think.unwrap_or(false),
                },
            },
            hooks: HookSettings {
                after_create: raw.hooks.after_create,
                before_run: raw.hooks.before_run,
                after_run: raw.hooks.after_run,
                before_remove: raw.hooks.before_remove,
                timeout_ms: parse_positive_or_default(
                    raw.hooks.timeout_ms.as_ref(),
                    60_000,
                    "hooks.timeout_ms",
                    config_path,
                    &mut parse_diagnostics,
                ),
            },
            observability: ObservabilitySettings {
                dashboard_enabled: raw.observability.dashboard_enabled.unwrap_or(true),
                refresh_ms: parse_positive_or_default(
                    raw.observability.refresh_ms.as_ref(),
                    1_000,
                    "observability.refresh_ms",
                    config_path,
                    &mut parse_diagnostics,
                ),
                render_interval_ms: parse_positive_or_default(
                    raw.observability.render_interval_ms.as_ref(),
                    16,
                    "observability.render_interval_ms",
                    config_path,
                    &mut parse_diagnostics,
                ),
            },
            server: ServerSettings {
                port: overrides.server_port_override.or_else(|| {
                    parse_u64_opt(
                        raw.server.port.as_ref(),
                        "server.port",
                        config_path,
                        &mut parse_diagnostics,
                    )
                    .and_then(|n| u16::try_from(n).ok())
                }),
                host: raw.server.host.unwrap_or_else(|| "127.0.0.1".to_string()),
            },
        };

        let mut diagnostics = parse_diagnostics;
        diagnostics.extend(settings.validation_diagnostics(config_path));
        if diagnostics.is_empty() {
            Ok(settings)
        } else {
            Err(ConfigDiagnostics {
                format,
                file: config_path.display().to_string(),
                diagnostics,
            })
        }
    }

    fn validation_diagnostics(&self, config_path: &Path) -> Vec<ConfigDiagnostic> {
        let mut diagnostics = Vec::new();
        let file = config_path.display().to_string();

        match self.tracker.kind.as_deref() {
            Some("linear") | Some("memory") => {}
            Some(other) => diagnostics.push(ConfigDiagnostic {
                code: "unsupported_tracker_kind".to_string(),
                message: format!("Unsupported tracker kind `{other}`."),
                file: file.clone(),
                field_path: Some("tracker.kind".to_string()),
                line: None,
                column: None,
                hint: Some("Use `linear` or `memory`.".to_string()),
            }),
            None => diagnostics.push(ConfigDiagnostic {
                code: "missing_tracker_kind".to_string(),
                message: "Missing tracker kind.".to_string(),
                file: file.clone(),
                field_path: Some("tracker.kind".to_string()),
                line: None,
                column: None,
                hint: Some("Set `tracker.kind = \"linear\"` or `\"memory\"`.".to_string()),
            }),
        }

        if self.tracker.kind.as_deref() == Some("linear") && self.tracker.api_key.is_none() {
            diagnostics.push(ConfigDiagnostic {
                code: "missing_linear_api_token".to_string(),
                message: "Missing Linear API token.".to_string(),
                file: file.clone(),
                field_path: Some("tracker.api_key".to_string()),
                line: None,
                column: None,
                hint: Some(
                    "Set `tracker.api_key` in `.symphony.toml` or export `LINEAR_API_KEY`."
                        .to_string(),
                ),
            });
        }

        if self.tracker.kind.as_deref() == Some("linear") && self.tracker.project_slug.is_none() {
            diagnostics.push(ConfigDiagnostic {
                code: "missing_linear_project_slug".to_string(),
                message: "Missing Linear project slug.".to_string(),
                file: file.clone(),
                field_path: Some("tracker.project_slug".to_string()),
                line: None,
                column: None,
                hint: Some("Set `tracker.project_slug`.".to_string()),
            });
        }

        for (field_path, value) in [
            (
                "agent.max_concurrent_agents",
                self.agent.max_concurrent_agents as u64,
            ),
            ("agent.max_turns", self.agent.max_turns as u64),
            ("provider.turn_timeout_ms", self.provider.turn_timeout_ms),
            ("provider.read_timeout_ms", self.provider.read_timeout_ms),
        ] {
            if value == 0 {
                diagnostics.push(ConfigDiagnostic {
                    code: "non_positive_integer".to_string(),
                    message: format!("`{field_path}` must be greater than 0."),
                    file: file.clone(),
                    field_path: Some(field_path.to_string()),
                    line: None,
                    column: None,
                    hint: Some("Use a positive integer.".to_string()),
                });
            }
        }

        if matches!(self.provider.kind, ProviderKind::Codex)
            && self.provider.codex.command.trim().is_empty()
        {
            diagnostics.push(ConfigDiagnostic {
                code: "empty_codex_command".to_string(),
                message: "`provider.codex.command` must be present and non-empty.".to_string(),
                file,
                field_path: Some("provider.codex.command".to_string()),
                line: None,
                column: None,
                hint: Some("Set it to something like `codex app-server`.".to_string()),
            });
        }

        if matches!(self.provider.kind, ProviderKind::Claude)
            && self.provider.claude.node_command.trim().is_empty()
        {
            diagnostics.push(ConfigDiagnostic {
                code: "empty_provider_command".to_string(),
                message: "`provider.claude.node_command` must be present and non-empty."
                    .to_string(),
                file: config_path.display().to_string(),
                field_path: Some("provider.claude.node_command".to_string()),
                line: None,
                column: None,
                hint: Some("Set it to a Node runtime such as `node`.".to_string()),
            });
        }

        if matches!(self.provider.kind, ProviderKind::Gemini)
            && self.provider.gemini.command.trim().is_empty()
        {
            diagnostics.push(ConfigDiagnostic {
                code: "empty_provider_command".to_string(),
                message: "`provider.gemini.command` must be present and non-empty.".to_string(),
                file: config_path.display().to_string(),
                field_path: Some("provider.gemini.command".to_string()),
                line: None,
                column: None,
                hint: Some("Set it to a Gemini CLI command.".to_string()),
            });
        }

        if matches!(self.provider.kind, ProviderKind::Ollama)
            && self.provider.ollama.model.trim().is_empty()
        {
            diagnostics.push(ConfigDiagnostic {
                code: "missing_provider_model".to_string(),
                message: "`provider.ollama.model` must be present and non-empty.".to_string(),
                file: config_path.display().to_string(),
                field_path: Some("provider.ollama.model".to_string()),
                line: None,
                column: None,
                hint: Some("Set it to an installed Ollama model name.".to_string()),
            });
        }

        diagnostics
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
        if let Some(policy) = &self.provider.codex.turn_sandbox_policy {
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

fn load_toml_config(
    config_path: &Path,
    workflow_path: Option<&Path>,
    overrides: &CliOverrides,
) -> std::result::Result<LoadedConfig, ConfigDiagnostics> {
    let content = std::fs::read_to_string(config_path).map_err(|error| {
        ConfigDiagnostics::single(
            ConfigFormat::Toml,
            config_path,
            ConfigDiagnostic {
                code: "config_read_error".to_string(),
                message: format!("Failed to read config file: {error}"),
                file: config_path.display().to_string(),
                field_path: None,
                line: None,
                column: None,
                hint: None,
            },
        )
    })?;

    let raw = toml::from_str::<RawRoot>(&content).map_err(|error| {
        let (line, column) = span_to_line_col(error.span(), &content);
        ConfigDiagnostics::single(
            ConfigFormat::Toml,
            config_path,
            ConfigDiagnostic {
                code: "config_parse_error".to_string(),
                message: error.to_string(),
                file: config_path.display().to_string(),
                field_path: None,
                line,
                column,
                hint: Some("Check TOML syntax and field types.".to_string()),
            },
        )
    })?;

    let legacy_codex_present = raw_legacy_codex_present(&raw.codex);
    let provider_kind = raw.provider.kind.clone();
    let settings = Settings::from_raw_root(raw, config_path, ConfigFormat::Toml, overrides)?;
    let mut warnings = Vec::new();
    if legacy_codex_present {
        warnings.push(
            "Legacy `[codex]` config is deprecated; move settings under `[provider]`.".to_string(),
        );
        if provider_kind.is_some() {
            warnings.push(
                "Both `[provider]` and legacy `[codex]` config are present; `[provider]` takes precedence."
                    .to_string(),
            );
        }
    }
    if workflow_path.is_some_and(|path| path.exists())
        && workflow::has_front_matter(workflow_path.expect("checked above")).unwrap_or(false)
    {
        warnings.push(format!(
            "Ignoring legacy WORKFLOW.md front matter because {} exists.",
            config_path.display()
        ));
    }
    Ok(LoadedConfig {
        path: config_path.to_path_buf(),
        format: ConfigFormat::Toml,
        settings,
        warnings,
    })
}

fn load_legacy_workflow_config(
    workflow_path: &Path,
    config_path: &Path,
    overrides: &CliOverrides,
) -> std::result::Result<LoadedConfig, ConfigDiagnostics> {
    let raw_yaml = workflow::load_legacy_front_matter(workflow_path).map_err(|error| {
        ConfigDiagnostics::single(
            ConfigFormat::LegacyWorkflowFrontMatter,
            workflow_path,
            ConfigDiagnostic {
                code: "legacy_workflow_read_error".to_string(),
                message: error.to_string(),
                file: workflow_path.display().to_string(),
                field_path: None,
                line: None,
                column: None,
                hint: Some(format!(
                    "Create {} to move runtime config out of WORKFLOW.md.",
                    config_path.display()
                )),
            },
        )
    })?;

    let raw = serde_yaml::from_value::<RawRoot>(raw_yaml).map_err(|error| {
        let location = error.location();
        ConfigDiagnostics::single(
            ConfigFormat::LegacyWorkflowFrontMatter,
            workflow_path,
            ConfigDiagnostic {
                code: "legacy_workflow_parse_error".to_string(),
                message: error.to_string(),
                file: workflow_path.display().to_string(),
                field_path: None,
                line: location.as_ref().map(|location| location.line()),
                column: location.as_ref().map(|location| location.column()),
                hint: Some(format!(
                    "Move runtime config into {}. Legacy WORKFLOW.md front matter is deprecated.",
                    config_path.display()
                )),
            },
        )
    })?;

    let settings = Settings::from_raw_root(
        raw,
        workflow_path,
        ConfigFormat::LegacyWorkflowFrontMatter,
        overrides,
    )?;
    Ok(LoadedConfig {
        path: workflow_path.to_path_buf(),
        format: ConfigFormat::LegacyWorkflowFrontMatter,
        settings,
        warnings: vec![format!(
            "Using deprecated WORKFLOW.md front matter for runtime config because {} was not found.",
            config_path.display()
        )],
    })
}

pub fn validate(
    config_path: &Path,
    workflow_path: Option<&Path>,
    overrides: &CliOverrides,
) -> std::result::Result<ValidateOutput, ValidateOutput> {
    match Settings::load(config_path, workflow_path, overrides) {
        Ok(loaded) => {
            if let Some(path) = workflow_path {
                crate::workflow::load(path).map_err(|error| ValidateOutput {
                    valid: false,
                    config_path: loaded.path.display().to_string(),
                    config_format: loaded.format,
                    warnings: loaded.warnings.clone(),
                    diagnostics: vec![ConfigDiagnostic {
                        code: "workflow_read_error".to_string(),
                        message: error.to_string(),
                        file: path.display().to_string(),
                        field_path: None,
                        line: None,
                        column: None,
                        hint: None,
                    }],
                    workflow_path: Some(path.display().to_string()),
                })?;
            }

            Ok(ValidateOutput {
                valid: true,
                config_path: loaded.path.display().to_string(),
                config_format: loaded.format,
                warnings: loaded.warnings,
                diagnostics: Vec::new(),
                workflow_path: workflow_path.map(|path| path.display().to_string()),
            })
        }
        Err(error) => Err(ValidateOutput {
            valid: false,
            config_path: error.file.clone(),
            config_format: error.format,
            warnings: Vec::new(),
            diagnostics: error.diagnostics,
            workflow_path: workflow_path.map(|path| path.display().to_string()),
        }),
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

fn parse_flexible_u64(value: &FlexibleU64) -> std::result::Result<u64, &'static str> {
    value.parse()
}

fn parse_u64_opt(
    value: Option<&FlexibleU64>,
    field_path: &str,
    config_path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<u64> {
    value.and_then(|value| match parse_flexible_u64(value) {
        Ok(parsed) => Some(parsed),
        Err(message) => {
            diagnostics.push(ConfigDiagnostic {
                code: "invalid_integer".to_string(),
                message: message.to_string(),
                file: config_path.display().to_string(),
                field_path: Some(field_path.to_string()),
                line: None,
                column: None,
                hint: Some("Use a positive integer.".to_string()),
            });
            None
        }
    })
}

fn parse_u64_or_default(
    value: Option<&FlexibleU64>,
    default: u64,
    field_path: &str,
    config_path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> u64 {
    parse_u64_opt(value, field_path, config_path, diagnostics).unwrap_or(default)
}

fn parse_positive_or_default(
    value: Option<&FlexibleU64>,
    default: u64,
    field_path: &str,
    config_path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> u64 {
    match parse_u64_opt(value, field_path, config_path, diagnostics) {
        Some(0) | None => default,
        Some(value) => value,
    }
}

fn parse_usize_opt(
    value: Option<&FlexibleU64>,
    field_path: &str,
    config_path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<usize> {
    parse_u64_opt(value, field_path, config_path, diagnostics)
        .and_then(|value| usize::try_from(value).ok())
}

fn parse_usize_or_default(
    value: Option<&FlexibleU64>,
    default: usize,
    field_path: &str,
    config_path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> usize {
    parse_usize_opt(value, field_path, config_path, diagnostics).unwrap_or(default)
}

fn parse_provider_kind(
    raw: Option<&str>,
    legacy_codex_present: bool,
    config_path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> ProviderKind {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some("codex") => ProviderKind::Codex,
        Some("claude") => ProviderKind::Claude,
        Some("gemini") => ProviderKind::Gemini,
        Some("ollama") => ProviderKind::Ollama,
        Some(other) => {
            diagnostics.push(ConfigDiagnostic {
                code: "unsupported_provider_kind".to_string(),
                message: format!("Unsupported provider kind `{other}`."),
                file: config_path.display().to_string(),
                field_path: Some("provider.kind".to_string()),
                line: None,
                column: None,
                hint: Some("Use `codex`, `claude`, `gemini`, or `ollama`.".to_string()),
            });
            ProviderKind::Codex
        }
        None if legacy_codex_present => ProviderKind::Codex,
        None => ProviderKind::Codex,
    }
}

fn parse_gemini_output_format(
    raw: Option<&str>,
    config_path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> GeminiOutputFormat {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        Some("stream-json") => GeminiOutputFormat::StreamJson,
        Some("json") => GeminiOutputFormat::Json,
        Some(other) => {
            diagnostics.push(ConfigDiagnostic {
                code: "unsupported_gemini_output_format".to_string(),
                message: format!("Unsupported Gemini output format `{other}`."),
                file: config_path.display().to_string(),
                field_path: Some("provider.gemini.output_format".to_string()),
                line: None,
                column: None,
                hint: Some("Use `stream-json` or `json`.".to_string()),
            });
            GeminiOutputFormat::StreamJson
        }
        None => GeminiOutputFormat::StreamJson,
    }
}

fn raw_legacy_codex_present(raw: &RawCodexProvider) -> bool {
    raw.command.is_some()
        || raw.approval_policy.is_some()
        || raw.thread_sandbox.is_some()
        || raw.turn_sandbox_policy.is_some()
}

fn merge_codex_provider_settings(
    provider: &RawCodexProvider,
    legacy: &RawCodexProvider,
) -> RawCodexProvider {
    RawCodexProvider {
        command: provider.command.clone().or_else(|| legacy.command.clone()),
        approval_policy: provider
            .approval_policy
            .clone()
            .or_else(|| legacy.approval_policy.clone()),
        thread_sandbox: provider
            .thread_sandbox
            .clone()
            .or_else(|| legacy.thread_sandbox.clone()),
        turn_sandbox_policy: provider
            .turn_sandbox_policy
            .clone()
            .or_else(|| legacy.turn_sandbox_policy.clone()),
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

fn span_to_line_col(span: Option<Range<usize>>, content: &str) -> (Option<usize>, Option<usize>) {
    let Some(start) = span.map(|range| range.start) else {
        return (None, None);
    };

    let mut line = 1usize;
    let mut column = 1usize;
    for ch in content[..start.min(content.len())].chars() {
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (Some(line), Some(column))
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

#[doc(hidden)]
pub fn settings_from_toml_str(toml: &str) -> Settings {
    let raw = toml::from_str::<RawRoot>(toml).unwrap();
    Settings::from_raw_root(
        raw,
        Path::new(".symphony.toml"),
        ConfigFormat::Toml,
        &CliOverrides::default(),
    )
    .unwrap()
}

#[allow(dead_code)]
#[doc(hidden)]
pub fn settings_from_legacy_yaml_str(yaml: &str) -> Settings {
    let raw = serde_yaml::from_str::<RawRoot>(yaml).unwrap();
    Settings::from_raw_root(
        raw,
        Path::new("WORKFLOW.md"),
        ConfigFormat::LegacyWorkflowFrontMatter,
        &CliOverrides::default(),
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_defaults() {
        let settings = settings_from_toml_str("[tracker]\nkind = \"memory\"\n");
        assert_eq!(settings.polling.interval_ms, 30_000);
        assert_eq!(settings.agent.max_turns, 20);
        assert_eq!(settings.tracker.active_states, vec!["Todo", "In Progress"]);
    }

    #[test]
    fn linear_tracker_requires_token() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join(".symphony.toml");
        fs::write(
            &config_path,
            "[tracker]\nkind = \"linear\"\nproject_slug = \"test\"\napi_key = \"$LINEAR_API_KEY_MISSING\"\n",
        )
        .unwrap();

        let err = Settings::load(&config_path, None, &CliOverrides::default()).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "missing_linear_api_token")
        );
    }

    #[test]
    fn parses_linear_workspace_slug() {
        let settings = settings_from_toml_str(
            "[tracker]\nkind = \"linear\"\nworkspace_slug = \"weaveteam\"\nproject_slug = \"test\"\napi_key = \"token\"\n",
        );
        assert_eq!(
            settings.tracker.workspace_slug.as_deref(),
            Some("weaveteam")
        );
    }

    #[test]
    fn normalizes_state_limits() {
        let settings = settings_from_toml_str(
            "[tracker]\nkind = \"memory\"\n[agent.max_concurrent_agents_by_state]\n\"In Progress\" = 2\n",
        );
        assert_eq!(settings.max_concurrent_agents_for_state("in progress"), 2);
    }

    #[test]
    fn rejects_empty_codex_command() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join(".symphony.toml");
        fs::write(
            &config_path,
            "[tracker]\nkind = \"memory\"\n[codex]\ncommand = \"\"\n",
        )
        .unwrap();

        let err = Settings::load(&config_path, None, &CliOverrides::default()).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "empty_codex_command")
        );
    }

    #[test]
    fn validates_legacy_front_matter_when_toml_missing() {
        let temp = tempfile::tempdir().unwrap();
        let workflow_path = temp.path().join("WORKFLOW.md");
        fs::write(
            &workflow_path,
            "---\ntracker:\n  kind: memory\n---\nPrompt\n",
        )
        .unwrap();

        let loaded = Settings::load(
            &temp.path().join(".symphony.toml"),
            Some(&workflow_path),
            &CliOverrides::default(),
        )
        .unwrap();
        assert_eq!(loaded.format, ConfigFormat::LegacyWorkflowFrontMatter);
        assert_eq!(loaded.settings.tracker.kind.as_deref(), Some("memory"));
    }

    #[test]
    fn toml_takes_precedence_over_workflow_front_matter() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join(".symphony.toml");
        let workflow_path = temp.path().join("WORKFLOW.md");
        fs::write(&config_path, "[tracker]\nkind = \"memory\"\n").unwrap();
        fs::write(
            &workflow_path,
            "---\ntracker:\n  kind: linear\n---\nPrompt\n",
        )
        .unwrap();

        let loaded =
            Settings::load(&config_path, Some(&workflow_path), &CliOverrides::default()).unwrap();
        assert_eq!(loaded.format, ConfigFormat::Toml);
        assert!(!loaded.warnings.is_empty());
    }

    #[test]
    fn validate_returns_structured_error_output() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join(".symphony.toml");
        fs::write(&config_path, "[tracker]\nkind = \"linear\"\n").unwrap();

        let output = validate(&config_path, None, &CliOverrides::default()).unwrap_err();
        assert!(!output.valid);
        assert_eq!(output.config_format, ConfigFormat::Toml);
        assert!(!output.diagnostics.is_empty());
    }

    #[test]
    fn validate_reports_workflow_read_error_when_prompt_missing() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join(".symphony.toml");
        let workflow_path = temp.path().join("WORKFLOW.md");
        fs::write(&config_path, "[tracker]\nkind = \"memory\"\n").unwrap();

        let output =
            validate(&config_path, Some(&workflow_path), &CliOverrides::default()).unwrap_err();
        assert!(!output.valid);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "workflow_read_error")
        );
    }
}
