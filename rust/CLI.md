# `rsymphony` CLI

This document covers the command-line interface for the Rust Symphony binary,
`rsymphony`.

## Install

Build locally:

```bash
cargo build --release --bin rsymphony
```

Or install globally with Cargo:

```bash
cargo install --path . --bin rsymphony --force
```

## Command Shape

`rsymphony` has two modes:

1. Run the Symphony service
2. Validate config and prompt files without starting the service

Default paths:

- config: `./.symphony.toml`
- workflow prompt: `./WORKFLOW.md`

## Run Symphony

Basic form:

```bash
rsymphony \
  --i-understand-that-this-will-be-running-without-the-usual-guardrails \
  [--config /path/to/.symphony.toml] \
  [--logs-root /path/to/logs] \
  [--port 4000] \
  [WORKFLOW.md]
```

Example:

```bash
rsymphony \
  --i-understand-that-this-will-be-running-without-the-usual-guardrails \
  --config ./infra/.symphony.toml \
  --port 4000 \
  ./WORKFLOW.md
```

Notes:

- The long guardrail-acknowledgement flag is required for normal runs.
- `--yolo` is accepted as an alias for the acknowledgement flag.
- The positional path, when present, is the workflow prompt file only.
- Runtime config comes from `.symphony.toml`, not from `WORKFLOW.md`.
- If `.symphony.toml` is missing, Symphony currently falls back to legacy
  `WORKFLOW.md` front matter and emits a deprecation warning.

## Validate Config

Use `validate` to check config without starting the service:

```bash
rsymphony validate [--json] [WORKFLOW.md]
```

With a custom config file:

```bash
rsymphony --config /path/to/.symphony.toml validate ./WORKFLOW.md
```

Examples:

```bash
rsymphony validate
rsymphony validate ./WORKFLOW.md
rsymphony --config ./.symphony.toml validate --json ./WORKFLOW.md
```

Behavior:

- Validates `.symphony.toml` by default.
- If a workflow path is provided, also checks that the prompt file is readable.
- Exits non-zero when validation fails.
- Prints human-readable diagnostics by default.
- Prints structured JSON with `--json`.

## Flags

Global flags:

- `--config <path>`: path to the TOML config file. Defaults to `./.symphony.toml`.
- `--logs-root <path>`: override the directory used for log output.
- `--port <port>`: override `server.port` and enable the HTTP dashboard/API.

Run-only flags:

- `--i-understand-that-this-will-be-running-without-the-usual-guardrails`: required to start the service.
- `--yolo`: alias for the acknowledgement flag.

Validate-only flags:

- `--json`: emit machine-readable validation output.

## Files the CLI Uses

- [`.symphony.toml`](/Users/charliewilco/Developer/symphony/rust/.symphony.toml): runtime configuration
- [`WORKFLOW.md`](/Users/charliewilco/Developer/symphony/rust/WORKFLOW.md): Liquid/Markdown prompt template

Relevant behavior:

- `.symphony.toml` is parsed and validated as typed config.
- `WORKFLOW.md` is prompt text only in the primary path.
- Config and prompt are reloaded independently while the service is running.
- On reload failure, Symphony keeps the last known good config or prompt.

## Validation Output

Human-readable output looks like this:

```text
Configuration invalid in /repo/.symphony.toml (toml)
- Missing Linear API token. [tracker.api_key] (missing_linear_api_token)
  hint: Set `tracker.api_key` in `.symphony.toml` or export `LINEAR_API_KEY`.
```

JSON output looks like this:

```json
{
  "valid": false,
  "config_path": "/repo/.symphony.toml",
  "config_format": "toml",
  "warnings": [],
  "diagnostics": [
    {
      "code": "missing_linear_api_token",
      "message": "Missing Linear API token.",
      "file": "/repo/.symphony.toml",
      "field_path": "tracker.api_key",
      "hint": "Set `tracker.api_key` in `.symphony.toml` or export `LINEAR_API_KEY`."
    }
  ],
  "workflow_path": "/repo/WORKFLOW.md"
}
```

Diagnostics can include:

- error code
- human-readable message
- file path
- field path
- line and column when available
- hint text

## Exit Behavior

- `rsymphony ...run...` exits non-zero if startup validation fails.
- `rsymphony validate` exits zero when config is valid.
- `rsymphony validate` exits non-zero when config is invalid.

## Common Workflows

Validate before a real run:

```bash
rsymphony validate ./WORKFLOW.md
rsymphony \
  --i-understand-that-this-will-be-running-without-the-usual-guardrails \
  ./WORKFLOW.md
```

Run with a custom config and dashboard:

```bash
rsymphony \
  --i-understand-that-this-will-be-running-without-the-usual-guardrails \
  --config ./ops/.symphony.toml \
  --port 4000 \
  ./WORKFLOW.md
```

Validate in CI:

```bash
rsymphony --config ./.symphony.toml validate --json ./WORKFLOW.md
```
