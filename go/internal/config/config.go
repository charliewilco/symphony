package config

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	toml "github.com/pelletier/go-toml/v2"
)

type Config struct {
	Tracker   TrackerConfig   `toml:"tracker" json:"tracker"`
	Polling   PollingConfig   `toml:"polling" json:"polling"`
	Workspace WorkspaceConfig `toml:"workspace" json:"workspace"`
	Hooks     HooksConfig     `toml:"hooks" json:"hooks"`
	Agent     AgentConfig     `toml:"agent" json:"agent"`
	Codex     CodexConfig     `toml:"codex" json:"codex"`
}

type TrackerConfig struct {
	Kind           string   `toml:"kind" json:"kind"`
	WorkspaceSlug  string   `toml:"workspace_slug" json:"workspace_slug"`
	ProjectSlug    string   `toml:"project_slug" json:"project_slug"`
	APIToken       string   `toml:"api_token" json:"api_token"`
	ActiveStates   []string `toml:"active_states" json:"active_states"`
	TerminalStates []string `toml:"terminal_states" json:"terminal_states"`
}

type PollingConfig struct {
	IntervalMS int `toml:"interval_ms" json:"interval_ms"`
}
type WorkspaceConfig struct {
	Root string `toml:"root" json:"root"`
}
type HooksConfig struct {
	AfterCreate  string `toml:"after_create" json:"after_create"`
	BeforeRun    string `toml:"before_run" json:"before_run"`
	AfterRun     string `toml:"after_run" json:"after_run"`
	BeforeRemove string `toml:"before_remove" json:"before_remove"`
	TimeoutMS    int    `toml:"timeout_ms" json:"timeout_ms"`
}
type AgentConfig struct {
	MaxConcurrentAgents        int            `toml:"max_concurrent_agents" json:"max_concurrent_agents"`
	MaxConcurrentAgentsByState map[string]int `toml:"max_concurrent_agents_by_state" json:"max_concurrent_agents_by_state"`
	MaxTurns                   int            `toml:"max_turns" json:"max_turns"`
	MaxRetryBackoffMS          int            `toml:"max_retry_backoff_ms" json:"max_retry_backoff_ms"`
}
type CodexConfig struct {
	Command           string         `toml:"command" json:"command"`
	ApprovalPolicy    string         `toml:"approval_policy" json:"approval_policy"`
	ThreadSandbox     string         `toml:"thread_sandbox" json:"thread_sandbox"`
	TurnSandboxPolicy map[string]any `toml:"turn_sandbox_policy" json:"turn_sandbox_policy"`
	StallTimeoutMS    int            `toml:"stall_timeout_ms" json:"stall_timeout_ms"`
	ReadTimeoutMS     int            `toml:"read_timeout_ms" json:"read_timeout_ms"`
	TurnTimeoutMS     int            `toml:"turn_timeout_ms" json:"turn_timeout_ms"`
}

type Diagnostic struct {
	Code      string `json:"code"`
	Message   string `json:"message"`
	File      string `json:"file"`
	FieldPath string `json:"field_path"`
	Line      int    `json:"line,omitempty"`
	Column    int    `json:"column,omitempty"`
	Hint      string `json:"hint,omitempty"`
}

func DefaultConfig() Config {
	return Config{
		Tracker: TrackerConfig{Kind: "linear", ActiveStates: []string{"Todo", "In Progress", "Merging", "Rework"}, TerminalStates: []string{"Closed", "Cancelled", "Canceled", "Duplicate", "Done"}},
		Polling: PollingConfig{IntervalMS: 5000},
		Agent:   AgentConfig{MaxConcurrentAgents: 10, MaxTurns: 20, MaxRetryBackoffMS: 300000},
		Hooks:   HooksConfig{TimeoutMS: 300000},
		Codex:   CodexConfig{ReadTimeoutMS: 60000, TurnTimeoutMS: 600000},
	}
}

func Load(path string) (Config, error) {
	cfg := DefaultConfig()
	b, err := os.ReadFile(path)
	if err != nil {
		return cfg, err
	}
	if err := toml.Unmarshal(b, &cfg); err != nil {
		return cfg, fmt.Errorf("parse toml: %w", err)
	}
	return cfg, nil
}

func Validate(cfg Config, path string) []Diagnostic {
	var ds []Diagnostic
	add := func(code, msg, field, hint string) {
		ds = append(ds, Diagnostic{Code: code, Message: msg, File: path, FieldPath: field, Hint: hint})
	}
	if cfg.Tracker.Kind == "linear" && strings.TrimSpace(cfg.Tracker.APIToken) == "" {
		add("missing_linear_api_token", "Linear API token is required", "tracker.api_token", "Set tracker.api_token or LINEAR_API_TOKEN")
	}
	if strings.TrimSpace(cfg.Tracker.ProjectSlug) == "" {
		add("missing_project_slug", "Tracker project slug is required", "tracker.project_slug", "Set tracker.project_slug")
	}
	if strings.TrimSpace(cfg.Tracker.WorkspaceSlug) == "" {
		add("missing_workspace_slug", "Tracker workspace slug is required", "tracker.workspace_slug", "Set tracker.workspace_slug")
	}
	if cfg.Agent.MaxConcurrentAgents <= 0 {
		add("invalid_max_concurrent_agents", "max_concurrent_agents must be > 0", "agent.max_concurrent_agents", "Set a positive integer")
	}
	if cfg.Agent.MaxTurns <= 0 {
		add("invalid_max_turns", "max_turns must be > 0", "agent.max_turns", "Set a positive integer")
	}
	if strings.TrimSpace(cfg.Codex.Command) == "" {
		add("missing_codex_command", "codex.command is required", "codex.command", "Set the codex app-server command")
	}
	return ds
}

func DiagnosticsJSON(ds []Diagnostic) (string, error) {
	b, err := json.MarshalIndent(map[string]any{"diagnostics": ds}, "", "  ")
	return string(b), err
}
