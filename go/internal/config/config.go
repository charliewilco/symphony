// Package config provides typed settings from workflow YAML front matter.
package config

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"symphony/internal/workflow"

	"github.com/osteele/liquid"
	"gopkg.in/yaml.v3"
)

// CliOverrides holds CLI flag overrides.
type CliOverrides struct {
	LogsRoot           string
	ServerPortOverride *int
}

// Settings is the fully resolved typed config.
type Settings struct {
	Tracker       TrackerSettings
	Polling       PollingSettings
	Workspace     WorkspaceSettings
	Worker        WorkerSettings
	Agent         AgentSettings
	Codex         CodexSettings
	Hooks         HookSettings
	Observability ObservabilitySettings
	Server        ServerSettings
}

type TrackerSettings struct {
	Kind           string
	Endpoint       string
	APIKey         string
	ProjectSlug    string
	Assignee       string
	ActiveStates   []string
	TerminalStates []string
}

type PollingSettings struct {
	IntervalMs int64
}

type WorkspaceSettings struct {
	Root string
}

type WorkerSettings struct {
	SSHHosts                   []string
	MaxConcurrentAgentsPerHost *int
}

type AgentSettings struct {
	MaxConcurrentAgents        int
	MaxTurns                   int
	MaxRetryBackoffMs          int64
	MaxConcurrentAgentsByState map[string]int
}

type CodexSettings struct {
	Command           string
	ApprovalPolicy    any
	ThreadSandbox     string
	TurnSandboxPolicy any
	TurnTimeoutMs     int64
	ReadTimeoutMs     int64
	StallTimeoutMs    int64
}

type HookSettings struct {
	AfterCreate  string
	BeforeRun    string
	AfterRun     string
	BeforeRemove string
	TimeoutMs    int64
}

type ObservabilitySettings struct {
	DashboardEnabled bool
	RefreshMs        int64
	RenderIntervalMs int64
}

type ServerSettings struct {
	Port *int
	Host string
}

// RefreshPayload is returned from the refresh API endpoint.
type RefreshPayload struct {
	Queued      bool     `json:"queued"`
	Coalesced   bool     `json:"coalesced"`
	RequestedAt string   `json:"requested_at"`
	Operations  []string `json:"operations"`
}

// rawRoot mirrors the YAML front matter structure for unmarshalling.
type rawRoot struct {
	Tracker       rawTracker       `yaml:"tracker"`
	Polling       rawPolling       `yaml:"polling"`
	Workspace     rawWorkspace     `yaml:"workspace"`
	Worker        rawWorker        `yaml:"worker"`
	Agent         rawAgent         `yaml:"agent"`
	Codex         rawCodex         `yaml:"codex"`
	Hooks         rawHooks         `yaml:"hooks"`
	Observability rawObservability `yaml:"observability"`
	Server        rawServer        `yaml:"server"`
}

type rawTracker struct {
	Kind           *string  `yaml:"kind"`
	Endpoint       *string  `yaml:"endpoint"`
	APIKey         *string  `yaml:"api_key"`
	ProjectSlug    *string  `yaml:"project_slug"`
	Assignee       *string  `yaml:"assignee"`
	ActiveStates   []string `yaml:"active_states"`
	TerminalStates []string `yaml:"terminal_states"`
}

type rawPolling struct {
	IntervalMs any `yaml:"interval_ms"`
}

type rawWorkspace struct {
	Root *string `yaml:"root"`
}

type rawWorker struct {
	SSHHosts                   []string `yaml:"ssh_hosts"`
	MaxConcurrentAgentsPerHost any      `yaml:"max_concurrent_agents_per_host"`
}

type rawAgent struct {
	MaxConcurrentAgents        any            `yaml:"max_concurrent_agents"`
	MaxTurns                   any            `yaml:"max_turns"`
	MaxRetryBackoffMs          any            `yaml:"max_retry_backoff_ms"`
	MaxConcurrentAgentsByState map[string]any `yaml:"max_concurrent_agents_by_state"`
}

type rawCodex struct {
	Command           *string `yaml:"command"`
	ApprovalPolicy    any     `yaml:"approval_policy"`
	ThreadSandbox     *string `yaml:"thread_sandbox"`
	TurnSandboxPolicy any     `yaml:"turn_sandbox_policy"`
	TurnTimeoutMs     any     `yaml:"turn_timeout_ms"`
	ReadTimeoutMs     any     `yaml:"read_timeout_ms"`
	StallTimeoutMs    any     `yaml:"stall_timeout_ms"`
}

type rawHooks struct {
	AfterCreate  *string `yaml:"after_create"`
	BeforeRun    *string `yaml:"before_run"`
	AfterRun     *string `yaml:"after_run"`
	BeforeRemove *string `yaml:"before_remove"`
	TimeoutMs    any     `yaml:"timeout_ms"`
}

type rawObservability struct {
	DashboardEnabled *bool `yaml:"dashboard_enabled"`
	RefreshMs        any   `yaml:"refresh_ms"`
	RenderIntervalMs any   `yaml:"render_interval_ms"`
}

type rawServer struct {
	Port any     `yaml:"port"`
	Host *string `yaml:"host"`
}

// FromWorkflow builds Settings from a loaded workflow and CLI overrides.
func FromWorkflow(w *workflow.LoadedWorkflow, overrides *CliOverrides) (*Settings, error) {
	// Re-marshal and unmarshal through yaml to get proper typing
	yamlBytes, err := yaml.Marshal(w.Config)
	if err != nil {
		return nil, fmt.Errorf("config marshal error: %w", err)
	}
	var raw rawRoot
	if err := yaml.Unmarshal(yamlBytes, &raw); err != nil {
		return nil, fmt.Errorf("config unmarshal error: %w", err)
	}

	workspaceRoot := defaultWorkspaceRoot()
	if raw.Workspace.Root != nil {
		workspaceRoot = *raw.Workspace.Root
	}
	expandedRoot, err := expandPathLike(workspaceRoot)
	if err != nil {
		return nil, err
	}

	trackerAPIKey := resolveEnvString(raw.Tracker.APIKey, "LINEAR_API_KEY")
	trackerAssignee := resolveEnvString(raw.Tracker.Assignee, "LINEAR_ASSIGNEE")

	activeStates := raw.Tracker.ActiveStates
	if len(activeStates) == 0 {
		activeStates = []string{"Todo", "In Progress"}
	}
	terminalStates := raw.Tracker.TerminalStates
	if len(terminalStates) == 0 {
		terminalStates = []string{"Closed", "Cancelled", "Canceled", "Duplicate", "Done"}
	}

	endpoint := "https://api.linear.app/graphql"
	if raw.Tracker.Endpoint != nil && *raw.Tracker.Endpoint != "" {
		endpoint = *raw.Tracker.Endpoint
	}

	trackerKind := ""
	if raw.Tracker.Kind != nil {
		trackerKind = *raw.Tracker.Kind
	}
	projectSlug := ""
	if raw.Tracker.ProjectSlug != nil {
		projectSlug = *raw.Tracker.ProjectSlug
	}

	// Agent state limits
	agentStateLimits := make(map[string]int)
	for state, val := range raw.Agent.MaxConcurrentAgentsByState {
		n, err := parsePositiveInt(val)
		if err != nil || n <= 0 {
			continue
		}
		agentStateLimits[NormalizeIssueState(state)] = n
	}

	maxConcurrent := 10
	if v, err := parsePositiveInt(raw.Agent.MaxConcurrentAgents); err == nil && v > 0 {
		maxConcurrent = v
	}
	maxTurns := 20
	if v, err := parsePositiveInt(raw.Agent.MaxTurns); err == nil && v > 0 {
		maxTurns = v
	}
	maxRetryBackoff := int64(300000)
	if v, err := parseInt64(raw.Agent.MaxRetryBackoffMs); err == nil && v > 0 {
		maxRetryBackoff = v
	}

	codexCommand := "codex app-server"
	if raw.Codex.Command != nil {
		codexCommand = *raw.Codex.Command
	}

	approvalPolicy := defaultApprovalPolicy()
	if raw.Codex.ApprovalPolicy != nil {
		approvalPolicy = raw.Codex.ApprovalPolicy
	}
	threadSandbox := "workspace-write"
	if raw.Codex.ThreadSandbox != nil {
		threadSandbox = *raw.Codex.ThreadSandbox
	}

	turnTimeout := int64(3600000)
	if v, err := parseInt64(raw.Codex.TurnTimeoutMs); err == nil && v > 0 {
		turnTimeout = v
	}
	readTimeout := int64(5000)
	if v, err := parseInt64(raw.Codex.ReadTimeoutMs); err == nil && v > 0 {
		readTimeout = v
	}
	stallTimeout := int64(300000)
	if v, err := parseInt64(raw.Codex.StallTimeoutMs); err == nil {
		stallTimeout = v
	}

	hookTimeout := int64(60000)
	if v, err := parseInt64(raw.Hooks.TimeoutMs); err == nil && v > 0 {
		hookTimeout = v
	}

	dashboardEnabled := true
	if raw.Observability.DashboardEnabled != nil {
		dashboardEnabled = *raw.Observability.DashboardEnabled
	}
	refreshMs := int64(1000)
	if v, err := parseInt64(raw.Observability.RefreshMs); err == nil && v > 0 {
		refreshMs = v
	}
	renderIntervalMs := int64(16)
	if v, err := parseInt64(raw.Observability.RenderIntervalMs); err == nil && v > 0 {
		renderIntervalMs = v
	}

	pollInterval := int64(30000)
	if v, err := parseInt64(raw.Polling.IntervalMs); err == nil && v > 0 {
		pollInterval = v
	}

	sshHosts := make([]string, 0)
	for _, h := range raw.Worker.SSHHosts {
		trimmed := strings.TrimSpace(h)
		if trimmed != "" {
			sshHosts = append(sshHosts, trimmed)
		}
	}

	var maxPerHost *int
	if v, err := parsePositiveInt(raw.Worker.MaxConcurrentAgentsPerHost); err == nil && v > 0 {
		maxPerHost = &v
	}

	serverHost := "127.0.0.1"
	if raw.Server.Host != nil {
		serverHost = *raw.Server.Host
	}
	var serverPort *int
	if overrides != nil && overrides.ServerPortOverride != nil {
		serverPort = overrides.ServerPortOverride
	} else if v, err := parsePositiveInt(raw.Server.Port); err == nil {
		serverPort = &v
	}

	s := &Settings{
		Tracker: TrackerSettings{
			Kind:           trackerKind,
			Endpoint:       endpoint,
			APIKey:         trackerAPIKey,
			ProjectSlug:    projectSlug,
			Assignee:       trackerAssignee,
			ActiveStates:   activeStates,
			TerminalStates: terminalStates,
		},
		Polling: PollingSettings{
			IntervalMs: pollInterval,
		},
		Workspace: WorkspaceSettings{
			Root: expandedRoot,
		},
		Worker: WorkerSettings{
			SSHHosts:                   sshHosts,
			MaxConcurrentAgentsPerHost: maxPerHost,
		},
		Agent: AgentSettings{
			MaxConcurrentAgents:        maxConcurrent,
			MaxTurns:                   maxTurns,
			MaxRetryBackoffMs:          maxRetryBackoff,
			MaxConcurrentAgentsByState: agentStateLimits,
		},
		Codex: CodexSettings{
			Command:           codexCommand,
			ApprovalPolicy:    approvalPolicy,
			ThreadSandbox:     threadSandbox,
			TurnSandboxPolicy: raw.Codex.TurnSandboxPolicy,
			TurnTimeoutMs:     turnTimeout,
			ReadTimeoutMs:     readTimeout,
			StallTimeoutMs:    stallTimeout,
		},
		Hooks: HookSettings{
			AfterCreate:  derefStr(raw.Hooks.AfterCreate),
			BeforeRun:    derefStr(raw.Hooks.BeforeRun),
			AfterRun:     derefStr(raw.Hooks.AfterRun),
			BeforeRemove: derefStr(raw.Hooks.BeforeRemove),
			TimeoutMs:    hookTimeout,
		},
		Observability: ObservabilitySettings{
			DashboardEnabled: dashboardEnabled,
			RefreshMs:        refreshMs,
			RenderIntervalMs: renderIntervalMs,
		},
		Server: ServerSettings{
			Port: serverPort,
			Host: serverHost,
		},
	}

	if err := s.Validate(); err != nil {
		return nil, err
	}
	return s, nil
}

// Validate checks required fields.
func (s *Settings) Validate() error {
	switch s.Tracker.Kind {
	case "linear", "memory":
		// ok
	case "":
		return fmt.Errorf("missing_tracker_kind")
	default:
		return fmt.Errorf("unsupported_tracker_kind: %s", s.Tracker.Kind)
	}

	if s.Tracker.Kind == "linear" && s.Tracker.APIKey == "" {
		return fmt.Errorf("missing_linear_api_token")
	}
	if s.Tracker.Kind == "linear" && s.Tracker.ProjectSlug == "" {
		return fmt.Errorf("missing_linear_project_slug")
	}
	if s.Agent.MaxConcurrentAgents == 0 {
		return fmt.Errorf("agent.max_concurrent_agents must be > 0")
	}
	if s.Agent.MaxTurns == 0 {
		return fmt.Errorf("agent.max_turns must be > 0")
	}
	if s.Codex.Command == "" {
		return fmt.Errorf("codex.command must be present and non-empty")
	}
	if s.Codex.TurnTimeoutMs == 0 {
		return fmt.Errorf("codex.turn_timeout_ms must be > 0")
	}
	if s.Codex.ReadTimeoutMs == 0 {
		return fmt.Errorf("codex.read_timeout_ms must be > 0")
	}
	return nil
}

// MaxConcurrentAgentsForState returns the per-state limit or the global limit.
func (s *Settings) MaxConcurrentAgentsForState(state string) int {
	if n, ok := s.Agent.MaxConcurrentAgentsByState[NormalizeIssueState(state)]; ok {
		return n
	}
	return s.Agent.MaxConcurrentAgents
}

// EffectiveLogsRoot returns the logs root from overrides or cwd.
func (s *Settings) EffectiveLogsRoot(overrides *CliOverrides) string {
	if overrides != nil && overrides.LogsRoot != "" {
		return overrides.LogsRoot
	}
	cwd, err := os.Getwd()
	if err != nil {
		return "."
	}
	return cwd
}

// DefaultTurnSandboxPolicy returns the sandbox policy for a workspace.
func (s *Settings) DefaultTurnSandboxPolicy(workspacePath string) any {
	if s.Codex.TurnSandboxPolicy != nil {
		return s.Codex.TurnSandboxPolicy
	}
	writableRoot := workspacePath
	if writableRoot == "" {
		writableRoot = s.Workspace.Root
	}
	return map[string]any{
		"type":          "workspaceWrite",
		"writableRoots": []string{writableRoot},
	}
}

// NormalizeIssueState lowercases and trims state strings.
func NormalizeIssueState(state string) string {
	return strings.ToLower(strings.TrimSpace(state))
}

// DefaultPromptTemplate returns the fallback prompt template.
func DefaultPromptTemplate() string {
	return strings.Join([]string{
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
	}, "\n")
}

// RenderPrompt renders the prompt template with issue and attempt data.
// Uses strict variable checking per §5.4: unknown variables fail rendering.
func RenderPrompt(template string, issueObj map[string]any, attempt *int) (string, error) {
	engine := liquid.NewEngine()
	engine.StrictVariables()
	bindings := map[string]any{
		"issue": issueObj,
	}
	if attempt != nil {
		bindings["attempt"] = *attempt
	}
	out, err := engine.ParseAndRenderString(template, bindings)
	if err != nil {
		errStr := err.Error()
		if strings.Contains(errStr, "parse") || strings.Contains(errStr, "syntax") {
			return "", fmt.Errorf("template_parse_error: %w", err)
		}
		return "", fmt.Errorf("template_render_error: %w", err)
	}
	return out, nil
}

// ToJSON marshals a value to JSON bytes.
func ToJSON(v any) ([]byte, error) {
	return json.Marshal(v)
}

// --- helpers ---

func defaultWorkspaceRoot() string {
	return filepath.Join(os.TempDir(), "symphony_workspaces")
}

func defaultApprovalPolicy() any {
	return map[string]any{
		"reject": map[string]any{
			"sandbox_approval":  true,
			"rules":             true,
			"mcp_elicitations":  true,
		},
	}
}

func resolveEnvString(value *string, canonicalEnv string) string {
	if value != nil {
		trimmed := strings.TrimSpace(*value)
		if strings.HasPrefix(trimmed, "$") {
			envName := trimmed[1:]
			v := os.Getenv(envName)
			if v != "" {
				return v
			}
			return ""
		}
		if trimmed == "" {
			v := os.Getenv(canonicalEnv)
			return v
		}
		return *value
	}
	if canonicalEnv != "" {
		return os.Getenv(canonicalEnv)
	}
	return ""
}

func expandPathLike(value string) (string, error) {
	if strings.HasPrefix(value, "$") {
		envName := value[1:]
		v := os.Getenv(envName)
		if v == "" {
			return "", fmt.Errorf("missing env var for path: %s", envName)
		}
		return v, nil
	}
	if value == "~" || strings.HasPrefix(value, "~/") {
		home := os.Getenv("HOME")
		if home == "" {
			return "", fmt.Errorf("HOME is not set")
		}
		if value == "~" {
			return home, nil
		}
		return filepath.Join(home, value[2:]), nil
	}
	return value, nil
}

func parsePositiveInt(v any) (int, error) {
	if v == nil {
		return 0, fmt.Errorf("nil")
	}
	switch val := v.(type) {
	case int:
		return val, nil
	case int64:
		return int(val), nil
	case float64:
		return int(val), nil
	case string:
		n, err := strconv.Atoi(strings.TrimSpace(val))
		if err != nil {
			return 0, err
		}
		return n, nil
	default:
		return 0, fmt.Errorf("expected integer, got %T", v)
	}
}

func parseInt64(v any) (int64, error) {
	if v == nil {
		return 0, fmt.Errorf("nil")
	}
	switch val := v.(type) {
	case int:
		return int64(val), nil
	case int64:
		return val, nil
	case float64:
		return int64(val), nil
	case string:
		n, err := strconv.ParseInt(strings.TrimSpace(val), 10, 64)
		if err != nil {
			return 0, err
		}
		return n, nil
	default:
		return 0, fmt.Errorf("expected integer, got %T", v)
	}
}

func derefStr(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}
