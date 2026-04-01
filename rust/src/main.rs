use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::{Args as ClapArgs, Parser, Subcommand};
use rsymphony::config::{CliOverrides, Settings, ValidateOutput, config_file_path, validate};
use rsymphony::config_store::ConfigStore;
use rsymphony::http;
use rsymphony::log_file;
use rsymphony::orchestrator::{OrchestratorHandle, OrchestratorRuntime, Snapshot};
use rsymphony::workflow::workflow_file_path;
use rsymphony::workflow_store::WorkflowStore;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

#[derive(Debug, Parser)]
#[command(name = "rsymphony")]
#[command(about = "Symphony in Rust")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(
        long = "i-understand-that-this-will-be-running-without-the-usual-guardrails",
        visible_alias = "yolo"
    )]
    acknowledge_guardrails: bool,
    #[arg(long)]
    logs_root: Option<PathBuf>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long = "config")]
    config_path: Option<PathBuf>,
    workflow_path: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Validate(ValidateArgs),
}

#[derive(Debug, ClapArgs)]
struct ValidateArgs {
    #[arg(long)]
    json: bool,
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
    if let Some(command) = args.command {
        return match command {
            Command::Validate(validate) => run_validate(validate, args.config_path),
        };
    }

    if !args.acknowledge_guardrails {
        return Err(anyhow!(acknowledgement_banner()));
    }

    let workflow_path = workflow_file_path(args.workflow_path.as_deref())?;
    let config_path = config_file_path(args.config_path.as_deref())?;

    let overrides = CliOverrides {
        logs_root: args.logs_root,
        server_port_override: args.port,
    };
    let resolved = Settings::load(&config_path, Some(&workflow_path), &overrides)?;
    let settings = resolved.settings.clone();
    init_tracing(&settings.effective_logs_root(&overrides))?;
    for warning in &resolved.warnings {
        tracing::warn!("{warning}");
    }

    let workflow_store = WorkflowStore::new(workflow_path).await?;
    let config_store =
        ConfigStore::new(config_path, workflow_store.path().await, overrides.clone()).await?;
    config_store.spawn_reload_task();
    workflow_store.spawn_reload_task();
    let orchestrator = OrchestratorRuntime::start(
        config_store.clone(),
        workflow_store.clone(),
        overrides.clone(),
    )
    .await?;
    tokio::spawn(run_terminal_dashboard(
        orchestrator.clone(),
        config_store.clone(),
        workflow_store.clone(),
        overrides.clone(),
    ));

    if let Some(port) = config_store.current_settings().await.server.port {
        tracing::info!("Starting HTTP server on port {port}");
        tokio::spawn(http::serve(
            orchestrator.clone(),
            config_store.clone(),
            workflow_store.clone(),
            overrides.clone(),
        ));
    }

    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn run_terminal_dashboard(
    orchestrator: OrchestratorHandle,
    config_store: ConfigStore,
    _workflow_store: WorkflowStore,
    _overrides: CliOverrides,
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
        let settings = config_store.current_settings().await;
        let refresh_ms = settings.observability.refresh_ms;
        let render_interval_ms = settings.observability.render_interval_ms;

        if !settings.observability.dashboard_enabled {
            tokio::time::sleep(std::time::Duration::from_millis(refresh_ms.max(1))).await;
            continue;
        }

        let now_ms = now_millis();
        let snapshot = orchestrator.snapshot().await;
        let (total_tokens, snapshot): (u64, Option<Snapshot>) = match snapshot {
            Ok(snapshot) => {
                let total_tokens = snapshot.agent_totals.total_tokens;
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

fn run_validate(args: ValidateArgs, config_override: Option<PathBuf>) -> Result<()> {
    let workflow_path = args
        .workflow_path
        .as_deref()
        .map(|path| workflow_file_path(Some(path)))
        .transpose()?;
    let config_path = config_file_path(config_override.as_deref())?;
    let overrides = CliOverrides::default();

    match validate(&config_path, workflow_path.as_deref(), &overrides) {
        Ok(output) => {
            print_validate_output(&output, args.json)?;
            Ok(())
        }
        Err(output) => {
            print_validate_output(&output, args.json)?;
            Err(anyhow!("config validation failed"))
        }
    }
}

fn print_validate_output(output: &ValidateOutput, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(output)?);
        return Ok(());
    }

    if output.valid {
        println!(
            "Config valid: {} ({})",
            output.config_path, output.config_format
        );
        if let Some(workflow_path) = &output.workflow_path {
            println!("Workflow prompt readable: {workflow_path}");
        }
        for warning in &output.warnings {
            println!("warning: {warning}");
        }
        return Ok(());
    }

    println!(
        "Configuration invalid in {} ({})",
        output.config_path, output.config_format
    );
    for diagnostic in &output.diagnostics {
        let mut location = String::new();
        if let Some(line) = diagnostic.line {
            if let Some(column) = diagnostic.column {
                location = format!(" at {line}:{column}");
            } else {
                location = format!(" at line {line}");
            }
        }
        if let Some(field_path) = &diagnostic.field_path {
            println!(
                "- {} [{}] ({}){}",
                diagnostic.message, field_path, diagnostic.code, location
            );
        } else {
            println!("- {} ({}){}", diagnostic.message, diagnostic.code, location);
        }
        if let Some(hint) = &diagnostic.hint {
            println!("  hint: {hint}");
        }
    }
    Ok(())
}

const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_BLUE: &str = "\x1b[34m";
const ANSI_MAGENTA: &str = "\x1b[35m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_RESET: &str = "\x1b[0m";
const RUNNING_ID_WIDTH: usize = 8;
const RUNNING_STAGE_WIDTH: usize = 14;
const RUNNING_PID_WIDTH: usize = 8;
const RUNNING_AGE_WIDTH: usize = 12;
const RUNNING_TOKENS_WIDTH: usize = 10;
const RUNNING_SESSION_WIDTH: usize = 14;

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
    if let Some(line) = colorize_running_table_header(line) {
        return line;
    }
    if let Some(line) = colorize_running_table_row(line) {
        return line;
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
        return format!("{ANSI_DIM}{ANSI_BOLD}{line}{ANSI_RESET}");
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

fn colorize_running_table_header(line: &str) -> Option<String> {
    let body = line.strip_prefix("│   ")?;
    let columns = split_running_table_columns(body)?;
    Some(format!(
        "│   {} {} {} {} {} {} {}",
        colorize_cell(&columns[0], ANSI_DIM),
        colorize_cell(&columns[1], ANSI_DIM),
        colorize_cell(&columns[2], ANSI_DIM),
        colorize_cell(&columns[3], ANSI_DIM),
        colorize_cell(&columns[4], ANSI_DIM),
        colorize_cell(&columns[5], ANSI_DIM),
        colorize_cell(&columns[6], ANSI_DIM),
    ))
}

fn colorize_running_table_row(line: &str) -> Option<String> {
    let body = line.strip_prefix("│ ● ")?;
    let columns = split_running_table_columns(body)?;
    Some(format!(
        "│ ● {} {} {} {} {} {} {}",
        colorize_cell(&columns[0], ANSI_CYAN),
        colorize_stage_cell(&columns[1]),
        colorize_cell(&columns[2], ANSI_YELLOW),
        colorize_cell(&columns[3], ANSI_MAGENTA),
        colorize_cell(&columns[4], ANSI_BLUE),
        colorize_cell(&columns[5], ANSI_CYAN),
        colorize_event_cell(&columns[6]),
    ))
}

fn split_running_table_columns(body: &str) -> Option<Vec<String>> {
    let widths = [
        RUNNING_ID_WIDTH,
        RUNNING_STAGE_WIDTH,
        RUNNING_PID_WIDTH,
        RUNNING_AGE_WIDTH,
        RUNNING_TOKENS_WIDTH,
        RUNNING_SESSION_WIDTH,
    ];
    let chars = body.chars().collect::<Vec<_>>();
    let event_width = chars
        .len()
        .checked_sub(widths.iter().sum::<usize>() + widths.len())?;
    let mut widths = widths.to_vec();
    widths.push(event_width);

    let mut columns = Vec::with_capacity(widths.len());
    let mut index = 0usize;
    for (slot, width) in widths.into_iter().enumerate() {
        let end = index + width;
        if end > chars.len() {
            return None;
        }
        columns.push(chars[index..end].iter().collect::<String>());
        index = end;
        if slot < 6 {
            if chars.get(index) != Some(&' ') {
                return None;
            }
            index += 1;
        }
    }

    Some(columns)
}

fn colorize_cell(value: &str, color: &str) -> String {
    format!("{color}{value}{ANSI_RESET}")
}

fn colorize_stage_cell(value: &str) -> String {
    let trimmed = value.trim();
    let color = match trimmed {
        "In Progress" => ANSI_GREEN,
        "Rework" => ANSI_YELLOW,
        "Done" | "Completed" => ANSI_BLUE,
        "Todo" => ANSI_BLUE,
        _ => ANSI_CYAN,
    };
    colorize_cell(value, color)
}

fn colorize_event_cell(value: &str) -> String {
    let trimmed = value.trim().to_ascii_lowercase();
    let color = if trimmed.contains("failed") || trimmed.contains("error") {
        ANSI_RED
    } else if trimmed.contains("completed") {
        ANSI_GREEN
    } else if trimmed.contains("token") {
        ANSI_YELLOW
    } else {
        ANSI_CYAN
    };
    colorize_cell(value, color)
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
        "Your configured agent provider will run without any guardrails.",
        "Symphony Rust is not a supported product and is presented as-is.",
        "To proceed, start with `--i-understand-that-this-will-be-running-without-the-usual-guardrails` CLI argument",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rsymphony::config::{CliOverrides, ProviderKind, Settings};
    use rsymphony::orchestrator::{
        PollingSnapshot, RetrySnapshot, RunningSnapshot, Snapshot, TokenTotals,
    };
    use rsymphony::status_dashboard::format_snapshot_content_for_test;
    use serde_json::json;
    use tempfile::tempdir;

    fn settings() -> Settings {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join(".symphony.toml");
        let workflow_path = dir.path().join("WORKFLOW.md");
        std::fs::write(
            &config_path,
            "[tracker]\nkind = \"memory\"\nproject_slug = \"demo\"\n[server]\nport = 4000\n",
        )
        .unwrap();
        std::fs::write(&workflow_path, "Prompt body\n").unwrap();
        Settings::load(&config_path, Some(&workflow_path), &CliOverrides::default())
            .unwrap()
            .settings
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            running: vec![RunningSnapshot {
                issue_id: "issue-1".to_string(),
                identifier: "MT-725".to_string(),
                state: "In Progress".to_string(),
                provider_kind: ProviderKind::Codex,
                worker_host: None,
                workspace_path: None,
                session_id: Some("thread-1-turn-1".to_string()),
                provider_process_id: Some("2510350".to_string()),
                agent_input_tokens: 100,
                agent_output_tokens: 25,
                agent_total_tokens: 125,
                turn_count: 3,
                started_at: Utc::now(),
                last_agent_timestamp: None,
                last_agent_message: Some(json!({
                    "event": "command_output_streaming",
                    "message": { "text": "command output streaming: > mt-721" }
                })),
                last_agent_event: Some("command_output_streaming".to_string()),
                runtime_seconds: 90,
            }],
            retrying: vec![RetrySnapshot {
                issue_id: "issue-2".to_string(),
                attempt: 2,
                due_in_ms: 1_500,
                identifier: Some("MT-202".to_string()),
                error: Some("error with \\nnewline".to_string()),
                worker_host: None,
                workspace_path: None,
            }],
            agent_totals: TokenTotals {
                input_tokens: 100,
                output_tokens: 25,
                total_tokens: 125,
                seconds_running: 90,
            },
            rate_limits: Some(json!({
                "limit_id": "gpt-5",
                "primary": { "remaining": 10, "limit": 20 },
                "credits": { "unlimited": true }
            })),
            polling: PollingSnapshot {
                checking: false,
                next_poll_in_ms: Some(5_000),
                poll_interval_ms: 30_000,
            },
        }
    }

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

    #[test]
    fn parses_validate_subcommand_with_config_override() {
        let args = Args::try_parse_from([
            "rsymphony",
            "--config",
            "/tmp/.symphony.toml",
            "validate",
            "/tmp/WORKFLOW.md",
        ])
        .unwrap();

        assert_eq!(
            args.config_path.as_deref(),
            Some(Path::new("/tmp/.symphony.toml"))
        );
        match args.command {
            Some(Command::Validate(validate)) => {
                assert_eq!(
                    validate.workflow_path.as_deref(),
                    Some(Path::new("/tmp/WORKFLOW.md"))
                );
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_validate_subcommand_and_json_flag() {
        let args =
            Args::try_parse_from(["rsymphony", "--config", "custom.toml", "validate", "--json"])
                .unwrap();
        assert_eq!(args.config_path, Some(PathBuf::from("custom.toml")));
        match args.command {
            Some(Command::Validate(validate)) => assert!(validate.json),
            _ => panic!("expected validate subcommand"),
        }
    }

    #[test]
    fn colorizes_running_table_columns_individually() {
        let rendered =
            format_snapshot_content_for_test(Some(&snapshot()), &settings(), 658_875.2, Some(115));
        let line = rendered
            .lines()
            .find(|line| line.starts_with("│ ● "))
            .expect("running row");
        let colored = colorize_line(line);
        assert!(colored.contains(ANSI_CYAN));
        assert!(colored.contains(ANSI_GREEN));
        assert!(colored.contains(ANSI_YELLOW));
        assert!(colored.contains(ANSI_MAGENTA));
        assert!(colored.contains(ANSI_BLUE));
        assert!(colored.contains("MT-725"));
        assert!(colored.contains("command output streaming"));
    }

    #[test]
    fn colorizes_running_table_header_columns() {
        let line =
            "│   ID       STAGE          PID      AGE / TURN   TOKENS     SESSION        EVENT";
        let colored = colorize_line(line);
        assert!(colored.contains(ANSI_DIM));
        assert!(colored.contains("AGE / TURN"));
        assert!(colored.contains("EVENT"));
    }
}
