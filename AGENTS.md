# Repository Guidelines

## Project Structure & Module Organization

This repository contains two implementations of Symphony:

- `rust/` is the active Rust port.
- `elixir/` is the reference implementation and test oracle.
- `README.md` at the repo root explains the project at a high level.

Inside `rust/`, the main code lives in `src/`:

- `src/main.rs` for the CLI entrypoint and terminal dashboard
- `src/lib.rs` and sibling modules for orchestrator, tracker, presenter, and HTTP logic
- `WORKFLOW.md` for the runtime workflow contract
- `justfile` for local developer commands

## Build, Test, and Development Commands

Work from `rust/` unless you are intentionally editing the reference app.

- `just check` runs the default validation gate.
- `just fmt` and `just fmt-check` format or verify formatting.
- `just clippy` runs linting with warnings treated as errors.
- `just test` runs the Rust test suite.
- `just run ./WORKFLOW.md` starts Symphony locally.
- `just i` installs `rsymphony` globally via Cargo.

## Coding Style & Naming Conventions

Follow standard Rust formatting and let `rustfmt` make layout decisions. Keep module names `snake_case`, types `PascalCase`, and constants `SCREAMING_SNAKE_CASE`. Prefer small, focused modules over large files when a subsystem grows. Keep dashboard and protocol formatting deterministic; avoid theme-dependent assumptions in the HTML/CSS layer.

## Testing Guidelines

Use Rust unit and integration tests colocated with the code they cover. Name tests by behavior, not implementation details, for example `formats_snapshot_content_with_running_and_retry_rows`. Run `just check` or, at minimum, `just clippy` and `just test` before handing off changes.

## Commit & Pull Request Guidelines

Commit messages in this repo use short conventional prefixes such as `feat(rust): ...`, `fix(rust): ...`, `ci(rust): ...`, or `chore(rust): ...`. Keep the subject imperative and under 72 characters. Pull requests should summarize the user-visible change, note validation run, and include screenshots or logs when dashboard behavior changes.

## Agent Notes

Do not overwrite changes you did not make. If a task touches both implementations, treat `elixir/` as the behavioral reference and `rust/` as the target implementation.
