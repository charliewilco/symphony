# Symphony — TypeScript/Bun

A TypeScript implementation of the [Symphony specification](../SPEC.md), built with [Bun](https://bun.sh).

## Requirements

- [Bun](https://bun.sh) v1.0+
- `codex` CLI installed and authenticated (or configured via `codex.command`)
- A Linear API key with access to your project

## Quick Start

```bash
cd typescript
bun install

# Configure WORKFLOW.md with your project settings
cp WORKFLOW.md.example WORKFLOW.md  # edit as needed
export LINEAR_API_KEY=lin_api_...

# Run (requires explicit acknowledgement of no-guardrails posture)
bun run src/main.ts --yolo [path/to/WORKFLOW.md]
```

## CLI Options

```
Usage: symphony [options] [workflow-path]

Options:
  --yolo / --i-understand-that-this-will-be-running-without-the-usual-guardrails
                    Required: acknowledge high-trust operation mode
  --port <n>        Override HTTP server port (also enables the server)
  --logs-root <dir> Directory for log files (default: ./logs)
  [workflow-path]   Path to WORKFLOW.md (default: ./WORKFLOW.md)
```

## Architecture

```
src/
  main.ts          CLI entry point, startup, signal handling
  workflow.ts      WORKFLOW.md parser (YAML front matter + Liquid template)
  workflow_store.ts  Hot-reload watcher for WORKFLOW.md
  config.ts        Typed settings with defaults and env resolution
  tracker.ts       Linear GraphQL client (paginated queries)
  workspace.ts     Per-issue workspace lifecycle (create/reuse/hooks/cleanup)
  prompt.ts        Liquid template rendering (strict variables/filters)
  codex.ts         Codex app-server JSON-RPC protocol client
  agent_runner.ts  Agent runner (workspace + prompt + multi-turn codex session)
  orchestrator.ts  State machine: polling, dispatch, concurrency, retries
  http.ts          Optional HTTP server: dashboard + /api/v1/* REST API
  dashboard.ts     Terminal status dashboard (ANSI)
  logger.ts        Structured JSON logger (stderr + optional file sink)
```

## Spec Conformance

- §5: Workflow file loading, YAML front matter, Liquid template rendering
- §6: Config layer with defaults, env resolution, hot-reload, dispatch validation
- §7: Orchestrator state machine (Unclaimed → Claimed → Running → RetryQueued → Released)
- §8: Poll loop, candidate selection, concurrency control, retry/backoff, reconciliation
- §9: Workspace lifecycle, hooks, safety invariants (path containment)
- §10: Codex app-server JSON-RPC protocol, multi-turn loop, auto-approval, linear_graphql tool
- §11: Linear GraphQL adapter (paginated queries, normalization, error handling)
- §12: Prompt construction and Liquid template rendering
- §13: Structured logging, terminal dashboard, optional HTTP server

## HTTP Dashboard

When `--port` is provided or `server.port` is set in `WORKFLOW.md`:

- `GET /` — Human-readable HTML dashboard (auto-refreshes every 5s)
- `GET /api/v1/state` — Full JSON snapshot
- `GET /api/v1/<issue-identifier>` — Per-issue debug info
- `POST /api/v1/refresh` — Trigger immediate poll cycle

## Tests

```bash
bun test
```

## Trust Posture

This implementation targets **high-trust environments**:
- Codex sessions run with `approval_policy: on-failure` by default
- Auto-approves all command/file-change approval requests
- `turn_input_required` is treated as a hard failure (not stalled indefinitely)
- No operator confirmation is required for individual agent actions

Configure `codex.approval_policy`, `codex.thread_sandbox`, and `codex.turn_sandbox_policy` in `WORKFLOW.md` to adjust.
