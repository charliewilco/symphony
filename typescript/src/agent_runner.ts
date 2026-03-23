// Agent Runner — §10.7 of the spec
// Wraps workspace creation, prompt building, and app-server client.
import type { Issue, AgentEvent, LiveSession } from "./types.ts";
import type { Settings } from "./config.ts";
import type { WorkflowDefinition } from "./workflow.ts";
import type { LinearTracker } from "./tracker.ts";
import { ensureWorkspace, runBeforeRun, runAfterRun } from "./workspace.ts";
import { buildPrompt, buildContinuationPrompt } from "./prompt.ts";
import { CodexClient } from "./codex.ts";
import { logger } from "./logger.ts";

export interface AgentRunnerCallbacks {
  onEvent: (event: AgentEvent) => void;
  onSessionUpdate: (partial: Partial<LiveSession>) => void;
  signal: AbortSignal;
}

export interface AgentRunResult {
  success: boolean;
  error?: string;
  ended_normally: boolean; // true if no crash/timeout — orchestrator should schedule short retry
}

/** §10.5 linear_graphql tool handler */
function makeLinearToolHandler(tracker: LinearTracker | null) {
  return async (
    _toolCallId: string,
    toolName: string,
    input: unknown
  ): Promise<{ success: boolean; output?: unknown; error?: string }> => {
    if (toolName === "linear_graphql") {
      if (!tracker) {
        return { success: false, error: "linear_graphql: tracker not configured" };
      }
      const inp = input as Record<string, unknown> | null | undefined;
      const query = inp?.["query"];
      if (typeof query !== "string" || !query.trim()) {
        return { success: false, error: "linear_graphql: query must be a non-empty string" };
      }
      const variables = (inp?.["variables"] as Record<string, unknown> | undefined) ?? {};
      const result = await tracker.executeRawGraphQL(query, variables);
      return result.success
        ? { success: true, output: result.data }
        : { success: false, output: result.data, error: String(result.errors?.[0] ?? "GraphQL error") };
    }
    // Unsupported tool — return failure, session continues §10.5
    return { success: false, error: "unsupported_tool_call" };
  };
}

/**
 * Run a full agent session for one issue.
 * Handles workspace, prompt, multi-turn loop, and cleanup.
 */
export async function runAgent(
  issue: Issue,
  attempt: number | null,
  workflow: WorkflowDefinition,
  settings: Settings,
  tracker: LinearTracker | null,
  callbacks: AgentRunnerCallbacks
): Promise<AgentRunResult> {
  const { onEvent, onSessionUpdate, signal } = callbacks;
  const { workspace: wsSettings, hooks, codex: codexSettings, agent: agentSettings } = settings;

  // Stage: PreparingWorkspace
  let workspace;
  try {
    workspace = await ensureWorkspace(issue.identifier, wsSettings, hooks);
  } catch (err) {
    const error = String(err);
    logger.error(`workspace preparation failed issue_id=${issue.id} issue_identifier=${issue.identifier} error=${error}`);
    onEvent({ kind: "startup_failed", error });
    return { success: false, error, ended_normally: false };
  }

  // Stage: before_run hook
  try {
    await runBeforeRun(workspace.path, hooks);
  } catch (err) {
    const error = String(err);
    logger.error(`before_run hook failed issue_id=${issue.id} issue_identifier=${issue.identifier} error=${error}`);
    await runAfterRun(workspace.path, hooks);
    return { success: false, error, ended_normally: false };
  }

  // §9.5 Invariant 1: cwd must equal workspace_path
  const workspacePath = workspace.path;

  // Stage: BuildingPrompt
  let firstPrompt: string;
  try {
    firstPrompt = await buildPrompt(workflow.prompt_template, issue, attempt);
  } catch (err) {
    const error = String(err);
    logger.error(`prompt build failed issue_id=${issue.id} issue_identifier=${issue.identifier} error=${error}`);
    await runAfterRun(workspacePath, hooks);
    return { success: false, error, ended_normally: false };
  }

  // Stage: LaunchingAgentProcess
  const client = new CodexClient(codexSettings, workspacePath);
  client.onEvent(onEvent);
  client.onToolCall(makeLinearToolHandler(tracker));

  // Forward token/rate updates to orchestrator
  client.onEvent((event) => {
    if (event.kind === "token_update") {
      onSessionUpdate({
        codex_input_tokens: event.thread_input,
        codex_output_tokens: event.thread_output,
        codex_total_tokens: event.thread_total,
      });
    }
    if (event.kind === "rate_limit_update") {
      // handled by orchestrator via onEvent
    }
    const ts = new Date();
    onSessionUpdate({
      last_codex_event: event.kind,
      last_codex_timestamp: ts,
      last_codex_message: event,
    });
  });

  // Handle abort
  signal.addEventListener("abort", () => {
    client.kill();
  });

  try {
    await client.launch();
  } catch (err) {
    const error = `Failed to launch codex: ${err}`;
    logger.error(`${error} issue_id=${issue.id} issue_identifier=${issue.identifier}`);
    onEvent({ kind: "startup_failed", error });
    await runAfterRun(workspacePath, hooks);
    return { success: false, error, ended_normally: false };
  }

  // Stage: InitializingSession
  let threadId: string;
  try {
    threadId = await client.startSession();
    onSessionUpdate({
      codex_app_server_pid: client.getPid(),
      thread_id: threadId,
    });
  } catch (err) {
    const error = String(err);
    logger.error(`session init failed issue_id=${issue.id} issue_identifier=${issue.identifier} error=${error}`);
    client.kill();
    onEvent({ kind: "startup_failed", error });
    await runAfterRun(workspacePath, hooks);
    return { success: false, error, ended_normally: false };
  }

  let totalTurns = 0;
  let lastTurnSuccess = false;
  let lastError: string | undefined;

  // Multi-turn loop — §7.1
  const maxTurns = agentSettings.max_turns;

  for (let turnIndex = 0; turnIndex < maxTurns; turnIndex++) {
    if (signal.aborted) {
      lastError = "CanceledByReconciliation";
      break;
    }

    const isFirstTurn = turnIndex === 0;
    const prompt = isFirstTurn
      ? firstPrompt
      : buildContinuationPrompt(issue, turnIndex + 1);

    logger.info(`turn starting issue_id=${issue.id} issue_identifier=${issue.identifier} turn=${turnIndex + 1}`);

    let turnResult;
    try {
      const { turn_id, result } = await client.startTurn(
        threadId,
        prompt,
        issue.title,
        issue.identifier
      );
      onSessionUpdate({
        turn_id,
        session_id: `${threadId}-${turn_id}`,
        turn_count: turnIndex + 1,
      });
      turnResult = result;
    } catch (err) {
      const error = String(err);
      logger.error(`turn failed issue_id=${issue.id} issue_identifier=${issue.identifier} turn=${turnIndex + 1} error=${error}`);
      lastError = error;
      lastTurnSuccess = false;
      break;
    }

    totalTurns++;

    if (!turnResult.completed) {
      lastError = turnResult.failure_reason ?? "turn failed";
      lastTurnSuccess = false;
      logger.warn(`turn not completed issue_id=${issue.id} issue_identifier=${issue.identifier} reason=${lastError}`);
      break;
    }

    lastTurnSuccess = true;
    logger.info(`turn completed issue_id=${issue.id} issue_identifier=${issue.identifier} turn=${turnIndex + 1}`);

    // After each successful turn, check if issue is still active
    // (This would require fetching issue state — we'll signal "ended_normally" and let orchestrator decide)
    // The orchestrator re-checks on the short continuation retry
    // For simplicity, we continue turns while the signal is not aborted
    // The orchestrator will stop us via abort if the issue becomes terminal
    if (signal.aborted) break;
  }

  client.kill();
  await runAfterRun(workspacePath, hooks);

  if (lastTurnSuccess) {
    logger.info(`agent run succeeded issue_id=${issue.id} issue_identifier=${issue.identifier} turns=${totalTurns}`);
    return { success: true, ended_normally: true };
  } else {
    logger.warn(`agent run failed issue_id=${issue.id} issue_identifier=${issue.identifier} error=${lastError}`);
    return {
      success: false,
      error: lastError ?? "unknown error",
      ended_normally: signal.aborted, // if aborted by reconciliation, it ended normally from our perspective
    };
  }
}
