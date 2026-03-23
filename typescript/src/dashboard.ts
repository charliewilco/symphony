// Terminal status dashboard — §13.4 of the spec
import type { Snapshot } from "./types.ts";
import type { Settings } from "./config.ts";

const RESET = "\x1b[0m";
const BOLD = "\x1b[1m";
const DIM = "\x1b[2m";
const RED = "\x1b[31m";
const GREEN = "\x1b[32m";
const YELLOW = "\x1b[33m";
const BLUE = "\x1b[34m";
const CYAN = "\x1b[36m";
const MAGENTA = "\x1b[35m";

function noColor(): boolean {
  return !!process.env["NO_COLOR"];
}

function c(text: string, ...codes: string[]): string {
  if (noColor()) return text;
  return codes.join("") + text + RESET;
}

function pad(s: string, width: number): string {
  if (s.length >= width) return s.slice(0, width);
  return s + " ".repeat(width - s.length);
}

function fmtDuration(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${s % 60}s`;
  const h = Math.floor(m / 60);
  return `${h}h${m % 60}m`;
}

function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function fmtTps(tps: number): string {
  if (tps <= 0) return "0";
  if (tps >= 1000) return `${(tps / 1000).toFixed(1)}k`;
  return tps.toFixed(1);
}

/** Build the terminal dashboard content string. */
export function formatDashboard(
  snapshot: Snapshot | null,
  settings: Settings,
  tps: number,
  columns: number
): string {
  const lines: string[] = [];
  const w = Math.max(60, columns);
  const boxW = w - 2;

  const top = "╭─ SYMPHONY STATUS " + "─".repeat(Math.max(0, boxW - 18)) + "╮";
  const bottom = "╰" + "─".repeat(boxW) + "╯";
  const mid = (content: string) => `│ ${pad(content, boxW - 2)} │`;
  const divider = (label: string) =>
    `├─ ${label} ` + "─".repeat(Math.max(0, boxW - label.length - 3)) + "┤";

  lines.push(c(top, BOLD));

  if (!snapshot) {
    lines.push(c(mid("Orchestrator snapshot unavailable"), RED, BOLD));
    lines.push(bottom);
    return lines.join("\n");
  }

  const { counts, running, retrying, codex_totals, polling, rate_limits } = snapshot;

  // Summary line
  const agentLine = `Agents: ${counts.running} running, ${counts.retrying} retrying`;
  lines.push(c(mid(agentLine), BOLD, GREEN));

  const throughputLine = `Throughput: ${fmtTps(tps)} tok/s`;
  lines.push(c(mid(throughputLine), BOLD, CYAN));

  const runtimeLine = `Runtime: ${Math.round(codex_totals.seconds_running)}s total`;
  lines.push(c(mid(runtimeLine), BOLD, MAGENTA));

  const tokensLine = `Tokens: ${fmtTokens(codex_totals.total_tokens)} (in:${fmtTokens(codex_totals.input_tokens)} out:${fmtTokens(codex_totals.output_tokens)})`;
  lines.push(c(mid(tokensLine), BOLD, YELLOW));

  const pollLine = `Next poll: ${polling.next_poll_in_ms !== null ? fmtDuration(polling.next_poll_in_ms) : "—"} (interval: ${fmtDuration(polling.poll_interval_ms)})`;
  lines.push(c(mid(pollLine), DIM));

  if (rate_limits) {
    const rlStr = JSON.stringify(rate_limits);
    lines.push(c(mid(`Rate Limits: ${rlStr.slice(0, Math.min(rlStr.length, boxW - 16))}`), BOLD, BLUE));
  }

  if (settings.server.port) {
    lines.push(c(mid(`Dashboard: http://${settings.server.host}:${settings.server.port}`), BOLD, CYAN));
  }

  // Running sessions
  if (running.length > 0) {
    lines.push(c(divider(`Running (${running.length})`), BOLD));

    const ID_W = 10;
    const STATE_W = 14;
    const TURN_W = 6;
    const TOK_W = 10;
    const AGE_W = 10;
    const EVENT_W = Math.max(0, boxW - ID_W - STATE_W - TURN_W - TOK_W - AGE_W - 12);

    const header = pad("ID", ID_W) + " " +
      pad("STATE", STATE_W) + " " +
      pad("TURN", TURN_W) + " " +
      pad("TOKENS", TOK_W) + " " +
      pad("AGE", AGE_W) + " " +
      pad("LAST EVENT", EVENT_W);
    lines.push(c(mid("  " + header), DIM));

    for (const r of running) {
      const age = fmtDuration(Date.now() - new Date(r.started_at).getTime());
      const tokens = fmtTokens(r.tokens.total_tokens);
      const event = r.last_event ?? "—";

      const row =
        c(pad(r.issue_identifier, ID_W), CYAN) + " " +
        c(pad(r.state, STATE_W), stateColor(r.state)) + " " +
        c(pad(String(r.turn_count), TURN_W), YELLOW) + " " +
        c(pad(tokens, TOK_W), BLUE) + " " +
        c(pad(age, AGE_W), MAGENTA) + " " +
        c(pad(event, EVENT_W), eventColor(event));

      lines.push(`│ ● ${pad(row, boxW - 4)} │`);
    }
  }

  // Retry queue
  if (retrying.length > 0) {
    lines.push(c(divider(`Backoff queue (${retrying.length})`), BOLD));
    for (const r of retrying) {
      const dueIn = fmtDuration(Math.max(0, r.due_in_ms));
      const errShort = r.error ? r.error.replace(/\n/g, " ").slice(0, 40) : "";
      lines.push(c(mid(`  ↻ ${r.issue_identifier} (attempt ${r.attempt}) in ${dueIn}${errShort ? " — " + errShort : ""}`), YELLOW));
    }
  }

  lines.push(c(bottom, DIM));
  return lines.join("\n");
}

function stateColor(state: string): string {
  const s = state.toLowerCase();
  if (s === "in progress") return GREEN;
  if (s === "done" || s === "completed") return BLUE;
  if (s === "rework") return YELLOW;
  if (s === "todo") return BLUE;
  return CYAN;
}

function eventColor(event: string): string {
  const e = event.toLowerCase();
  if (e.includes("failed") || e.includes("error")) return RED;
  if (e.includes("completed")) return GREEN;
  if (e.includes("token")) return YELLOW;
  return CYAN;
}

/** Rolling token-per-second calculation. */
export class TpsTracker {
  private samples: Array<[ms: number, tokens: number]> = [];
  private readonly windowMs: number;

  constructor(windowMs = 5_000) {
    this.windowMs = windowMs;
  }

  update(nowMs: number, totalTokens: number): number {
    const cutoff = nowMs - this.windowMs;
    this.samples = this.samples.filter(([ms]) => ms >= cutoff);
    this.samples.push([nowMs, totalTokens]);

    if (this.samples.length < 2) return 0;

    const [earliestMs, earliestTok] = this.samples[0];
    const elapsed = nowMs - earliestMs;
    if (elapsed === 0) return 0;
    const delta = totalTokens - earliestTok;
    return (delta / elapsed) * 1000;
  }
}

/** Start the terminal dashboard render loop. */
export function startDashboard(
  getSnapshot: () => Snapshot | null,
  getSettings: () => Settings
): { stop: () => void } {
  if (!process.stdout.isTTY) {
    return { stop: () => {} };
  }

  const tpsTracker = new TpsTracker();
  let running = true;
  let lastContent: string | null = null;

  const columns = () => {
    const c = parseInt(process.env["COLUMNS"] ?? "", 10);
    return c >= 80 ? c : process.stdout.columns ?? 115;
  };

  function tick(): void {
    if (!running) return;

    const settings = getSettings();
    const snapshot = getSnapshot();
    const nowMs = Date.now();
    const totalTokens = snapshot?.codex_totals.total_tokens ?? 0;
    const tps = tpsTracker.update(nowMs, totalTokens);

    const content = formatDashboard(snapshot, settings, tps, columns());

    if (content !== lastContent) {
      process.stdout.write("\x1b[2J\x1b[H");
      process.stdout.write(content + "\n");
      lastContent = content;
    }
  }

  const timer = setInterval(tick, 1_000);
  tick();

  return {
    stop: () => {
      running = false;
      clearInterval(timer);
    },
  };
}
