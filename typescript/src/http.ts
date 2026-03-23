// Optional HTTP server — §13.7 of the spec
import type { Orchestrator } from "./orchestrator.ts";
import type { Settings } from "./config.ts";

function jsonResponse(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data, null, 2), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function errorResponse(code: string, message: string, status: number): Response {
  return jsonResponse({ error: { code, message } }, status);
}

function methodNotAllowed(): Response {
  return errorResponse("method_not_allowed", "Method not allowed", 405);
}

/** Simple HTML dashboard page. */
function dashboardHtml(): Response {
  const html = `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Symphony Dashboard</title>
  <style>
    body { font-family: monospace; background: #0d1117; color: #c9d1d9; margin: 0; padding: 20px; }
    h1 { color: #58a6ff; }
    h2 { color: #8b949e; border-bottom: 1px solid #30363d; padding-bottom: 4px; }
    table { border-collapse: collapse; width: 100%; }
    th { color: #8b949e; text-align: left; padding: 6px 12px; border-bottom: 1px solid #30363d; }
    td { padding: 6px 12px; border-bottom: 1px solid #21262d; }
    .badge-running { color: #3fb950; }
    .badge-retrying { color: #d29922; }
    .badge-state { color: #58a6ff; }
    #state { display: none; }
    .meta { color: #8b949e; font-size: 0.85em; }
    .totals { display: flex; gap: 32px; flex-wrap: wrap; margin: 12px 0; }
    .total-item { }
    .total-label { color: #8b949e; font-size: 0.8em; }
    .total-value { font-size: 1.2em; color: #e6edf3; }
    a { color: #58a6ff; text-decoration: none; }
    a:hover { text-decoration: underline; }
  </style>
</head>
<body>
  <h1>&#9835; Symphony</h1>
  <div id="status" class="meta">Loading...</div>
  <div id="app"></div>
  <script>
    async function fetchState() {
      const res = await fetch('/api/v1/state');
      return res.json();
    }

    function renderTokens(t) {
      if (!t) return '';
      return \`In: \${t.input_tokens.toLocaleString()} / Out: \${t.output_tokens.toLocaleString()} / Total: \${t.total_tokens.toLocaleString()}\`;
    }

    function timeSince(iso) {
      if (!iso) return '—';
      const diff = Date.now() - new Date(iso).getTime();
      const s = Math.floor(diff / 1000);
      if (s < 60) return s + 's ago';
      if (s < 3600) return Math.floor(s/60) + 'm ago';
      return Math.floor(s/3600) + 'h ago';
    }

    function render(state) {
      const el = document.getElementById('app');
      const running = state.running || [];
      const retrying = state.retrying || [];
      const totals = state.codex_totals || {};

      let html = \`
        <div class="totals">
          <div class="total-item">
            <div class="total-label">RUNNING</div>
            <div class="total-value badge-running">\${state.counts?.running ?? 0}</div>
          </div>
          <div class="total-item">
            <div class="total-label">RETRYING</div>
            <div class="total-value badge-retrying">\${state.counts?.retrying ?? 0}</div>
          </div>
          <div class="total-item">
            <div class="total-label">TOKENS</div>
            <div class="total-value">\${(totals.total_tokens || 0).toLocaleString()}</div>
          </div>
          <div class="total-item">
            <div class="total-label">RUNTIME</div>
            <div class="total-value">\${Math.round(totals.seconds_running || 0)}s</div>
          </div>
          <div class="total-item">
            <div class="total-label">POLL INTERVAL</div>
            <div class="total-value">\${state.polling?.poll_interval_ms ?? '—'}ms</div>
          </div>
        </div>
      \`;

      if (running.length > 0) {
        html += \`<h2>Running Sessions</h2>
        <table>
          <thead><tr><th>Issue</th><th>State</th><th>Turns</th><th>Tokens</th><th>Last Event</th><th>Started</th></tr></thead>
          <tbody>\`;
        for (const r of running) {
          html += \`<tr>
            <td><a href="/api/v1/\${r.issue_identifier}">\${r.issue_identifier}</a></td>
            <td class="badge-state">\${r.state}</td>
            <td>\${r.turn_count}</td>
            <td>\${renderTokens(r.tokens)}</td>
            <td>\${r.last_event || '—'}</td>
            <td>\${timeSince(r.started_at)}</td>
          </tr>\`;
        }
        html += \`</tbody></table>\`;
      }

      if (retrying.length > 0) {
        html += \`<h2>Retry Queue</h2>
        <table>
          <thead><tr><th>Issue</th><th>Attempt</th><th>Retry In</th><th>Error</th></tr></thead>
          <tbody>\`;
        for (const r of retrying) {
          const dueIn = Math.max(0, Math.round(r.due_in_ms / 1000));
          html += \`<tr>
            <td>\${r.issue_identifier}</td>
            <td>\${r.attempt}</td>
            <td>\${dueIn}s</td>
            <td>\${r.error || '—'}</td>
          </tr>\`;
        }
        html += \`</tbody></table>\`;
      }

      if (running.length === 0 && retrying.length === 0) {
        html += \`<p class="meta">No active sessions.</p>\`;
      }

      html += \`<p class="meta">Generated: \${state.generated_at}</p>\`;
      el.innerHTML = html;
    }

    async function refresh() {
      try {
        const state = await fetchState();
        render(state);
        document.getElementById('status').textContent = 'Auto-refreshing every 5s';
      } catch(e) {
        document.getElementById('status').textContent = 'Error: ' + e;
      }
    }

    refresh();
    setInterval(refresh, 5000);
  </script>
</body>
</html>`;
  return new Response(html, {
    status: 200,
    headers: { "Content-Type": "text/html; charset=utf-8" },
  });
}

/** Start the HTTP server. §13.7 */
export function startHttpServer(
  orchestrator: Orchestrator,
  settings: Settings
): { port: number; stop: () => void } {
  const { port, host } = settings.server;
  if (!port && port !== 0) throw new Error("No server port configured");

  let refreshQueued = false;

  const server = Bun.serve({
    port: port ?? 0,
    hostname: host,
    fetch(req: Request) {
      const url = new URL(req.url);
      const pathname = url.pathname;

      // §13.7.1 Dashboard
      if (pathname === "/" || pathname === "") {
        if (req.method !== "GET") return methodNotAllowed();
        return dashboardHtml();
      }

      // §13.7.2 JSON API
      if (pathname === "/api/v1/state") {
        if (req.method !== "GET") return methodNotAllowed();
        return jsonResponse(orchestrator.snapshot());
      }

      if (pathname === "/api/v1/refresh") {
        if (req.method !== "POST") return methodNotAllowed();
        const coalesced = refreshQueued;
        if (!refreshQueued) {
          refreshQueued = true;
          orchestrator.triggerRefresh().finally(() => { refreshQueued = false; });
        }
        return jsonResponse({
          queued: true,
          coalesced,
          requested_at: new Date().toISOString(),
          operations: ["poll", "reconcile"],
        }, 202);
      }

      // /api/v1/<issue_identifier>
      const issueMatch = pathname.match(/^\/api\/v1\/([^/]+)$/);
      if (issueMatch) {
        if (req.method !== "GET") return methodNotAllowed();
        const issueIdentifier = decodeURIComponent(issueMatch[1]);
        const snap = orchestrator.snapshot();
        const running = snap.running.find((r) => r.issue_identifier === issueIdentifier);
        const retrying = snap.retrying.find((r) => r.issue_identifier === issueIdentifier);

        if (!running && !retrying) {
          return errorResponse("issue_not_found", `Issue "${issueIdentifier}" not found in current state`, 404);
        }

        const status = running ? "running" : "retrying";
        return jsonResponse({
          issue_identifier: issueIdentifier,
          issue_id: running?.issue_id ?? retrying?.issue_id,
          status,
          workspace: running ? { path: running.workspace_path } : null,
          running: running
            ? {
                session_id: running.session_id,
                turn_count: running.turn_count,
                state: running.state,
                started_at: running.started_at,
                last_event: running.last_event,
                last_message: running.last_message,
                last_event_at: running.last_event_at,
                tokens: running.tokens,
              }
            : null,
          retry: retrying ?? null,
          last_error: retrying?.error ?? null,
        });
      }

      return errorResponse("not_found", "Not found", 404);
    },
  });

  return {
    port: server.port ?? (port ?? 0),
    stop: () => server.stop(),
  };
}
