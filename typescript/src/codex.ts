// Codex app-server protocol client — §10 of the spec
import { spawn, type ChildProcess } from "node:child_process";
import type { AgentEvent, TokenUsage } from "./types.ts";
import type { CodexSettings } from "./config.ts";
import { logger } from "./logger.ts";

interface JsonRpcRequest {
  id?: number;
  method: string;
  params?: unknown;
}

interface JsonRpcResponse {
  id?: number;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
  method?: string;
  params?: unknown;
}

/** Extract token usage from an event payload. Handles various field shapes. */
function extractUsage(payload: Record<string, unknown>): TokenUsage | undefined {
  // Try direct usage map
  const usage = payload["usage"] as Record<string, unknown> | undefined;
  if (usage) {
    return {
      input_tokens: typeof usage["input_tokens"] === "number" ? usage["input_tokens"] : undefined,
      output_tokens: typeof usage["output_tokens"] === "number" ? usage["output_tokens"] : undefined,
      total_tokens: typeof usage["total_tokens"] === "number" ? usage["total_tokens"] : undefined,
    };
  }
  return undefined;
}

/** Extract absolute thread token totals from a thread/tokenUsage/updated event. §13.5 */
function extractThreadTokens(
  payload: Record<string, unknown>
): { input: number; output: number; total: number } | null {
  // thread/tokenUsage/updated shape
  const tu = payload["tokenUsage"] as Record<string, unknown> | undefined;
  if (tu) {
    const input = typeof tu["inputTokens"] === "number" ? tu["inputTokens"] : 0;
    const output = typeof tu["outputTokens"] === "number" ? tu["outputTokens"] : 0;
    const total = typeof tu["totalTokens"] === "number" ? tu["totalTokens"] : (input + output);
    return { input, output, total };
  }
  // total_token_usage wrapper
  const ttu = payload["total_token_usage"] as Record<string, unknown> | undefined;
  if (ttu) {
    const input = typeof ttu["input_tokens"] === "number" ? ttu["input_tokens"] : 0;
    const output = typeof ttu["output_tokens"] === "number" ? ttu["output_tokens"] : 0;
    const total = typeof ttu["total_tokens"] === "number" ? ttu["total_tokens"] : (input + output);
    return { input, output, total };
  }
  return null;
}

/** Detect if a payload is a rate-limit event. */
function extractRateLimits(msg: Record<string, unknown>): unknown | null {
  const method = msg["method"];
  if (
    typeof method === "string" &&
    (method.includes("rateLimit") || method.includes("rate_limit"))
  ) {
    return msg["params"] ?? msg["result"] ?? null;
  }
  const rateLimits = msg["rate_limits"] ?? msg["rateLimits"];
  if (rateLimits) return rateLimits;
  return null;
}

export interface AppServerSession {
  thread_id: string;
  pid: string | null;
}

export interface TurnResult {
  completed: boolean; // true = success, false = failure/cancel
  failure_reason?: string;
  usage?: TokenUsage;
}

type ToolCallHandler = (
  toolCallId: string,
  toolName: string,
  input: unknown
) => Promise<{ success: boolean; output?: unknown; error?: string }>;

export class CodexClient {
  private process: ChildProcess | null = null;
  private settings: CodexSettings;
  private workspacePath: string;
  private requestId = 1;
  private lineBuffer = "";
  private pendingResponses = new Map<
    number,
    { resolve: (r: JsonRpcResponse) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }
  >();
  private eventCallback: ((event: AgentEvent) => void) | null = null;
  private toolCallHandler: ToolCallHandler | null = null;
  private stderrBuffer = "";
  private currentTurnResolve: ((result: TurnResult) => void) | null = null;
  private currentTurnReject: ((err: Error) => void) | null = null;
  private currentTurnTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(settings: CodexSettings, workspacePath: string) {
    this.settings = settings;
    this.workspacePath = workspacePath;
  }

  onEvent(cb: (event: AgentEvent) => void): void {
    this.eventCallback = cb;
  }

  onToolCall(handler: ToolCallHandler): void {
    this.toolCallHandler = handler;
  }

  /** §10.1 Launch the app-server subprocess. */
  async launch(): Promise<void> {
    const command = `bash -lc ${JSON.stringify(this.settings.command)}`;

    this.process = spawn("bash", ["-lc", this.settings.command], {
      cwd: this.workspacePath,
      stdio: ["pipe", "pipe", "pipe"],
      env: { ...process.env },
    });

    const proc = this.process;

    proc.stderr?.on("data", (data: Buffer) => {
      this.stderrBuffer += data.toString();
      // Log stderr as diagnostics (not protocol)
      const lines = this.stderrBuffer.split("\n");
      this.stderrBuffer = lines.pop() ?? "";
      for (const line of lines) {
        if (line.trim()) logger.debug(`codex stderr: ${line}`);
      }
    });

    proc.stdout?.on("data", (data: Buffer) => {
      this.lineBuffer += data.toString();
      this.processBuffer();
    });

    proc.on("close", (code: number | null) => {
      logger.info(`codex process exited code=${code}`);
      // Reject any pending requests
      for (const [, pending] of this.pendingResponses) {
        clearTimeout(pending.timer);
        pending.reject(new Error(`Codex process exited with code ${code}`));
      }
      this.pendingResponses.clear();

      // Fail any in-progress turn
      if (this.currentTurnReject) {
        const rej = this.currentTurnReject;
        this.currentTurnResolve = null;
        this.currentTurnReject = null;
        if (this.currentTurnTimer) clearTimeout(this.currentTurnTimer);
        rej(new Error(`codex process exited with code ${code}`));
      }
    });

    proc.on("error", (err: Error) => {
      logger.error(`codex process error: ${err.message}`);
      for (const [, pending] of this.pendingResponses) {
        clearTimeout(pending.timer);
        pending.reject(err);
      }
      this.pendingResponses.clear();
    });
  }

  private processBuffer(): void {
    const lines = this.lineBuffer.split("\n");
    this.lineBuffer = lines.pop() ?? "";
    for (const line of lines) {
      if (!line.trim()) continue;
      this.handleLine(line);
    }
  }

  private handleLine(line: string): void {
    let msg: Record<string, unknown>;
    try {
      msg = JSON.parse(line) as Record<string, unknown>;
    } catch {
      this.emit({ kind: "malformed", raw: line });
      return;
    }

    // Dispatch based on message type

    // Response to a pending request
    const msgId = msg["id"];
    if (typeof msgId === "number" && this.pendingResponses.has(msgId)) {
      const pending = this.pendingResponses.get(msgId)!;
      clearTimeout(pending.timer);
      this.pendingResponses.delete(msgId);
      pending.resolve(msg as JsonRpcResponse);
      return;
    }

    const method = typeof msg["method"] === "string" ? msg["method"] : null;

    // Handle notifications and server-pushed messages
    this.dispatchServerMessage(method, msg);
  }

  private dispatchServerMessage(
    method: string | null,
    msg: Record<string, unknown>
  ): void {
    if (!method) {
      this.emit({ kind: "other_message", message: msg });
      return;
    }

    // Turn lifecycle
    if (method === "turn/completed" || method === "turn/complete") {
      const usage = extractUsage(msg["params"] as Record<string, unknown> ?? {});
      const rateLimits = extractRateLimits(msg);
      if (rateLimits) this.emit({ kind: "rate_limit_update", payload: rateLimits });
      this.resolveTurn({ completed: true, usage });
      this.emit({ kind: "turn_completed", usage, rate_limits: rateLimits });
      return;
    }

    if (method === "turn/failed") {
      const params = (msg["params"] as Record<string, unknown>) ?? {};
      const reason = String(params["reason"] ?? params["message"] ?? "turn failed");
      this.resolveTurn({ completed: false, failure_reason: reason });
      this.emit({ kind: "turn_failed", error: reason });
      return;
    }

    if (method === "turn/cancelled" || method === "turn/canceled") {
      this.resolveTurn({ completed: false, failure_reason: "cancelled" });
      this.emit({ kind: "turn_cancelled" });
      return;
    }

    // Thread token usage (absolute totals) — §13.5
    if (method === "thread/tokenUsage/updated" || method === "thread/token_usage/updated") {
      const params = (msg["params"] as Record<string, unknown>) ?? {};
      const tokens = extractThreadTokens(params);
      if (tokens) {
        this.emit({ kind: "token_update", thread_input: tokens.input, thread_output: tokens.output, thread_total: tokens.total });
      }
      return;
    }

    // Rate limits
    const rateLimits = extractRateLimits(msg);
    if (rateLimits) {
      this.emit({ kind: "rate_limit_update", payload: rateLimits });
      return;
    }

    // Approval requests — §10.5: auto-approve
    if (method === "item/approval/request" || method === "approval/request") {
      const params = (msg["params"] as Record<string, unknown>) ?? {};
      const approvalId =
        typeof params["id"] === "string" ? params["id"] :
        typeof msg["id"] === "string" ? msg["id"] :
        String(msg["id"] ?? "");
      this.emit({ kind: "approval_auto_approved" });
      // Send approval response
      this.sendRaw({ id: approvalId, result: { approved: true } });
      return;
    }

    // User input required — §10.5: treat as hard failure
    if (
      method === "item/tool/requestUserInput" ||
      method === "turn/userInputRequired" ||
      method === "user_input_required"
    ) {
      this.emit({ kind: "turn_input_required" });
      this.rejectTurn(new Error("turn_input_required"));
      return;
    }

    // Dynamic tool calls — §10.5
    if (method === "item/tool/call" || method === "tool/call") {
      const params = (msg["params"] as Record<string, unknown>) ?? {};
      const toolCallId =
        typeof params["id"] === "string" ? params["id"] :
        typeof msg["id"] === "string" ? msg["id"] :
        String(msg["id"] ?? "");
      const toolName = String(params["name"] ?? params["toolName"] ?? "");
      const input = params["input"] ?? params["arguments"] ?? {};

      this.handleToolCall(toolCallId, toolName, input);
      return;
    }

    // Notification / status update
    if (method.includes("notification") || method.includes("status") || method.includes("output")) {
      const params = (msg["params"] as Record<string, unknown>) ?? {};
      this.emit({ kind: "notification", message: params });
      return;
    }

    this.emit({ kind: "other_message", message: msg });
  }

  private async handleToolCall(
    toolCallId: string,
    toolName: string,
    input: unknown
  ): Promise<void> {
    if (this.toolCallHandler) {
      try {
        const result = await this.toolCallHandler(toolCallId, toolName, input);
        this.sendRaw({ id: toolCallId, result });
      } catch (err) {
        this.sendRaw({
          id: toolCallId,
          result: { success: false, error: String(err) },
        });
      }
    } else {
      // Unsupported tool — return failure and continue §10.5
      this.emit({ kind: "unsupported_tool_call", tool_name: toolName });
      this.sendRaw({
        id: toolCallId,
        result: { success: false, error: "unsupported_tool_call" },
      });
    }
  }

  private emit(event: AgentEvent): void {
    this.eventCallback?.(event);
  }

  private sendRaw(msg: unknown): void {
    if (!this.process?.stdin) return;
    const line = JSON.stringify(msg) + "\n";
    this.process.stdin.write(line);
  }

  private sendRequest(method: string, params?: unknown): Promise<JsonRpcResponse> {
    const id = this.requestId++;
    const req: JsonRpcRequest = { id, method };
    if (params !== undefined) req.params = params;

    return new Promise<JsonRpcResponse>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pendingResponses.delete(id);
        reject(new Error(`Request timeout: ${method} (${this.settings.read_timeout_ms}ms)`));
      }, this.settings.read_timeout_ms);

      this.pendingResponses.set(id, { resolve, reject, timer });
      this.sendRaw(req);
    });
  }

  private sendNotification(method: string, params?: unknown): void {
    const msg: JsonRpcRequest = { method };
    if (params !== undefined) msg.params = params;
    this.sendRaw(msg);
  }

  /** §10.2 Session startup handshake. Returns thread_id. */
  async startSession(): Promise<string> {
    // 1. initialize request
    await this.sendRequest("initialize", {
      clientInfo: { name: "symphony", version: "0.1.0" },
      capabilities: {},
    });

    // 2. initialized notification
    this.sendNotification("initialized", {});

    // 3. thread/start request
    const threadStartParams: Record<string, unknown> = {
      approvalPolicy: this.settings.approval_policy,
      sandbox: this.settings.thread_sandbox,
      cwd: this.workspacePath,
    };
    if (this.settings.turn_sandbox_policy) {
      threadStartParams["sandboxPolicy"] = this.settings.turn_sandbox_policy;
    }

    const threadResponse = await this.sendRequest("thread/start", threadStartParams);

    if (threadResponse.error) {
      throw new Error(`thread/start failed: ${threadResponse.error.message}`);
    }

    const result = threadResponse.result as Record<string, unknown> | undefined;
    const thread = result?.["thread"] as Record<string, unknown> | undefined;
    const threadId =
      typeof thread?.["id"] === "string" ? thread["id"] :
      typeof result?.["threadId"] === "string" ? result["threadId"] :
      typeof result?.["thread_id"] === "string" ? result["thread_id"] : null;

    if (!threadId) {
      throw new Error("thread/start did not return a thread_id");
    }

    this.emit({ kind: "session_started", pid: String(this.process?.pid ?? "") || null });
    return threadId;
  }

  /** §10.2/10.3 Start a turn and stream until completion. Returns turn_id. */
  async startTurn(
    threadId: string,
    prompt: string,
    issueTitle: string,
    issueIdentifier: string
  ): Promise<{ turn_id: string; result: TurnResult }> {
    const turnStartParams: Record<string, unknown> = {
      threadId,
      input: [{ type: "text", text: prompt }],
      cwd: this.workspacePath,
      title: `${issueIdentifier}: ${issueTitle}`,
      approvalPolicy: this.settings.approval_policy,
    };
    if (this.settings.turn_sandbox_policy) {
      turnStartParams["sandboxPolicy"] = this.settings.turn_sandbox_policy;
    }

    const turnResponse = await this.sendRequest("turn/start", turnStartParams);

    if (turnResponse.error) {
      throw new Error(`turn/start failed: ${turnResponse.error.message}`);
    }

    const result = turnResponse.result as Record<string, unknown> | undefined;
    const turn = result?.["turn"] as Record<string, unknown> | undefined;
    const turnId =
      typeof turn?.["id"] === "string" ? turn["id"] :
      typeof result?.["turnId"] === "string" ? result["turnId"] :
      typeof result?.["turn_id"] === "string" ? result["turn_id"] : "unknown";

    // Wait for turn to complete
    const turnResult = await this.awaitTurnCompletion();
    return { turn_id: turnId, result: turnResult };
  }

  private awaitTurnCompletion(): Promise<TurnResult> {
    return new Promise<TurnResult>((resolve, reject) => {
      this.currentTurnResolve = resolve;
      this.currentTurnReject = reject;

      this.currentTurnTimer = setTimeout(() => {
        this.currentTurnResolve = null;
        this.currentTurnReject = null;
        reject(new Error("turn_timeout"));
      }, this.settings.turn_timeout_ms);
    });
  }

  private resolveTurn(result: TurnResult): void {
    if (this.currentTurnResolve) {
      if (this.currentTurnTimer) clearTimeout(this.currentTurnTimer);
      const res = this.currentTurnResolve;
      this.currentTurnResolve = null;
      this.currentTurnReject = null;
      res(result);
    }
  }

  private rejectTurn(err: Error): void {
    if (this.currentTurnReject) {
      if (this.currentTurnTimer) clearTimeout(this.currentTurnTimer);
      const rej = this.currentTurnReject;
      this.currentTurnResolve = null;
      this.currentTurnReject = null;
      rej(err);
    }
  }

  getPid(): string | null {
    const pid = this.process?.pid;
    return pid !== undefined ? String(pid) : null;
  }

  /** Terminate the subprocess. */
  kill(): void {
    if (this.process) {
      try { this.process.kill("SIGTERM"); } catch {}
    }
    // Cleanup pending
    for (const [, pending] of this.pendingResponses) {
      clearTimeout(pending.timer);
    }
    this.pendingResponses.clear();
    if (this.currentTurnTimer) clearTimeout(this.currentTurnTimer);
    this.currentTurnResolve = null;
    this.currentTurnReject = null;
  }
}
