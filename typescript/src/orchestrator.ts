// Orchestrator — §7, §8 of the spec
// Single authoritative in-memory state for dispatch, retries, and reconciliation.
import * as nodePath from "node:path";
import { sanitizeWorkspaceKey } from "./workspace.ts";
import type {
  Issue,
  OrchestratorState,
  RunningEntry,
  RetryEntry,
  AgentEvent,
  LiveSession,
  Snapshot,
  RunningSnapshot,
  RetrySnapshot,
} from "./types.ts";
import type { Settings } from "./config.ts";
import type { WorkflowDefinition } from "./workflow.ts";
import { settingsFromWorkflow, validateForDispatch } from "./config.ts";
import { WorkflowStore } from "./workflow_store.ts";
import { createTracker, type LinearTracker } from "./tracker.ts";
import { removeWorkspace } from "./workspace.ts";
import { runAgent } from "./agent_runner.ts";
import { logger } from "./logger.ts";

// §8.4 Backoff formula
const CONTINUATION_RETRY_DELAY_MS = 1_000;

function calcBackoffMs(attempt: number, maxMs: number): number {
  // delay = min(10000 * 2^(attempt-1), max)
  const delay = 10_000 * Math.pow(2, attempt - 1);
  return Math.min(delay, maxMs);
}

// §4.2 Sorting — priority ascending (null last), then created_at oldest first, then identifier
function sortIssues(issues: Issue[]): Issue[] {
  return [...issues].sort((a, b) => {
    // priority: lower is better; null sorts last
    const pa = a.priority ?? 9999;
    const pb = b.priority ?? 9999;
    if (pa !== pb) return pa - pb;

    // created_at: oldest first
    const ta = a.created_at?.getTime() ?? Infinity;
    const tb = b.created_at?.getTime() ?? Infinity;
    if (ta !== tb) return ta - tb;

    // identifier: lexicographic
    return a.identifier.localeCompare(b.identifier);
  });
}

// §8.2 Blocker check for Todo state
function isTodoBlocked(issue: Issue, terminalStates: string[]): boolean {
  if (issue.state.toLowerCase() !== "todo") return false;
  for (const blocker of issue.blocked_by) {
    const bState = (blocker.state ?? "").toLowerCase();
    if (!terminalStates.map((s) => s.toLowerCase()).includes(bState)) {
      return true; // non-terminal blocker
    }
  }
  return false;
}

export class Orchestrator {
  private state: OrchestratorState;
  private workflowStore: WorkflowStore;
  private cliOverrides?: { port?: number; logs_root?: string };

  private pollTimer: ReturnType<typeof setTimeout> | null = null;
  private nextPollInMs: number | null = null;
  private snapshotListeners: Array<() => void> = [];

  constructor(
    workflowStore: WorkflowStore,
    overrides?: { port?: number; logs_root?: string }
  ) {
    this.workflowStore = workflowStore;
    this.cliOverrides = overrides;

    const settings = settingsFromWorkflow(workflowStore.getCurrent(), overrides);
    this.state = {
      poll_interval_ms: settings.polling.interval_ms,
      max_concurrent_agents: settings.agent.max_concurrent_agents,
      running: new Map(),
      claimed: new Set(),
      retry_attempts: new Map(),
      completed: new Set(),
      codex_totals: { input_tokens: 0, output_tokens: 0, total_tokens: 0, seconds_running: 0 },
      codex_rate_limits: null,
      ended_session_seconds: 0,
    };

    // React to workflow reloads
    workflowStore.onReload((def) => {
      const newSettings = settingsFromWorkflow(def, this.cliOverrides);
      this.state.poll_interval_ms = newSettings.polling.interval_ms;
      this.state.max_concurrent_agents = newSettings.agent.max_concurrent_agents;
      logger.info(
        `config reloaded poll_interval_ms=${newSettings.polling.interval_ms} max_agents=${newSettings.agent.max_concurrent_agents}`
      );
    });
  }

  private getSettings(): Settings {
    return settingsFromWorkflow(this.workflowStore.getCurrent(), this.cliOverrides);
  }

  private getTracker(settings: Settings): LinearTracker | null {
    try {
      return createTracker(settings.tracker);
    } catch {
      return null;
    }
  }

  /** §8.6 Startup terminal cleanup — removes stale workspaces for terminal issues. */
  async startupCleanup(): Promise<void> {
    const settings = this.getSettings();
    const tracker = this.getTracker(settings);
    if (!tracker) {
      logger.warn("startup cleanup skipped: tracker unavailable");
      return;
    }

    let terminalIssues: Array<{ id: string; identifier: string }>;
    try {
      terminalIssues = await tracker.fetchIssuesByStates(settings.tracker.terminal_states);
    } catch (err) {
      logger.warn(`startup terminal cleanup fetch failed error=${err}`);
      return;
    }

    for (const { identifier } of terminalIssues) {
      try {
        await removeWorkspace(identifier, settings.workspace, settings.hooks);
      } catch (err) {
        logger.warn(`startup workspace cleanup failed identifier=${identifier} error=${err}`);
      }
    }
    logger.info(`startup cleanup completed cleaned=${terminalIssues.length}`);
  }

  /** Start the polling loop. */
  async start(): Promise<void> {
    const settings = this.getSettings();
    const validationError = validateForDispatch(settings);
    if (validationError) {
      throw new Error(`Startup validation failed: ${validationError}`);
    }

    await this.startupCleanup();

    // Immediate first tick
    await this.tick();
    this.scheduleNextPoll();
  }

  private scheduleNextPoll(): void {
    if (this.pollTimer) clearTimeout(this.pollTimer);
    const intervalMs = this.state.poll_interval_ms;
    this.nextPollInMs = intervalMs;
    this.pollTimer = setTimeout(async () => {
      this.nextPollInMs = null;
      await this.tick();
      this.scheduleNextPoll();
    }, intervalMs);
  }

  stop(): void {
    if (this.pollTimer) {
      clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
    // Cancel all running workers
    for (const [, entry] of this.state.running) {
      entry.worker_abort.abort();
    }
    // Cancel all retry timers
    for (const [, retry] of this.state.retry_attempts) {
      clearTimeout(retry.timer_handle);
    }
  }

  /** §8.1 One poll tick. */
  private async tick(): Promise<void> {
    logger.debug("poll tick starting");

    // 1. Reconcile running issues
    await this.reconcile();

    // 2. Validate config
    const settings = this.getSettings();
    const validationError = validateForDispatch(settings);
    if (validationError) {
      logger.error(`dispatch preflight validation failed: ${validationError}`);
      return; // skip dispatch, reconciliation already ran
    }

    // 3. Fetch candidate issues
    const tracker = this.getTracker(settings);
    if (!tracker) {
      logger.error("dispatch skipped: tracker unavailable");
      return;
    }

    let candidates: Issue[];
    try {
      candidates = await tracker.fetchCandidateIssues(settings.tracker.active_states);
    } catch (err) {
      logger.error(`candidate fetch failed error=${err}`);
      return;
    }

    // 4. Sort
    const sorted = sortIssues(candidates);

    // 5. Dispatch eligible issues
    await this.dispatch(sorted, settings, tracker);

    // 6. Notify listeners
    this.notifyListeners();
  }

  /** §8.5 Reconciliation — stall detection + tracker state refresh. */
  private async reconcile(): Promise<void> {
    const settings = this.getSettings();
    const now = Date.now();

    // Part A: Stall detection
    if (settings.codex.stall_timeout_ms > 0) {
      for (const [issueId, entry] of this.state.running) {
        const lastActivity = entry.session?.last_codex_timestamp?.getTime() ?? entry.started_at.getTime();
        const elapsed = now - lastActivity;
        if (elapsed > settings.codex.stall_timeout_ms) {
          logger.warn(
            `stall detected issue_id=${issueId} issue_identifier=${entry.issue.identifier} elapsed_ms=${elapsed}`
          );
          this.terminateWorker(issueId, "stall");
          this.scheduleRetry(issueId, entry.issue.identifier, entry.attempt, "stall timeout");
        }
      }
    }

    // Part B: Tracker state refresh
    const runningIds = [...this.state.running.keys()];
    if (runningIds.length === 0) return;

    const tracker = this.getTracker(settings);
    if (!tracker) return;

    let stateMap: Map<string, { identifier: string; state: string }>;
    try {
      stateMap = await tracker.fetchIssueStatesByIds(runningIds);
    } catch (err) {
      logger.warn(`reconciliation state refresh failed error=${err}`);
      return; // keep workers running
    }

    for (const [issueId, entry] of this.state.running) {
      const current = stateMap.get(issueId);
      if (!current) {
        // Issue not found — terminate without workspace cleanup
        logger.warn(`issue not found during reconciliation issue_id=${issueId}`);
        this.terminateWorker(issueId, "cancel");
        this.releaseClaim(issueId);
        continue;
      }

      const stateLower = current.state.toLowerCase();
      const isTerminal = settings.tracker.terminal_states.map((s) => s.toLowerCase()).includes(stateLower);
      const isActive = settings.tracker.active_states.map((s) => s.toLowerCase()).includes(stateLower);

      if (isTerminal) {
        logger.info(
          `issue terminal during run issue_id=${issueId} issue_identifier=${current.identifier} state=${current.state}`
        );
        this.terminateWorker(issueId, "cancel");
        this.releaseClaim(issueId);
        // Clean workspace for terminal issue
        removeWorkspace(current.identifier, settings.workspace, settings.hooks).catch(() => {});
      } else if (isActive) {
        // Update snapshot
        const runEntry = this.state.running.get(issueId);
        if (runEntry) {
          runEntry.issue = { ...runEntry.issue, state: current.state };
        }
      } else {
        // Not active, not terminal — terminate without workspace cleanup
        logger.info(
          `issue no longer active issue_id=${issueId} issue_identifier=${current.identifier} state=${current.state}`
        );
        this.terminateWorker(issueId, "cancel");
        this.releaseClaim(issueId);
      }
    }
  }

  /** §8.2 Dispatch eligible issues. */
  private async dispatch(
    candidates: Issue[],
    settings: Settings,
    tracker: LinearTracker
  ): Promise<void> {
    for (const issue of candidates) {
      if (!this.canDispatch(issue, settings)) continue;

      logger.info(
        `dispatching issue_id=${issue.id} issue_identifier=${issue.identifier} state=${issue.state}`
      );

      this.claimIssue(issue.id);
      await this.launchWorker(issue, null, settings, tracker);
    }
  }

  /** §8.2 Eligibility check. */
  private canDispatch(issue: Issue, settings: Settings): boolean {
    // Must have required fields
    if (!issue.id || !issue.identifier || !issue.title || !issue.state) return false;

    const stateLower = issue.state.toLowerCase();
    const isActive = settings.tracker.active_states.map((s) => s.toLowerCase()).includes(stateLower);
    const isTerminal = settings.tracker.terminal_states.map((s) => s.toLowerCase()).includes(stateLower);

    if (!isActive || isTerminal) return false;
    if (this.state.running.has(issue.id)) return false;
    if (this.state.claimed.has(issue.id)) return false;

    // Global concurrency
    const availableSlots = Math.max(settings.agent.max_concurrent_agents - this.state.running.size, 0);
    if (availableSlots <= 0) return false;

    // Per-state concurrency
    const stateKey = stateLower;
    const perStateLimit = settings.agent.max_concurrent_agents_by_state.get(stateKey);
    if (perStateLimit !== undefined) {
      const runningInState = [...this.state.running.values()].filter(
        (e) => e.issue.state.toLowerCase() === stateKey
      ).length;
      if (runningInState >= perStateLimit) return false;
    }

    // Blocker check for Todo
    if (isTodoBlocked(issue, settings.tracker.terminal_states)) return false;

    return true;
  }

  private claimIssue(issueId: string): void {
    this.state.claimed.add(issueId);
  }

  private releaseClaim(issueId: string): void {
    this.state.claimed.delete(issueId);
    this.state.running.delete(issueId);
    this.state.retry_attempts.delete(issueId);
  }

  private terminateWorker(issueId: string, reason: "cancel" | "stall"): void {
    const entry = this.state.running.get(issueId);
    if (!entry) return;

    const elapsed = (Date.now() - entry.started_at.getTime()) / 1000;
    this.state.ended_session_seconds += elapsed;
    entry.worker_abort.abort(reason);
    this.state.running.delete(issueId);
  }

  /** Launch a worker for an issue. */
  private async launchWorker(
    issue: Issue,
    attempt: number | null,
    settings: Settings,
    tracker: LinearTracker
  ): Promise<void> {
    const abort = new AbortController();
    const workspacePath = nodePath.join(
      settings.workspace.root,
      sanitizeWorkspaceKey(issue.identifier)
    );

    const entry: RunningEntry = {
      issue,
      attempt,
      workspace_path: workspacePath,
      started_at: new Date(),
      session: null,
      worker_abort: abort,
    };
    this.state.running.set(issue.id, entry);

    // Run worker in background
    const workflow = this.workflowStore.getCurrent();

    runAgent(issue, attempt, workflow, settings, tracker, {
      onEvent: (event: AgentEvent) => this.handleAgentEvent(issue.id, event),
      onSessionUpdate: (partial: Partial<LiveSession>) => {
        const runEntry = this.state.running.get(issue.id);
        if (runEntry) {
          runEntry.session = { ...(runEntry.session ?? this.defaultSession()), ...partial };
        }
      },
      signal: abort.signal,
    })
      .then((result) => this.handleWorkerExit(issue, attempt, result))
      .catch((err) => {
        logger.error(`worker threw unexpected error issue_id=${issue.id} error=${err}`);
        this.handleWorkerExit(issue, attempt, {
          success: false,
          error: String(err),
          ended_normally: false,
        });
      });
  }

  private defaultSession(): LiveSession {
    return {
      session_id: "",
      thread_id: "",
      turn_id: "",
      codex_app_server_pid: null,
      last_codex_event: null,
      last_codex_timestamp: null,
      last_codex_message: null,
      codex_input_tokens: 0,
      codex_output_tokens: 0,
      codex_total_tokens: 0,
      last_reported_input_tokens: 0,
      last_reported_output_tokens: 0,
      last_reported_total_tokens: 0,
      turn_count: 0,
    };
  }

  private handleAgentEvent(issueId: string, event: AgentEvent): void {
    const entry = this.state.running.get(issueId);
    if (!entry) return;

    if (event.kind === "token_update") {
      // §13.5 Track absolute thread totals using delta approach
      const session = entry.session ?? this.defaultSession();
      const prevInput = session.last_reported_input_tokens;
      const prevOutput = session.last_reported_output_tokens;
      const prevTotal = session.last_reported_total_tokens;

      const deltaInput = Math.max(0, event.thread_input - prevInput);
      const deltaOutput = Math.max(0, event.thread_output - prevOutput);
      const deltaTotal = Math.max(0, event.thread_total - prevTotal);

      this.state.codex_totals.input_tokens += deltaInput;
      this.state.codex_totals.output_tokens += deltaOutput;
      this.state.codex_totals.total_tokens += deltaTotal;

      if (entry.session) {
        entry.session.last_reported_input_tokens = event.thread_input;
        entry.session.last_reported_output_tokens = event.thread_output;
        entry.session.last_reported_total_tokens = event.thread_total;
      }
    }

    if (event.kind === "rate_limit_update") {
      this.state.codex_rate_limits = event.payload;
    }

    this.notifyListeners();
  }

  private handleWorkerExit(
    issue: Issue,
    attempt: number | null,
    result: { success: boolean; error?: string; ended_normally: boolean }
  ): void {
    const entry = this.state.running.get(issue.id);
    if (entry) {
      const elapsed = (Date.now() - entry.started_at.getTime()) / 1000;
      this.state.ended_session_seconds += elapsed;
      this.state.running.delete(issue.id);
    }

    if (result.success || result.ended_normally) {
      // Normal exit — schedule short continuation retry §7.3
      this.state.completed.add(issue.id);
      logger.info(
        `worker exited normally issue_id=${issue.id} issue_identifier=${issue.identifier} scheduling_continuation=true`
      );
      this.scheduleRetryAt(issue.id, issue.identifier, 1, CONTINUATION_RETRY_DELAY_MS, null);
    } else {
      // Abnormal exit — exponential backoff
      const settings = this.getSettings();
      const nextAttempt = (attempt ?? 0) + 1;
      const delayMs = calcBackoffMs(nextAttempt, settings.agent.max_retry_backoff_ms);
      logger.warn(
        `worker exited abnormally issue_id=${issue.id} issue_identifier=${issue.identifier} attempt=${nextAttempt} retry_in_ms=${delayMs} error=${result.error}`
      );
      this.scheduleRetryAt(issue.id, issue.identifier, nextAttempt, delayMs, result.error ?? null);
    }

    this.notifyListeners();
  }

  private scheduleRetry(
    issueId: string,
    identifier: string,
    attempt: number | null,
    error: string | null
  ): void {
    const settings = this.getSettings();
    const nextAttempt = (attempt ?? 0) + 1;
    const delayMs = calcBackoffMs(nextAttempt, settings.agent.max_retry_backoff_ms);
    this.scheduleRetryAt(issueId, identifier, nextAttempt, delayMs, error);
  }

  private scheduleRetryAt(
    issueId: string,
    identifier: string,
    attempt: number,
    delayMs: number,
    error: string | null
  ): void {
    // Cancel any existing retry timer
    const existing = this.state.retry_attempts.get(issueId);
    if (existing) clearTimeout(existing.timer_handle);

    const due_at_ms = Date.now() + delayMs;
    const timer_handle = setTimeout(() => {
      this.state.retry_attempts.delete(issueId);
      this.handleRetryFired(issueId, identifier, attempt).catch((err) => {
        logger.error(`retry handler error issue_id=${issueId} error=${err}`);
      });
    }, delayMs);

    this.state.retry_attempts.set(issueId, {
      issue_id: issueId,
      identifier,
      attempt,
      due_at_ms,
      timer_handle,
      error,
    });
  }

  /** §8.4 Retry timer fired. */
  private async handleRetryFired(
    issueId: string,
    identifier: string,
    attempt: number
  ): Promise<void> {
    const settings = this.getSettings();
    const tracker = this.getTracker(settings);

    if (!tracker) {
      logger.warn(`retry fired but tracker unavailable issue_id=${issueId}`);
      this.releaseClaim(issueId);
      return;
    }

    let candidates: Issue[];
    try {
      candidates = await tracker.fetchCandidateIssues(settings.tracker.active_states);
    } catch (err) {
      logger.error(`retry fetch failed issue_id=${issueId} error=${err}`);
      this.releaseClaim(issueId);
      return;
    }

    const issue = candidates.find((c) => c.id === issueId);

    if (!issue) {
      // Not found in active candidates — release claim
      logger.info(`retry: issue not active, releasing issue_id=${issueId}`);
      this.releaseClaim(issueId);
      return;
    }

    if (!this.canDispatch(issue, settings)) {
      // No slots available — requeue
      logger.info(`retry: no slots available, requeueing issue_id=${issueId}`);
      this.scheduleRetryAt(issueId, identifier, attempt, 5_000, "no available orchestrator slots");
      return;
    }

    logger.info(`retry firing issue_id=${issueId} issue_identifier=${identifier} attempt=${attempt}`);
    await this.launchWorker(issue, attempt, settings, tracker);
  }

  onSnapshot(listener: () => void): void {
    this.snapshotListeners.push(listener);
  }

  private notifyListeners(): void {
    for (const listener of this.snapshotListeners) {
      try { listener(); } catch {}
    }
  }

  /** §13.3 Build a runtime snapshot for observability. */
  snapshot(): Snapshot {
    const now = new Date().toISOString();
    const state = this.state;

    // Calculate live seconds from active sessions
    const liveSeconds = [...state.running.values()].reduce((sum, entry) => {
      return sum + (Date.now() - entry.started_at.getTime()) / 1000;
    }, 0);

    const running: RunningSnapshot[] = [...state.running.entries()].map(([, entry]) => ({
      issue_id: entry.issue.id,
      issue_identifier: entry.issue.identifier,
      state: entry.issue.state,
      session_id: entry.session?.session_id ?? null,
      turn_count: entry.session?.turn_count ?? 0,
      last_event: entry.session?.last_codex_event ?? null,
      last_message: JSON.stringify(entry.session?.last_codex_message ?? ""),
      started_at: entry.started_at.toISOString(),
      last_event_at: entry.session?.last_codex_timestamp?.toISOString() ?? null,
      tokens: {
        input_tokens: entry.session?.codex_input_tokens ?? 0,
        output_tokens: entry.session?.codex_output_tokens ?? 0,
        total_tokens: entry.session?.codex_total_tokens ?? 0,
      },
      codex_app_server_pid: entry.session?.codex_app_server_pid ?? null,
      workspace_path: entry.workspace_path,
    }));

    const retrying: RetrySnapshot[] = [...state.retry_attempts.values()].map((retry) => ({
      issue_id: retry.issue_id,
      issue_identifier: retry.identifier,
      attempt: retry.attempt,
      due_at: new Date(retry.due_at_ms).toISOString(),
      due_in_ms: Math.max(0, retry.due_at_ms - Date.now()),
      error: retry.error,
    }));

    return {
      generated_at: now,
      counts: { running: running.length, retrying: retrying.length },
      running,
      retrying,
      codex_totals: {
        input_tokens: state.codex_totals.input_tokens,
        output_tokens: state.codex_totals.output_tokens,
        total_tokens: state.codex_totals.total_tokens,
        seconds_running: Math.round((state.ended_session_seconds + liveSeconds) * 10) / 10,
      },
      rate_limits: state.codex_rate_limits,
      polling: {
        poll_interval_ms: state.poll_interval_ms,
        next_poll_in_ms: this.nextPollInMs,
      },
    };
  }

  /** Trigger an immediate poll (for /api/v1/refresh). */
  async triggerRefresh(): Promise<void> {
    if (this.pollTimer) {
      clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
    this.nextPollInMs = null;
    await this.tick();
    this.scheduleNextPoll();
  }
}
