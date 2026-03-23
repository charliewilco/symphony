// Configuration layer — §5.3, §6 of the spec
import * as os from "node:os";
import * as path from "node:path";
import type { WorkflowDefinition } from "./workflow.ts";

export interface Settings {
  tracker: TrackerSettings;
  polling: PollingSettings;
  workspace: WorkspaceSettings;
  agent: AgentSettings;
  codex: CodexSettings;
  hooks: HookSettings;
  server: ServerSettings;
}

export interface TrackerSettings {
  kind: string | null;
  endpoint: string;
  api_key: string | null;
  project_slug: string | null;
  active_states: string[];
  terminal_states: string[];
  assignee: string | null;
}

export interface PollingSettings {
  interval_ms: number;
}

export interface WorkspaceSettings {
  root: string;
}

export interface AgentSettings {
  max_concurrent_agents: number;
  max_turns: number;
  max_retry_backoff_ms: number;
  max_concurrent_agents_by_state: Map<string, number>;
}

export interface CodexSettings {
  command: string;
  approval_policy: unknown;
  thread_sandbox: string;
  turn_sandbox_policy: unknown;
  turn_timeout_ms: number;
  read_timeout_ms: number;
  stall_timeout_ms: number;
}

export interface HookSettings {
  after_create: string | null;
  before_run: string | null;
  after_run: string | null;
  before_remove: string | null;
  timeout_ms: number;
}

export interface ServerSettings {
  port: number | null;
  host: string;
}

export interface CliOverrides {
  port?: number;
  logs_root?: string;
}

// §6.1 Default values
const DEFAULTS = {
  tracker_endpoint_linear: "https://api.linear.app/graphql",
  active_states: ["Todo", "In Progress"],
  terminal_states: ["Closed", "Cancelled", "Canceled", "Duplicate", "Done"],
  poll_interval_ms: 30_000,
  workspace_root: () => path.join(os.tmpdir(), "symphony_workspaces"),
  max_concurrent_agents: 10,
  max_turns: 20,
  max_retry_backoff_ms: 300_000,
  codex_command: "codex app-server",
  codex_turn_timeout_ms: 3_600_000,
  codex_read_timeout_ms: 5_000,
  codex_stall_timeout_ms: 300_000,
  hooks_timeout_ms: 60_000,
  server_host: "127.0.0.1",
} as const;

/** Resolve a value that may be an environment variable reference ($VAR_NAME). */
function resolveEnvRef(value: string | null | undefined): string | null {
  if (!value) return null;
  if (value.startsWith("$")) {
    const varName = value.slice(1);
    return process.env[varName] ?? null;
  }
  return value;
}

/** Expand ~ and $VAR path prefixes for filesystem paths. */
function expandPath(value: string | null | undefined): string | null {
  if (!value) return null;

  // Bare strings without path separators are preserved as-is
  if (!value.includes("/") && !value.includes("\\") && !value.startsWith("~") && !value.startsWith("$")) {
    return value;
  }

  if (value.startsWith("$")) {
    const resolved = resolveEnvRef(value);
    return resolved ? expandPath(resolved) : null;
  }

  if (value.startsWith("~")) {
    return path.join(os.homedir(), value.slice(1));
  }

  return path.resolve(value);
}

function toInt(val: unknown, fallback: number): number {
  if (val === null || val === undefined) return fallback;
  const n = typeof val === "string" ? parseInt(val, 10) : Number(val);
  return Number.isFinite(n) ? n : fallback;
}

function toPositiveInt(val: unknown, fallback: number): number {
  const n = toInt(val, fallback);
  return n > 0 ? n : fallback;
}

function toStringArray(val: unknown, fallback: string[]): string[] {
  if (!Array.isArray(val)) return fallback;
  return val.filter((s) => typeof s === "string");
}

/** Build a Settings object from a workflow definition and CLI overrides. §6 */
export function settingsFromWorkflow(
  workflow: WorkflowDefinition,
  overrides?: CliOverrides
): Settings {
  const cfg = workflow.config;

  // §5.3.1 tracker
  const trackerRaw = (cfg["tracker"] as Record<string, unknown>) ?? {};
  const kind = typeof trackerRaw["kind"] === "string" ? trackerRaw["kind"] : null;
  const isLinear = kind === "linear";

  const rawApiKey = trackerRaw["api_key"] as string | undefined;
  const apiKey = rawApiKey
    ? resolveEnvRef(rawApiKey)
    : isLinear
    ? (process.env["LINEAR_API_KEY"] ?? null)
    : null;

  const tracker: TrackerSettings = {
    kind,
    endpoint:
      typeof trackerRaw["endpoint"] === "string"
        ? trackerRaw["endpoint"]
        : DEFAULTS.tracker_endpoint_linear,
    api_key: apiKey,
    project_slug:
      typeof trackerRaw["project_slug"] === "string"
        ? trackerRaw["project_slug"]
        : null,
    assignee:
      typeof trackerRaw["assignee"] === "string" ? trackerRaw["assignee"] : null,
    active_states: toStringArray(
      trackerRaw["active_states"],
      [...DEFAULTS.active_states]
    ),
    terminal_states: toStringArray(
      trackerRaw["terminal_states"],
      [...DEFAULTS.terminal_states]
    ),
  };

  // §5.3.2 polling
  const pollingRaw = (cfg["polling"] as Record<string, unknown>) ?? {};
  const polling: PollingSettings = {
    interval_ms: toPositiveInt(pollingRaw["interval_ms"], DEFAULTS.poll_interval_ms),
  };

  // §5.3.3 workspace
  const workspaceRaw = (cfg["workspace"] as Record<string, unknown>) ?? {};
  const rawRoot = workspaceRaw["root"] as string | undefined;
  const resolvedRoot = rawRoot
    ? expandPath(rawRoot) ?? DEFAULTS.workspace_root()
    : DEFAULTS.workspace_root();
  const workspace: WorkspaceSettings = { root: resolvedRoot };

  // §5.3.5 agent
  const agentRaw = (cfg["agent"] as Record<string, unknown>) ?? {};
  const byStateRaw = (agentRaw["max_concurrent_agents_by_state"] as Record<string, unknown>) ?? {};
  const byState = new Map<string, number>();
  for (const [k, v] of Object.entries(byStateRaw)) {
    const n = toPositiveInt(v, 0);
    if (n > 0) byState.set(k.toLowerCase(), n);
  }
  const agent: AgentSettings = {
    max_concurrent_agents: toPositiveInt(agentRaw["max_concurrent_agents"], DEFAULTS.max_concurrent_agents),
    max_turns: toPositiveInt(agentRaw["max_turns"], DEFAULTS.max_turns),
    max_retry_backoff_ms: toPositiveInt(agentRaw["max_retry_backoff_ms"], DEFAULTS.max_retry_backoff_ms),
    max_concurrent_agents_by_state: byState,
  };

  // §5.3.6 codex
  const codexRaw = (cfg["codex"] as Record<string, unknown>) ?? {};
  const codex: CodexSettings = {
    command:
      typeof codexRaw["command"] === "string" && codexRaw["command"]
        ? codexRaw["command"]
        : DEFAULTS.codex_command,
    approval_policy: codexRaw["approval_policy"] ?? "on-failure",
    thread_sandbox: typeof codexRaw["thread_sandbox"] === "string" ? codexRaw["thread_sandbox"] : "none",
    turn_sandbox_policy: codexRaw["turn_sandbox_policy"] ?? null,
    turn_timeout_ms: toPositiveInt(codexRaw["turn_timeout_ms"], DEFAULTS.codex_turn_timeout_ms),
    read_timeout_ms: toPositiveInt(codexRaw["read_timeout_ms"], DEFAULTS.codex_read_timeout_ms),
    stall_timeout_ms: toInt(codexRaw["stall_timeout_ms"] as unknown, DEFAULTS.codex_stall_timeout_ms),
  };

  // §5.3.4 hooks
  const hooksRaw = (cfg["hooks"] as Record<string, unknown>) ?? {};
  const hookTimeoutRaw = toPositiveInt(hooksRaw["timeout_ms"], 0);
  const hooks: HookSettings = {
    after_create:
      typeof hooksRaw["after_create"] === "string" ? hooksRaw["after_create"] : null,
    before_run:
      typeof hooksRaw["before_run"] === "string" ? hooksRaw["before_run"] : null,
    after_run:
      typeof hooksRaw["after_run"] === "string" ? hooksRaw["after_run"] : null,
    before_remove:
      typeof hooksRaw["before_remove"] === "string" ? hooksRaw["before_remove"] : null,
    timeout_ms: hookTimeoutRaw > 0 ? hookTimeoutRaw : DEFAULTS.hooks_timeout_ms,
  };

  // §13.7 server extension
  const serverRaw = (cfg["server"] as Record<string, unknown>) ?? {};
  const configPort =
    typeof serverRaw["port"] === "number" ? serverRaw["port"] :
    typeof serverRaw["port"] === "string" ? parseInt(serverRaw["port"], 10) : null;
  const effectivePort =
    overrides?.port !== undefined ? overrides.port :
    configPort !== null && Number.isFinite(configPort) ? configPort : null;

  const server: ServerSettings = {
    port: effectivePort,
    host: DEFAULTS.server_host,
  };

  return { tracker, polling, workspace, agent, codex, hooks, server };
}

/** Validate config for dispatch — §6.3 */
export function validateForDispatch(settings: Settings): string | null {
  if (!settings.tracker.kind) {
    return "tracker.kind is required";
  }
  if (!["linear"].includes(settings.tracker.kind)) {
    return `tracker.kind "${settings.tracker.kind}" is not supported`;
  }
  if (!settings.tracker.api_key) {
    return "tracker.api_key is missing (set LINEAR_API_KEY or tracker.api_key in WORKFLOW.md)";
  }
  if (!settings.tracker.project_slug) {
    return "tracker.project_slug is required for tracker.kind=linear";
  }
  if (!settings.codex.command) {
    return "codex.command is required";
  }
  return null; // valid
}
