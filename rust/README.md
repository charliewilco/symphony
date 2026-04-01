# Symphony Rust

This directory contains the current Rust implementation of Symphony, based on
[`SPEC.md`](../SPEC.md) at the repository root and the Elixir reference in
[`../elixir`](../elixir).

> [!WARNING]
> Symphony Rust is prototype software intended for evaluation only and is
> presented as-is. We recommend implementing your own hardened version based on
> `SPEC.md`.

## How it works

1. Polls Linear for candidate work
2. Creates a workspace per issue
3. Launches Codex in App Server mode inside the workspace
4. Sends a workflow prompt to Codex
5. Keeps Codex working on the issue until the work is done

During app-server sessions, Symphony also serves a client-side `linear_graphql`
tool so that repo skills can make raw Linear GraphQL calls.

If a claimed issue moves to a terminal state (`Done`, `Closed`, `Cancelled`, or
`Duplicate`), Symphony stops the active agent for that issue and cleans up
matching workspaces.

## How to use it

1. Make sure your codebase is set up to work well with agents: see
   [Harness engineering](https://openai.com/index/harness-engineering/).
2. Get a new personal token in Linear via Settings → Security & access →
   Personal API keys, and set it as the `LINEAR_API_KEY` environment variable.
3. Copy this directory's `.symphony.toml` and `WORKFLOW.md` to your repo.
4. Optionally copy the `commit`, `push`, `pull`, `land`, and `linear` skills to
   your repo.
   - The `linear` skill expects Symphony's `linear_graphql` app-server tool for
     raw Linear GraphQL operations such as comment editing or upload flows.
5. Customize `.symphony.toml` for runtime behavior and `WORKFLOW.md` for agent instructions.
6. Install the Rust toolchain and run the commands below.

## Prerequisites

Install the stable Rust toolchain and confirm Cargo is available:

```bash
rustc --version
cargo --version
```

If you do not already have Rust, install it with `rustup`:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup default stable
```

You also need a Linear API key before Symphony can talk to Linear:

```bash
export LINEAR_API_KEY=...
```

## Run

From a fresh checkout:

```bash
git clone https://github.com/openai/symphony
cd symphony/rust
cargo build --release --bin rsymphony
./target/release/rsymphony \
  --i-understand-that-this-will-be-running-without-the-usual-guardrails \
  --config ./.symphony.toml \
  ./WORKFLOW.md
```

You can also use the local task runner:

```bash
just run ./WORKFLOW.md ./.symphony.toml
```

The terminal dashboard renders automatically in a local TTY by default. Start
the web dashboard explicitly with `--port` when you want the browser UI.

Validate config without starting the service:

```bash
just validate
```

## Install

Install `rsymphony` into Cargo's global bin directory with the `just` alias:

```bash
cd rust
just i
```

That alias expands to Cargo's global install flow:

```bash
cargo install --path . --bin rsymphony --force
```

If you prefer not to use `just`, run the raw Cargo command directly:

```bash
cd rust
cargo install --path . --bin rsymphony --force
```

After installation, verify the binary is available:

```bash
which rsymphony
rsymphony --help
```

If your shell does not find `rsymphony`, add Cargo's bin directory to your
`PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

To remove the installed binary later:

```bash
cd rust
just u
```

Or with Cargo:

```bash
cargo uninstall rsymphony
```

## Configuration

Pass a custom workflow file path to `rsymphony` when starting the service:

```bash
rsymphony \
  --i-understand-that-this-will-be-running-without-the-usual-guardrails \
  --config /path/to/.symphony.toml \
  /path/to/custom/WORKFLOW.md
```

If no paths are passed, Symphony defaults to `./.symphony.toml` and `./WORKFLOW.md`.

Optional flags:

- `--logs-root` tells Symphony to write logs under a different directory
  (default: `./log`)
- `--port` also enables the web dashboard and API endpoints on that port
  (terminal observability remains on by default when attached to a TTY)
- `validate` checks config and prints clear errors; add `--json` for machine-readable output

`.symphony.toml` contains machine-validated runtime config. `WORKFLOW.md` is the
Codex session prompt only.

Minimal example:

```toml
[tracker]
kind = "linear"
workspace_slug = "..."
project_slug = "..."

[workspace]
root = "~/code/workspaces"

[hooks]
after_create = """
git clone git@github.com:your-org/your-repo.git .
"""

[agent]
max_concurrent_agents = 10
max_turns = 20

[codex]
command = "codex app-server"
```

```md
You are working on a Linear issue {{ issue.identifier }}.

Title: {{ issue.title }} Body: {{ issue.description }}
```

Notes:

- If a value is missing, defaults are used.
- `tracker.api_key` reads from `LINEAR_API_KEY` when unset or when value is
  `$LINEAR_API_KEY`.
- `tracker.workspace_slug` is the Linear workspace slug used for browser links
  like `https://linear.app/<workspace>/project/<project>/issues`.
- `tracker.assignee` can be a Linear assignee id or email; issues assigned to a
  different worker are treated as unroutable.
- For path values, `~` is expanded to the home directory.
- For env-backed path values, use `$VAR`. `workspace.root` resolves `$VAR`
  before path handling, while `codex.command` stays a shell command string and
  any `$VAR` expansion there happens in the launched shell.
- If `.symphony.toml` is missing, Symphony falls back to legacy `WORKFLOW.md`
  front matter for one release and prints a deprecation warning.
- If startup config is invalid, Symphony does not boot and prints all detected
  config errors.
- If a later config or prompt reload fails, Symphony keeps running with the last
  known good version and logs the reload error until the file is fixed.
- `server.port` or CLI `--port` enables the optional dashboard and JSON API at
  `/`, `/api/v1/state`, `/api/v1/<issue_identifier>`, and `/api/v1/refresh`.
- If the terminal dashboard looks wrong, confirm you are running in a real TTY
  and that `NO_COLOR` is not set.
- If the process fails to reach Linear, check `LINEAR_API_KEY` first.
- If `rsymphony` is not found after install, reopen your shell or update
  `PATH` to include `$HOME/.cargo/bin`.

## Project layout

- `src/`: runtime code
- `Cargo.toml`: crate and binary definitions
- `justfile`: local development and install commands
- `.symphony.toml`: in-repo runtime configuration
- `WORKFLOW.md`: prompt contract used by local runs
- `../.codex/`: repository-local Codex skills and setup helpers

## Testing

```bash
just check
```

Current Rust coverage focuses on unit and integration-style tests for the
runtime surface. It does not yet include the full live external end-to-end flow
that exists under `../elixir`.

## FAQ

### Why Rust?

Rust gives the implementation tighter control over process management, I/O, and
resource accounting while still producing a single deployable binary.

### What's the easiest way to set this up for my own codebase?

Launch `codex` in your repo, give it the URL to the Symphony repo, and ask it
to adapt the workflow and hooks for your environment.

## License

This project is licensed under the [Apache License 2.0](../LICENSE).
