use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use clap::Parser;
use symphony_rust::config::{CliOverrides, Settings};
use symphony_rust::http;
use symphony_rust::log_file;
use symphony_rust::orchestrator::OrchestratorRuntime;
use symphony_rust::workflow::{load, workflow_file_path};
use symphony_rust::workflow_store::WorkflowStore;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

#[derive(Debug, Parser)]
#[command(name = "symphony")]
#[command(about = "Symphony in Rust")]
struct Args {
    #[arg(long = "i-understand-that-this-will-be-running-without-the-usual-guardrails")]
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
