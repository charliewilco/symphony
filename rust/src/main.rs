use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::Parser;
use rsymphony::config::{CliOverrides, Settings};
use rsymphony::http;
use rsymphony::log_file;
use rsymphony::orchestrator::{OrchestratorHandle, OrchestratorRuntime, Snapshot};
use rsymphony::workflow::{load, workflow_file_path};
use rsymphony::workflow_store::WorkflowStore;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

#[derive(Debug, Parser)]
#[command(name = "rsymphony")]
#[command(about = "Symphony in Rust")]
struct Args {
    #[arg(
        long = "i-understand-that-this-will-be-running-without-the-usual-guardrails",
        visible_alias = "yolo"
    )]
    acknowledge_guardrails: bool,
    #[arg(long)]
    logs_root: Option<PathBuf>,
    #[arg(long)]
    port: Option<u16>,
    workflow_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = Args::parse();
    if !args.acknowledge_guardrails {
        return Err(anyhow!(acknowledgement_banner()));
    }

    let workflow_path = workflow_file_path(args.workflow_path.as_deref())?;
    if !workflow_path.exists() {
        return Err(anyhow!(
            "Workflow file not found: {}",
            workflow_path.display()
        ));
    }

    let overrides = CliOverrides {
        logs_root: args.logs_root,
        server_port_override: args.port,
    };
    let workflow = load(&workflow_path)?;
    let settings = Settings::from_workflow(&workflow, &overrides)?;
    init_tracing(&settings.effective_logs_root(&overrides))?;

    let workflow_store = WorkflowStore::new(workflow_path).await?;
    workflow_store.spawn_reload_task();
    let orchestrator =
        OrchestratorRuntime::start(workflow_store.clone(), overrides.clone()).await?;
    tokio::spawn(run_terminal_dashboard(
        orchestrator.clone(),
        workflow_store.clone(),
        overrides.clone(),
    ));

    if let Some(port) = Settings::from_workflow(&workflow_store.current().await, &overrides)?
        .server
        .port
    {
        tracing::info!("Starting HTTP server on port {port}");
        tokio::spawn(http::serve(
            orchestrator.clone(),
            workflow_store.clone(),
            overrides.clone(),
        ));
    }

    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn run_terminal_dashboard(
    orchestrator: OrchestratorHandle,
    workflow_store: WorkflowStore,
    overrides: CliOverrides,
) {
    let mut stdout = io::stdout();
    if !stdout.is_terminal() {
        return;
    }

    let mut last_rendered_content = None;
    let mut last_rendered_ms = None;
    let mut last_token_snapshot: Option<u64> = None;
    let mut token_samples: Vec<(u64, u64)> = Vec::new();

    loop {
        let (settings, refresh_ms, render_interval_ms) =
            match Settings::from_workflow(&workflow_store.current().await, &overrides) {
                Ok(settings) => {
                    let refresh_ms = settings.observability.refresh_ms;
                    let render_interval_ms = settings.observability.render_interval_ms;
                    (settings, refresh_ms, render_interval_ms)
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(1_000)).await;
                    continue;
                }
            };

        if !settings.observability.dashboard_enabled {
            tokio::time::sleep(std::time::Duration::from_millis(refresh_ms.max(1))).await;
            continue;
        }

        let now_ms = now_millis();
        let snapshot = orchestrator.snapshot().await;
        let (total_tokens, snapshot): (u64, Option<Snapshot>) = match snapshot {
            Ok(snapshot) => {
                let total_tokens = snapshot.codex_totals.total_tokens;
                last_token_snapshot = Some(total_tokens);
                (total_tokens, Some(snapshot))
            }
            Err(_) => (last_token_snapshot.unwrap_or(0), None),
        };

        let tps = rolling_tps(&mut token_samples, now_ms, total_tokens);
        let content = rsymphony::status_dashboard::format_snapshot_content_for_test(
            snapshot.as_ref(),
            &settings,
            tps,
            Some(terminal_columns()),
        );
        let content = colorize_terminal_dashboard(content);

        let content_changed = last_rendered_content.as_deref() != Some(content.as_str());
        let should_render = should_render_content(
            last_rendered_ms,
            now_ms,
            render_interval_ms,
            content_changed,
        );

        if should_render {
            let _ = write!(stdout, "\x1b[2J\x1b[H");
            let _ = write!(stdout, "{content}");
            let _ = stdout.write_all(b"\n");
            let _ = stdout.flush();
            last_rendered_ms = Some(now_ms);
            last_rendered_content = Some(content);
        }

        tokio::time::sleep(std::time::Duration::from_millis(refresh_ms.max(1))).await;
    }
}

const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_MAGENTA: &str = "\x1b[35m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_BLUE: &str = "\x1b[34m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_RESET: &str = "\x1b[0m";

fn colorize_terminal_dashboard(content: String) -> String {
    if std::env::var("NO_COLOR").is_ok() {
        return content;
    }

    let mut output = String::new();
    for line in content.lines() {
        output.push_str(&colorize_line(line));
        output.push('\n');
    }
    output
}

fn colorize_line(line: &str) -> String {
    if line.starts_with("╭─ SYMPHONY STATUS") {
        return format!("{ANSI_BOLD}{line}{ANSI_RESET}");
    }
    if line.starts_with("│ Orchestrator snapshot unavailable") {
        return format!("{ANSI_RED}{ANSI_BOLD}{line}{ANSI_RESET}");
    }
    if line.starts_with("│ Agents: ") {
        return format!(
            "{ANSI_BOLD}│ Agents: {ANSI_GREEN}{}{ANSI_RESET}",
            line_tail(line)
        );
    }
    if line.starts_with("│ Throughput: ") {
        return format!(
            "{ANSI_BOLD}│ Throughput: {ANSI_CYAN}{}{ANSI_RESET}",
            line_tail(line)
        );
    }
    if line.starts_with("│ Runtime: ") {
        return format!(
            "{ANSI_BOLD}│ Runtime: {ANSI_MAGENTA}{}{ANSI_RESET}",
            line_tail(line)
        );
    }
    if line.starts_with("│ Tokens: ") {
        return format!(
            "{ANSI_BOLD}│ Tokens: {ANSI_YELLOW}{}{ANSI_RESET}",
            line_tail(line)
        );
    }
    if line.starts_with("│ Rate Limits: ") {
        return format!(
            "{ANSI_BOLD}│ Rate Limits: {ANSI_BLUE}{}{ANSI_RESET}",
            line_tail(line)
        );
    }
    if line.contains("Rate Limits:") && line.contains("Agents:") && line.contains("Throughput:") {
        return format!("{ANSI_BOLD}│ {ANSI_GREEN}{line}{ANSI_RESET}");
    }
    if line.contains("Agents:") && line.contains("Throughput:") && line.contains("Runtime:") {
        return format!("{ANSI_BOLD}│ {ANSI_CYAN}{line}{ANSI_RESET}");
    }
    if line.contains("Tokens:") && line.contains("Rate Limits:") {
        return format!("{ANSI_BOLD}│ {ANSI_YELLOW}{line}{ANSI_RESET}");
    }
    if let Some(rest) = line.strip_prefix("│ Project: ") {
        return format!("{ANSI_BOLD}│ {ANSI_CYAN}{}{ANSI_RESET}", rest);
    }
    if let Some(rest) = line.strip_prefix("│ Dashboard: ") {
        return format!("{ANSI_BOLD}│ {ANSI_CYAN}{}{ANSI_RESET}", rest);
    }
    if line.contains("│ Status unknown") {
        return format!("{ANSI_BOLD}{ANSI_YELLOW}{} {ANSI_RESET}", line);
    }
    if line.starts_with("│ Next refresh: ") {
        return format!(
            "{ANSI_BOLD}│ Next refresh: {ANSI_CYAN}{}{ANSI_RESET}",
            line_tail(line)
        );
    }
    if line.starts_with("├─ Running") || line.starts_with("├─ Backoff queue") {
        return format!("{ANSI_BOLD}{line}{ANSI_RESET}");
    }
    if line.starts_with("├─ Status") {
        return format!("{ANSI_BOLD}{line}{ANSI_RESET}");
    }
    if line.starts_with("│ ● ") {
        return format!("{ANSI_CYAN}{line}{ANSI_RESET}");
    }
    if line.starts_with("│  ↻ ") {
        return format!("{ANSI_YELLOW}{line}{ANSI_RESET}");
    }
    if line.trim() == "│" {
        return format!("{ANSI_DIM}{line}{ANSI_RESET}");
    }
    if line.starts_with("╰") {
        return format!("{ANSI_DIM}{line}{ANSI_RESET}");
    }
    line.to_string()
}

fn line_tail(line: &str) -> &str {
    if let Some(position) = line.find(": ") {
        &line[position + 2..]
    } else {
        ""
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn should_render_content(
    last_rendered_ms: Option<u64>,
    now_ms: u64,
    render_interval_ms: u64,
    content_changed: bool,
) -> bool {
    match last_rendered_ms {
        None => true,
        Some(last) => content_changed || now_ms.saturating_sub(last) >= render_interval_ms.max(16),
    }
}

fn rolling_tps(samples: &mut Vec<(u64, u64)>, now_ms: u64, total_tokens: u64) -> f64 {
    const TPS_WINDOW_MS: u64 = 5_000;
    let window_start = now_ms.saturating_sub(TPS_WINDOW_MS);
    samples.push((now_ms, total_tokens));
    samples.retain(|(timestamp, _)| *timestamp >= window_start);

    if samples.len() < 2 {
        return 0.0;
    }

    let Some((earliest_ts, earliest_tokens)) = samples.first().copied() else {
        return 0.0;
    };
    let elapsed_ms = now_ms.saturating_sub(earliest_ts);
    if elapsed_ms == 0 {
        return 0.0;
    }
    let token_delta = total_tokens.saturating_sub(earliest_tokens);
    (token_delta as f64) / (elapsed_ms as f64 / 1000.0)
}

fn terminal_columns() -> usize {
    let default_columns = 115;
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|columns| *columns >= 80)
        .unwrap_or(default_columns)
}

fn init_tracing(logs_root: &Path) -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
    let log_file = log_file::default_log_file_for_root(logs_root);
    std::fs::create_dir_all(log_file.parent().unwrap_or(logs_root))?;
    let file_appender = tracing_appender::rolling::never(
        log_file.parent().unwrap_or(logs_root),
        log_file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("symphony.log"),
    );
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let _ = Box::leak(Box::new(guard));
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(non_blocking),
        );
    tracing::subscriber::set_global_default(subscriber)?;

    Ok(())
}

fn acknowledgement_banner() -> String {
    [
        "This Symphony implementation is a low key engineering preview.",
        "Codex will run without any guardrails.",
        "Symphony Rust is not a supported product and is presented as-is.",
        "To proceed, start with `--i-understand-that-this-will-be-running-without-the-usual-guardrails` CLI argument",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_legacy_guardrail_acknowledgement_flag() {
        let args = Args::try_parse_from([
            "rsymphony",
            "--i-understand-that-this-will-be-running-without-the-usual-guardrails",
        ])
        .unwrap();
        assert!(args.acknowledge_guardrails);
    }

    #[test]
    fn accepts_yolo_alias_for_guardrail_acknowledgement() {
        let args = Args::try_parse_from(["rsymphony", "--yolo"]).unwrap();
        assert!(args.acknowledge_guardrails);
    }
}
