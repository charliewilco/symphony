// Symphony domain model types — §4 of the spec

// §4.1.1 Issue
export interface BlockerRef {
  id: string | null;
  identifier: string | null;
  state: string | null;
}

export interface Issue {
  id: string;
  identifier: string;
  title: string;
  description: string | null;
  priority: number | null;
  state: string;
  branch_name: string | null;
  url: string | null;
  labels: string[];
  blocked_by: BlockerRef[];
  created_at: Date | null;
  updated_at: Date | null;
}

// §4.1.2 Workflow Definition
export interface WorkflowDefinition {
  config: Record<string, unknown>;
  prompt_template: string;
}

// §4.1.4 Workspace
export interface Workspace {
  path: string;
  workspace_key: string;
  created_now: boolean;
}

// §4.1.5 Run Attempt lifecycle phases
export type RunAttemptStatus =
  | "PreparingWorkspace"
  | "BuildingPrompt"
  | "LaunchingAgentProcess"
  | "InitializingSession"
  | "StreamingTurn"
  | "Finishing"
  | "Succeeded"
  | "Failed"
  | "TimedOut"
  | "Stalled"
  | "CanceledByReconciliation";

export interface RunAttempt {
  issue_id: string;
  issue_identifier: string;
  attempt: number | null;
  workspace_path: string;
  started_at: Date;
  status: RunAttemptStatus;
  error?: string;
}

// §4.1.6 Live Session
export interface LiveSession {
  session_id: string;
  thread_id: string;
  turn_id: string;
  codex_app_server_pid: string | null;
  last_codex_event: string | null;
  last_codex_timestamp: Date | null;
  last_codex_message: unknown;
  codex_input_tokens: number;
  codex_output_tokens: number;
  codex_total_tokens: number;
  last_reported_input_tokens: number;
  last_reported_output_tokens: number;
  last_reported_total_tokens: number;
  turn_count: number;
}

// §4.1.7 Retry Entry
export interface RetryEntry {
  issue_id: string;
  identifier: string;
  attempt: number;
  due_at_ms: number;
  timer_handle: ReturnType<typeof setTimeout>;
  error: string | null;
}

// Running entry in the orchestrator
export interface RunningEntry {
  issue: Issue;
  attempt: number | null;
  workspace_path: string;
  started_at: Date;
  session: LiveSession | null;
  worker_abort: AbortController;
}

// §4.1.8 Orchestrator Runtime State
export interface OrchestratorState {
  poll_interval_ms: number;
  max_concurrent_agents: number;
  running: Map<string, RunningEntry>; // issue_id -> entry
  claimed: Set<string>; // issue_ids
  retry_attempts: Map<string, RetryEntry>; // issue_id -> entry
  completed: Set<string>; // issue_ids (bookkeeping)
  codex_totals: TokenTotals;
  codex_rate_limits: unknown;
  ended_session_seconds: number; // accumulated seconds from ended sessions
}

export interface TokenTotals {
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  seconds_running: number;
}

// Events emitted by the agent runner back to the orchestrator
export type AgentEvent =
  | { kind: "session_started"; pid: string | null }
  | { kind: "startup_failed"; error: string }
  | { kind: "turn_completed"; usage?: TokenUsage; rate_limits?: unknown; message?: unknown }
  | { kind: "turn_failed"; error: string; usage?: TokenUsage }
  | { kind: "turn_cancelled"; usage?: TokenUsage }
  | { kind: "turn_ended_with_error"; error: string }
  | { kind: "turn_input_required" }
  | { kind: "approval_auto_approved" }
  | { kind: "unsupported_tool_call"; tool_name: string }
  | { kind: "notification"; message?: unknown }
  | { kind: "other_message"; message?: unknown }
  | { kind: "malformed"; raw: string }
  | { kind: "token_update"; thread_input: number; thread_output: number; thread_total: number }
  | { kind: "rate_limit_update"; payload: unknown };

export interface TokenUsage {
  input_tokens?: number;
  output_tokens?: number;
  total_tokens?: number;
}

// Snapshot types for observability / HTTP API (§13)
export interface RunningSnapshot {
  issue_id: string;
  issue_identifier: string;
  state: string;
  session_id: string | null;
  turn_count: number;
  last_event: string | null;
  last_message: string;
  started_at: string;
  last_event_at: string | null;
  tokens: {
    input_tokens: number;
    output_tokens: number;
    total_tokens: number;
  };
  codex_app_server_pid: string | null;
  workspace_path: string;
}

export interface RetrySnapshot {
  issue_id: string;
  issue_identifier: string;
  attempt: number;
  due_at: string;
  due_in_ms: number;
  error: string | null;
}

export interface Snapshot {
  generated_at: string;
  counts: { running: number; retrying: number };
  running: RunningSnapshot[];
  retrying: RetrySnapshot[];
  codex_totals: {
    input_tokens: number;
    output_tokens: number;
    total_tokens: number;
    seconds_running: number;
  };
  rate_limits: unknown;
  polling: {
    poll_interval_ms: number;
    next_poll_in_ms: number | null;
  };
}
