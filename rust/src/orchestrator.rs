use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::agent_runner::{self, WorkerEvent};
use crate::codex::CodexUpdate;
use crate::config::{CliOverrides, RefreshPayload, Settings, normalize_issue_state};
use crate::tracker::{Issue, Tracker, tracker_for_settings};
use crate::workflow_store::WorkflowStore;
use crate::workspace;

const CONTINUATION_RETRY_DELAY_MS: u64 = 1_000;
const FAILURE_RETRY_BASE_MS: u64 = 10_000;

#[derive(Clone)]
pub struct OrchestratorHandle {
    command_tx: mpsc::Sender<OrchestratorCommand>,
}

pub struct OrchestratorRuntime {
    state: Arc<Mutex<OrchestratorState>>,
    command_tx: mpsc::Sender<OrchestratorCommand>,
    workflow_store: WorkflowStore,
    overrides: CliOverrides,
    worker_events_tx: mpsc::Sender<WorkerEvent>,
    worker_events_rx: mpsc::Receiver<WorkerEvent>,
    command_rx: mpsc::Receiver<OrchestratorCommand>,
}

#[derive(Default)]
pub struct OrchestratorState {
    pub poll_interval_ms: u64,
    pub max_concurrent_agents: usize,
    pub max_retry_backoff_ms: u64,
    pub next_poll_due_at_ms: Option<u64>,
    pub poll_check_in_progress: bool,
    pub running: HashMap<String, RunningEntry>,
    pub claimed: HashSet<String>,
    pub retry_attempts: HashMap<String, RetryEntry>,
    pub codex_totals: TokenTotals,
    pub codex_rate_limits: Option<JsonValue>,
    pub refresh_pending: bool,
    pub refresh_coalesced: bool,
    retry_token_counter: u64,
}

pub struct RunningEntry {
    pub identifier: String,
    pub issue: Issue,
    pub started_at: DateTime<Utc>,
    pub session_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub codex_input_tokens: u64,
    pub codex_output_tokens: u64,
    pub codex_total_tokens: u64,
    pub codex_last_reported_input_tokens: u64,
    pub codex_last_reported_output_tokens: u64,
    pub codex_last_reported_total_tokens: u64,
    pub turn_count: u64,
    pub last_codex_timestamp: Option<DateTime<Utc>>,
    pub last_codex_message: Option<JsonValue>,
    pub last_codex_event: Option<String>,
    pub runtime_seconds: u64,
    pub workspace_path: Option<String>,
    pub worker_host: Option<String>,
    pub task: JoinHandle<()>,
    pub attempt: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RetryEntry {
    pub issue_id: String,
    pub identifier: Option<String>,
    pub attempt: u32,
    pub due_at_ms: u64,
    pub error: Option<String>,
    pub worker_host: Option<String>,
    pub workspace_path: Option<String>,
    pub token: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TokenTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub seconds_running: u64,
}

enum OrchestratorCommand {
    Snapshot {
        reply: oneshot::Sender<Snapshot>,
    },
    RequestRefresh {
        reply: oneshot::Sender<RefreshPayload>,
    },
    RetryFired {
        issue_id: String,
        token: u64,
    },
}

#[derive(Clone, Copy)]
enum RetryKind {
    Continuation,
    Failure,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkerSelection {
    Local,
    Host(String),
    NoCapacity,
}

struct RetryRequest {
    issue_id: String,
    attempt: u32,
    retry_kind: RetryKind,
    identifier: Option<String>,
    error: Option<String>,
    worker_host: Option<String>,
    workspace_path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub running: Vec<RunningSnapshot>,
    pub retrying: Vec<RetrySnapshot>,
    pub codex_totals: TokenTotals,
    pub rate_limits: Option<JsonValue>,
    pub polling: PollingSnapshot,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunningSnapshot {
    pub issue_id: String,
    pub identifier: String,
    pub state: String,
    pub worker_host: Option<String>,
    pub workspace_path: Option<String>,
    pub session_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub codex_input_tokens: u64,
    pub codex_output_tokens: u64,
    pub codex_total_tokens: u64,
    pub turn_count: u64,
    pub started_at: DateTime<Utc>,
    pub last_codex_timestamp: Option<DateTime<Utc>>,
    pub last_codex_message: Option<JsonValue>,
    pub last_codex_event: Option<String>,
    pub runtime_seconds: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RetrySnapshot {
    pub issue_id: String,
    pub attempt: u32,
    pub due_in_ms: u64,
    pub identifier: Option<String>,
    pub error: Option<String>,
    pub worker_host: Option<String>,
    pub workspace_path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PollingSnapshot {
    pub checking: bool,
    pub next_poll_in_ms: Option<u64>,
    pub poll_interval_ms: u64,
}

impl OrchestratorHandle {
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(OrchestratorCommand::Snapshot { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("orchestrator_unavailable"))?;
        Ok(reply_rx.await?)
    }

    pub async fn request_refresh(&self) -> Result<RefreshPayload> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.command_tx
            .send(OrchestratorCommand::RequestRefresh { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("orchestrator_unavailable"))?;
        Ok(reply_rx.await?)
    }
}

impl OrchestratorRuntime {
    pub async fn start(
        workflow_store: WorkflowStore,
        overrides: CliOverrides,
    ) -> Result<OrchestratorHandle> {
        let current_settings =
            Settings::from_workflow(&workflow_store.current().await, &overrides)?;
        let state = Arc::new(Mutex::new(OrchestratorState {
            poll_interval_ms: current_settings.polling.interval_ms,
            max_concurrent_agents: current_settings.agent.max_concurrent_agents,
            max_retry_backoff_ms: current_settings.agent.max_retry_backoff_ms,
            ..OrchestratorState::default()
        }));
        let (command_tx, command_rx) = mpsc::channel(64);
        let (worker_events_tx, worker_events_rx) = mpsc::channel(256);
        let handle = OrchestratorHandle {
            command_tx: command_tx.clone(),
        };
        let mut runtime = Self {
            state,
            command_tx: command_tx.clone(),
            workflow_store,
            overrides,
            worker_events_tx,
            worker_events_rx,
            command_rx,
        };
        tokio::spawn(async move {
            if let Err(error) = runtime.run().await {
                tracing::error!("Orchestrator stopped: {error}");
            }
        });
        Ok(handle)
    }

    async fn run(&mut self) -> Result<()> {
        self.run_terminal_cleanup().await;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        {
            let mut state = self.state.lock().await;
            state.next_poll_due_at_ms = Some(now_millis());
        }

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.refresh_settings().await?;
                    self.handle_tick().await?;
                }
                Some(worker_event) = self.worker_events_rx.recv() => {
                    self.handle_worker_event(worker_event).await?;
                }
                Some(command) = self.command_rx.recv() => {
                    self.handle_command(command).await?;
                }
            }
        }
    }

    async fn refresh_settings(&self) -> Result<Settings> {
        if let Err(error) = self.workflow_store.maybe_reload().await {
            tracing::error!("Failed to reload workflow: {error}");
        }
        let settings =
            Settings::from_workflow(&self.workflow_store.current().await, &self.overrides)?;
        let mut state = self.state.lock().await;
        state.poll_interval_ms = settings.polling.interval_ms;
        state.max_concurrent_agents = settings.agent.max_concurrent_agents;
        state.max_retry_backoff_ms = settings.agent.max_retry_backoff_ms;
        Ok(settings)
    }

    async fn run_terminal_cleanup(&self) {
        let Ok(settings) =
            Settings::from_workflow(&self.workflow_store.current().await, &self.overrides)
        else {
            return;
        };
        let tracker = tracker_for_settings(&settings);
        let Ok(issues) = tracker
            .fetch_issues_by_states(&settings.tracker.terminal_states, &settings)
            .await
        else {
            return;
        };
        for issue in issues {
            if settings.worker.ssh_hosts.is_empty() {
                let _ = workspace::remove_issue_workspace(&issue.identifier, &settings, None).await;
            } else {
                for host in &settings.worker.ssh_hosts {
                    let _ =
                        workspace::remove_issue_workspace(&issue.identifier, &settings, Some(host))
                            .await;
                }
            }
        }
    }

    async fn handle_tick(&mut self) -> Result<()> {
        let settings =
            Settings::from_workflow(&self.workflow_store.current().await, &self.overrides)?;
        let should_poll = {
            let mut state = self.state.lock().await;
            let now = now_millis();
            let due = state.next_poll_due_at_ms.unwrap_or(0);
            if due > now {
                false
            } else {
                state.poll_check_in_progress = true;
                true
            }
        };
        if !should_poll {
            return Ok(());
        }

        self.reconcile_running_issues(&settings).await?;
        self.dispatch_ready_retries(&settings).await?;
        self.dispatch_candidate_issues(&settings).await?;

        let mut state = self.state.lock().await;
        state.poll_check_in_progress = false;
        state.next_poll_due_at_ms = Some(now_millis() + settings.polling.interval_ms);
        state.refresh_pending = false;
        Ok(())
    }

    async fn dispatch_candidate_issues(&mut self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        let tracker = tracker_for_settings(settings);
        let mut issues = tracker.fetch_candidate_issues(settings).await?;
        issues.sort_by_key(|issue| (issue.priority.unwrap_or(i64::MAX), issue.created_at));
        for issue in issues {
            if !self.should_dispatch_issue(&issue, settings).await {
                continue;
            }
            let Some(issue) = self
                .revalidate_issue_for_dispatch(issue, tracker.clone(), settings)
                .await?
            else {
                continue;
            };
            self.dispatch_issue(issue, None, None, settings, tracker.clone())
                .await?;
        }
        Ok(())
    }

    async fn dispatch_ready_retries(&mut self, settings: &Settings) -> Result<()> {
        let now = now_millis();
        let ready = {
            let state = self.state.lock().await;
            state
                .retry_attempts
                .values()
                .filter(|entry| entry.due_at_ms <= now)
                .cloned()
                .collect::<Vec<_>>()
        };

        if ready.is_empty() {
            return Ok(());
        }

        let tracker = tracker_for_settings(settings);
        for retry in ready {
            {
                let state = self.state.lock().await;
                if state
                    .retry_attempts
                    .get(&retry.issue_id)
                    .is_none_or(|current| current.token != retry.token)
                {
                    continue;
                }
            }

            let issues = tracker
                .fetch_issue_states_by_ids(std::slice::from_ref(&retry.issue_id), settings)
                .await?;
            let Some(issue) = issues.into_iter().next() else {
                let mut state = self.state.lock().await;
                state.retry_attempts.remove(&retry.issue_id);
                state.claimed.remove(&retry.issue_id);
                continue;
            };
            let Some(issue) = self
                .revalidate_issue_for_dispatch(issue, tracker.clone(), settings)
                .await?
            else {
                let mut state = self.state.lock().await;
                state.retry_attempts.remove(&retry.issue_id);
                state.claimed.remove(&retry.issue_id);
                continue;
            };

            if !self
                .can_dispatch_retry(&issue, retry.worker_host.as_deref(), settings)
                .await
            {
                self.schedule_issue_retry(RetryRequest {
                    issue_id: issue.id.clone(),
                    attempt: retry.attempt.saturating_add(1),
                    retry_kind: RetryKind::Failure,
                    identifier: Some(issue.identifier.clone()),
                    error: Some("no available orchestrator slots".to_string()),
                    worker_host: retry.worker_host.clone(),
                    workspace_path: retry.workspace_path.clone(),
                })
                .await;
                continue;
            }

            self.dispatch_issue(
                issue,
                Some(retry.attempt),
                retry.worker_host.clone(),
                settings,
                tracker.clone(),
            )
            .await?;
        }

        Ok(())
    }

    async fn reconcile_running_issues(&mut self, settings: &Settings) -> Result<()> {
        self.reconcile_stalled_runs(settings).await;
        let running_ids = {
            let state = self.state.lock().await;
            state.running.keys().cloned().collect::<Vec<_>>()
        };
        if running_ids.is_empty() {
            return Ok(());
        }
        let tracker = tracker_for_settings(settings);
        let issues = match tracker
            .fetch_issue_states_by_ids(&running_ids, settings)
            .await
        {
            Ok(issues) => issues,
            Err(error) => {
                tracing::debug!("Failed to refresh running issues: {error}");
                return Ok(());
            }
        };
        let issues_by_id = issues
            .into_iter()
            .map(|issue| (issue.id.clone(), issue))
            .collect::<HashMap<_, _>>();

        for issue_id in running_ids {
            if let Some(issue) = issues_by_id.get(&issue_id) {
                if terminal_state(issue, settings) {
                    self.terminate_running_issue(&issue_id, true, settings)
                        .await;
                } else if !active_state(issue, settings) || !issue_routable_to_worker(issue) {
                    self.terminate_running_issue(&issue_id, false, settings)
                        .await;
                }
            } else {
                self.terminate_running_issue(&issue_id, false, settings)
                    .await;
            }
        }
        Ok(())
    }

    async fn reconcile_stalled_runs(&mut self, settings: &Settings) {
        if settings.codex.stall_timeout_ms == 0 {
            return;
        }
        let now = Utc::now();
        let stalled_runs = {
            let state = self.state.lock().await;
            state
                .running
                .iter()
                .filter_map(|(issue_id, entry)| {
                    let reference = entry.last_codex_timestamp.unwrap_or(entry.started_at);
                    let elapsed = now.signed_duration_since(reference).num_milliseconds();
                    (elapsed > settings.codex.stall_timeout_ms as i64).then(|| {
                        (
                            issue_id.clone(),
                            entry.identifier.clone(),
                            elapsed as u64,
                            next_retry_attempt(entry.attempt),
                            entry.worker_host.clone(),
                            entry.workspace_path.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        for (issue_id, identifier, elapsed_ms, attempt, worker_host, workspace_path) in stalled_runs
        {
            self.terminate_running_issue(&issue_id, false, settings)
                .await;
            self.schedule_issue_retry(RetryRequest {
                issue_id,
                attempt,
                retry_kind: RetryKind::Failure,
                identifier: Some(identifier),
                error: Some(format!("stalled for {elapsed_ms}ms without codex activity")),
                worker_host,
                workspace_path,
            })
            .await;
        }
    }

    async fn should_dispatch_issue(&self, issue: &Issue, settings: &Settings) -> bool {
        if !active_state(issue, settings)
            || terminal_state(issue, settings)
            || !issue_routable_to_worker(issue)
        {
            return false;
        }
        {
            let state = self.state.lock().await;
            if state.claimed.contains(&issue.id) || state.running.contains_key(&issue.id) {
                return false;
            }
            if state.running.len() >= state.max_concurrent_agents {
                return false;
            }
            let same_state_running = state
                .running
                .values()
                .filter(|entry| {
                    normalize_issue_state(&entry.issue.state) == normalize_issue_state(&issue.state)
                })
                .count();
            if same_state_running >= settings.max_concurrent_agents_for_state(&issue.state) {
                return false;
            }
            if matches!(
                select_worker_host_for_state(
                    &state,
                    None,
                    settings.worker.ssh_hosts.as_slice(),
                    settings.worker.max_concurrent_agents_per_host,
                ),
                WorkerSelection::NoCapacity
            ) {
                return false;
            }
        }

        !todo_issue_blocked_by_non_terminal(issue, settings)
    }

    async fn dispatch_issue(
        &mut self,
        issue: Issue,
        attempt: Option<u32>,
        preferred_worker_host: Option<String>,
        settings: &Settings,
        tracker: Arc<dyn Tracker>,
    ) -> Result<()> {
        let worker_host = match self
            .select_worker_host(preferred_worker_host, settings)
            .await
        {
            WorkerSelection::Local => None,
            WorkerSelection::Host(host) => Some(host.to_string()),
            WorkerSelection::NoCapacity => return Ok(()),
        };
        let issue_id = issue.id.clone();
        let workflow = self.workflow_store.current().await;
        let task = tokio::spawn(agent_runner::run(
            issue.clone(),
            settings.clone(),
            workflow.prompt_template.clone(),
            tracker,
            self.worker_events_tx.clone(),
            worker_host.clone(),
            attempt,
        ));

        let mut state = self.state.lock().await;
        state.claimed.insert(issue_id.clone());
        state.retry_attempts.remove(&issue_id);
        state.running.insert(
            issue_id,
            RunningEntry {
                identifier: issue.identifier.clone(),
                issue,
                started_at: Utc::now(),
                session_id: None,
                codex_app_server_pid: None,
                codex_input_tokens: 0,
                codex_output_tokens: 0,
                codex_total_tokens: 0,
                codex_last_reported_input_tokens: 0,
                codex_last_reported_output_tokens: 0,
                codex_last_reported_total_tokens: 0,
                turn_count: 0,
                last_codex_timestamp: None,
                last_codex_message: None,
                last_codex_event: None,
                runtime_seconds: 0,
                workspace_path: None,
                worker_host,
                task,
                attempt,
            },
        );
        Ok(())
    }

    async fn select_worker_host(
        &self,
        preferred_worker_host: Option<String>,
        settings: &Settings,
    ) -> WorkerSelection {
        let state = self.state.lock().await;
        select_worker_host_for_state(
            &state,
            preferred_worker_host.as_deref(),
            settings.worker.ssh_hosts.as_slice(),
            settings.worker.max_concurrent_agents_per_host,
        )
    }

    async fn terminate_running_issue(
        &mut self,
        issue_id: &str,
        cleanup_workspace: bool,
        settings: &Settings,
    ) {
        let entry = {
            let mut state = self.state.lock().await;
            state.claimed.remove(issue_id);
            state.running.remove(issue_id)
        };
        if let Some(entry) = entry {
            self.record_runtime_seconds(entry.started_at).await;
            entry.task.abort();
            if cleanup_workspace {
                let _ = workspace::remove_issue_workspace(
                    &entry.identifier,
                    settings,
                    entry.worker_host.as_deref(),
                )
                .await;
            }
        }
    }

    async fn revalidate_issue_for_dispatch(
        &self,
        issue: Issue,
        tracker: Arc<dyn Tracker>,
        settings: &Settings,
    ) -> Result<Option<Issue>> {
        let refreshed = tracker
            .fetch_issue_states_by_ids(std::slice::from_ref(&issue.id), settings)
            .await?;
        let Some(issue) = refreshed.into_iter().next() else {
            return Ok(None);
        };
        if retry_candidate_issue(&issue, settings) {
            Ok(Some(issue))
        } else {
            Ok(None)
        }
    }

    async fn can_dispatch_retry(
        &self,
        issue: &Issue,
        preferred_worker_host: Option<&str>,
        settings: &Settings,
    ) -> bool {
        if !retry_candidate_issue(issue, settings) {
            return false;
        }

        let state = self.state.lock().await;
        if state.running.len() >= state.max_concurrent_agents {
            return false;
        }
        if running_issue_count_for_state(&state.running, &issue.state)
            >= settings.max_concurrent_agents_for_state(&issue.state)
        {
            return false;
        }

        !matches!(
            select_worker_host_for_state(
                &state,
                preferred_worker_host,
                settings.worker.ssh_hosts.as_slice(),
                settings.worker.max_concurrent_agents_per_host,
            ),
            WorkerSelection::NoCapacity
        )
    }

    async fn schedule_issue_retry(&mut self, request: RetryRequest) {
        let delay = {
            let state = self.state.lock().await;
            retry_delay_ms(
                request.retry_kind,
                request.attempt,
                state.max_retry_backoff_ms,
            )
        };
        let due_at_ms = now_millis() + delay;
        let token = {
            let mut state = self.state.lock().await;
            state.retry_token_counter += 1;
            let token = state.retry_token_counter;
            state.retry_attempts.insert(
                request.issue_id.clone(),
                RetryEntry {
                    issue_id: request.issue_id.clone(),
                    identifier: request.identifier,
                    attempt: request.attempt,
                    due_at_ms,
                    error: request.error,
                    worker_host: request.worker_host.clone(),
                    workspace_path: request.workspace_path.clone(),
                    token,
                },
            );
            state.claimed.insert(request.issue_id.clone());
            token
        };
        let command_tx = self.command_tx.clone();
        let issue_id = request.issue_id;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            let _ = command_tx
                .send(OrchestratorCommand::RetryFired { issue_id, token })
                .await;
        });
    }

    async fn handle_worker_event(&mut self, event: WorkerEvent) -> Result<()> {
        match event {
            WorkerEvent::RuntimeInfo { issue_id, runtime } => {
                let mut state = self.state.lock().await;
                if let Some(entry) = state.running.get_mut(&issue_id) {
                    entry.workspace_path = Some(runtime.workspace_path);
                    entry.worker_host = runtime.worker_host;
                }
            }
            WorkerEvent::CodexUpdate { issue_id, update } => {
                self.integrate_codex_update(&issue_id, update).await;
            }
            WorkerEvent::Exit { issue_id, reason } => {
                let (identifier, worker_host, workspace_path, next_attempt, started_at) = {
                    let mut state = self.state.lock().await;
                    let Some(entry) = state.running.remove(&issue_id) else {
                        return Ok(());
                    };
                    state.claimed.remove(&issue_id);
                    (
                        entry.identifier,
                        entry.worker_host,
                        entry.workspace_path,
                        next_retry_attempt(entry.attempt),
                        entry.started_at,
                    )
                };
                self.record_runtime_seconds(started_at).await;
                match reason {
                    Ok(()) => {
                        self.schedule_issue_retry(RetryRequest {
                            issue_id,
                            attempt: 1,
                            retry_kind: RetryKind::Continuation,
                            identifier: Some(identifier),
                            error: None,
                            worker_host,
                            workspace_path,
                        })
                        .await;
                    }
                    Err(error) => {
                        tracing::warn!("Worker exited for {identifier}: {error}");
                        self.schedule_issue_retry(RetryRequest {
                            issue_id,
                            attempt: next_attempt,
                            retry_kind: RetryKind::Failure,
                            identifier: Some(identifier),
                            error: Some(error),
                            worker_host,
                            workspace_path,
                        })
                        .await;
                    }
                }
            }
        }
        Ok(())
    }

    async fn integrate_codex_update(&self, issue_id: &str, update: CodexUpdate) {
        let mut state = self.state.lock().await;
        let rate_limits = extract_rate_limits(&update.payload);
        let token_delta = {
            let Some(entry) = state.running.get_mut(issue_id) else {
                return;
            };
            entry.last_codex_timestamp = Some(update.timestamp);
            entry.last_codex_message = Some(json!({
                "event": update.event,
                "message": update.payload,
                "timestamp": update.timestamp,
            }));
            entry.last_codex_event = Some(update.event.clone());
            if let Some(session_id) = update.session_id {
                let was_new = entry.session_id.as_deref() != Some(&session_id);
                entry.session_id = Some(session_id);
                if was_new {
                    entry.turn_count += 1;
                }
            }
            if let Some(pid) = update.codex_app_server_pid {
                entry.codex_app_server_pid = Some(pid);
            }
            extract_usage(&update.payload).map(|usage| apply_usage_update(entry, usage))
        };
        if let Some(delta) = token_delta {
            state.codex_totals.input_tokens += delta.input_tokens;
            state.codex_totals.output_tokens += delta.output_tokens;
            state.codex_totals.total_tokens += delta.total_tokens;
        }
        if let Some(rate_limits) = rate_limits {
            state.codex_rate_limits = Some(rate_limits);
        }
    }

    async fn record_runtime_seconds(&self, started_at: DateTime<Utc>) {
        let runtime_seconds = Utc::now()
            .signed_duration_since(started_at)
            .num_seconds()
            .max(0) as u64;
        let mut state = self.state.lock().await;
        state.codex_totals.seconds_running += runtime_seconds;
    }

    async fn handle_command(&mut self, command: OrchestratorCommand) -> Result<()> {
        match command {
            OrchestratorCommand::Snapshot { reply } => {
                let _ = reply.send(self.snapshot_locked().await);
            }
            OrchestratorCommand::RequestRefresh { reply } => {
                let payload = {
                    let mut state = self.state.lock().await;
                    let coalesced = state.poll_check_in_progress || state.refresh_pending;
                    state.refresh_pending = true;
                    state.next_poll_due_at_ms = Some(now_millis());
                    RefreshPayload {
                        queued: true,
                        coalesced,
                        requested_at: Utc::now(),
                        operations: vec!["poll".to_string(), "reconcile".to_string()],
                    }
                };
                let _ = reply.send(payload);
            }
            OrchestratorCommand::RetryFired { issue_id, token } => {
                let state = self.state.lock().await;
                if state
                    .retry_attempts
                    .get(&issue_id)
                    .is_some_and(|entry| entry.token == token)
                {
                    drop(state);
                    let mut state = self.state.lock().await;
                    state.next_poll_due_at_ms = Some(now_millis());
                }
            }
        }
        Ok(())
    }

    async fn snapshot_locked(&self) -> Snapshot {
        let state = self.state.lock().await;
        let now = Utc::now();
        let now_ms = now_millis();
        Snapshot {
            running: state
                .running
                .iter()
                .map(|(issue_id, entry)| RunningSnapshot {
                    issue_id: issue_id.clone(),
                    identifier: entry.identifier.clone(),
                    state: entry.issue.state.clone(),
                    worker_host: entry.worker_host.clone(),
                    workspace_path: entry.workspace_path.clone(),
                    session_id: entry.session_id.clone(),
                    codex_app_server_pid: entry.codex_app_server_pid.clone(),
                    codex_input_tokens: entry.codex_input_tokens,
                    codex_output_tokens: entry.codex_output_tokens,
                    codex_total_tokens: entry.codex_total_tokens,
                    turn_count: entry.turn_count,
                    started_at: entry.started_at,
                    last_codex_timestamp: entry.last_codex_timestamp,
                    last_codex_message: entry.last_codex_message.clone(),
                    last_codex_event: entry.last_codex_event.clone(),
                    runtime_seconds: now
                        .signed_duration_since(entry.started_at)
                        .num_seconds()
                        .max(0) as u64,
                })
                .collect(),
            retrying: state
                .retry_attempts
                .values()
                .map(|entry| RetrySnapshot {
                    issue_id: entry.issue_id.clone(),
                    attempt: entry.attempt,
                    due_in_ms: entry.due_at_ms.saturating_sub(now_ms),
                    identifier: entry.identifier.clone(),
                    error: entry.error.clone(),
                    worker_host: entry.worker_host.clone(),
                    workspace_path: entry.workspace_path.clone(),
                })
                .collect(),
            codex_totals: state.codex_totals.clone(),
            rate_limits: state.codex_rate_limits.clone(),
            polling: PollingSnapshot {
                checking: state.poll_check_in_progress,
                next_poll_in_ms: state
                    .next_poll_due_at_ms
                    .map(|due_at| due_at.saturating_sub(now_ms)),
                poll_interval_ms: state.poll_interval_ms,
            },
        }
    }
}

#[derive(Clone, Copy)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

#[derive(Clone, Copy, Default)]
struct TokenDelta {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

fn apply_usage_update(entry: &mut RunningEntry, usage: Usage) -> TokenDelta {
    let (input_delta, input_reported) =
        compute_token_delta(entry.codex_last_reported_input_tokens, usage.input_tokens);
    let (output_delta, output_reported) =
        compute_token_delta(entry.codex_last_reported_output_tokens, usage.output_tokens);
    let (total_delta, total_reported) =
        compute_token_delta(entry.codex_last_reported_total_tokens, usage.total_tokens);

    entry.codex_input_tokens += input_delta;
    entry.codex_output_tokens += output_delta;
    entry.codex_total_tokens += total_delta;
    entry.codex_last_reported_input_tokens = input_reported;
    entry.codex_last_reported_output_tokens = output_reported;
    entry.codex_last_reported_total_tokens = total_reported;

    TokenDelta {
        input_tokens: input_delta,
        output_tokens: output_delta,
        total_tokens: total_delta,
    }
}

fn compute_token_delta(previous_reported: u64, next_total: u64) -> (u64, u64) {
    if next_total >= previous_reported {
        (next_total - previous_reported, next_total)
    } else {
        (0, previous_reported)
    }
}

fn extract_usage(payload: &JsonValue) -> Option<Usage> {
    absolute_token_usage_from_payload(payload)
        .or_else(|| turn_completed_usage_from_payload(payload))
        .and_then(parse_usage)
}

fn as_u64(value: &JsonValue) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_i64()
                .filter(|value| *value >= 0)
                .map(|value| value as u64)
        })
        .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
}

fn absolute_token_usage_from_payload(payload: &JsonValue) -> Option<&JsonValue> {
    explicit_usage_at_paths(
        payload,
        &[
            "/params/msg/payload/info/total_token_usage",
            "/params/msg/info/total_token_usage",
            "/params/tokenUsage/total",
            "/tokenUsage/total",
        ],
    )
}

fn turn_completed_usage_from_payload(payload: &JsonValue) -> Option<&JsonValue> {
    let method = payload.get("method").and_then(JsonValue::as_str)?;
    if method != "turn/completed" {
        return None;
    }
    explicit_usage_at_paths(payload, &["/usage", "/params/usage"])
}

fn explicit_usage_at_paths<'a>(payload: &'a JsonValue, paths: &[&str]) -> Option<&'a JsonValue> {
    paths
        .iter()
        .filter_map(|path| payload.pointer(path))
        .find(|usage| parse_usage(usage).is_some())
}

fn parse_usage(usage: &JsonValue) -> Option<Usage> {
    Some(Usage {
        input_tokens: get_token_value(
            usage,
            &[
                "input_tokens",
                "prompt_tokens",
                "input",
                "promptTokens",
                "inputTokens",
            ],
        )?,
        output_tokens: get_token_value(
            usage,
            &[
                "output_tokens",
                "completion_tokens",
                "output",
                "completion",
                "outputTokens",
                "completionTokens",
            ],
        )?,
        total_tokens: get_token_value(usage, &["total_tokens", "total", "totalTokens"])?,
    })
}

fn get_token_value(usage: &JsonValue, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| usage.get(*name).and_then(as_u64))
}

fn extract_rate_limits(payload: &JsonValue) -> Option<JsonValue> {
    rate_limits_from_payload(payload)
}

fn rate_limits_from_payload(payload: &JsonValue) -> Option<JsonValue> {
    if let Some(direct) = payload.get("rate_limits")
        && rate_limits_map(direct)
    {
        return Some(direct.clone());
    }
    if rate_limits_map(payload) {
        return Some(payload.clone());
    }
    match payload {
        JsonValue::Object(map) => map.values().find_map(rate_limits_from_payload),
        JsonValue::Array(items) => items.iter().find_map(rate_limits_from_payload),
        _ => None,
    }
}

fn rate_limits_map(payload: &JsonValue) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    let has_limit_id = object.contains_key("limit_id") || object.contains_key("limit_name");
    let has_buckets = ["primary", "secondary", "credits"]
        .iter()
        .any(|bucket| object.contains_key(*bucket));
    has_limit_id && has_buckets
}

fn retry_delay_ms(retry_kind: RetryKind, attempt: u32, max_retry_backoff_ms: u64) -> u64 {
    if matches!(retry_kind, RetryKind::Continuation) && attempt == 1 {
        return CONTINUATION_RETRY_DELAY_MS;
    }
    let exponent = attempt.saturating_sub(1).min(10);
    FAILURE_RETRY_BASE_MS
        .saturating_mul(1_u64 << exponent)
        .min(max_retry_backoff_ms)
}

fn next_retry_attempt(attempt: Option<u32>) -> u32 {
    match attempt {
        Some(attempt) if attempt > 0 => attempt + 1,
        _ => 1,
    }
}

fn active_state(issue: &Issue, settings: &Settings) -> bool {
    settings
        .tracker
        .active_states
        .iter()
        .any(|state| normalize_issue_state(state) == normalize_issue_state(&issue.state))
}

fn issue_routable_to_worker(issue: &Issue) -> bool {
    issue.assigned_to_worker
}

fn retry_candidate_issue(issue: &Issue, settings: &Settings) -> bool {
    active_state(issue, settings)
        && !terminal_state(issue, settings)
        && issue_routable_to_worker(issue)
        && !todo_issue_blocked_by_non_terminal(issue, settings)
}

fn todo_issue_blocked_by_non_terminal(issue: &Issue, settings: &Settings) -> bool {
    normalize_issue_state(&issue.state) == "todo"
        && issue.blocked_by.iter().any(|blocker| {
            blocker
                .state
                .as_deref()
                .is_none_or(|state| !terminal_state_name(state, settings))
        })
}

fn terminal_state(issue: &Issue, settings: &Settings) -> bool {
    terminal_state_name(&issue.state, settings)
}

fn terminal_state_name(state_name: &str, settings: &Settings) -> bool {
    settings
        .tracker
        .terminal_states
        .iter()
        .any(|state| normalize_issue_state(state) == normalize_issue_state(state_name))
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn running_issue_count_for_state(
    running: &HashMap<String, RunningEntry>,
    issue_state: &str,
) -> usize {
    let normalized_state = normalize_issue_state(issue_state);
    running
        .values()
        .filter(|entry| normalize_issue_state(&entry.issue.state) == normalized_state)
        .count()
}

fn running_worker_host_count(running: &HashMap<String, RunningEntry>, worker_host: &str) -> usize {
    running
        .values()
        .filter(|entry| entry.worker_host.as_deref() == Some(worker_host))
        .count()
}

fn worker_host_slots_available(
    state: &OrchestratorState,
    worker_host: &str,
    max_per_host: Option<usize>,
) -> bool {
    match max_per_host {
        Some(limit) if limit > 0 => running_worker_host_count(&state.running, worker_host) < limit,
        _ => true,
    }
}

fn select_worker_host_for_state(
    state: &OrchestratorState,
    preferred_worker_host: Option<&str>,
    ssh_hosts: &[String],
    max_per_host: Option<usize>,
) -> WorkerSelection {
    if ssh_hosts.is_empty() {
        return WorkerSelection::Local;
    }

    let available_hosts = ssh_hosts
        .iter()
        .map(String::as_str)
        .filter(|host| worker_host_slots_available(state, host, max_per_host))
        .collect::<Vec<_>>();

    if available_hosts.is_empty() {
        return WorkerSelection::NoCapacity;
    }

    if let Some(preferred) = preferred_worker_host
        && available_hosts.contains(&preferred)
    {
        return WorkerSelection::Host(preferred.to_string());
    }

    let selected = available_hosts
        .into_iter()
        .enumerate()
        .min_by_key(|(index, host)| (running_worker_host_count(&state.running, host), *index))
        .map(|(_, host)| host)
        .expect("available_hosts is not empty");
    WorkerSelection::Host(selected.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::Issue;
    use serde_json::json;
    use std::collections::HashMap;

    fn issue() -> Issue {
        Issue {
            id: "issue-1".to_string(),
            identifier: "MT-1".to_string(),
            title: "Test".to_string(),
            description: None,
            priority: None,
            state: "In Progress".to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            assigned_to_worker: true,
            created_at: None,
            updated_at: None,
            assignee_id: None,
            assignee_email: None,
        }
    }

    fn running_entry(issue: Issue, worker_host: Option<&str>) -> RunningEntry {
        RunningEntry {
            identifier: issue.identifier.clone(),
            issue,
            started_at: Utc::now(),
            session_id: None,
            codex_app_server_pid: None,
            codex_input_tokens: 0,
            codex_output_tokens: 0,
            codex_total_tokens: 0,
            codex_last_reported_input_tokens: 0,
            codex_last_reported_output_tokens: 0,
            codex_last_reported_total_tokens: 0,
            turn_count: 0,
            last_codex_timestamp: None,
            last_codex_message: None,
            last_codex_event: None,
            runtime_seconds: 0,
            workspace_path: None,
            worker_host: worker_host.map(ToString::to_string),
            task: tokio::spawn(async {}),
            attempt: None,
        }
    }

    #[test]
    fn retry_delay_matches_continuation_and_failure_backoff() {
        assert_eq!(retry_delay_ms(RetryKind::Continuation, 1, 60_000), 1_000);
        assert_eq!(retry_delay_ms(RetryKind::Failure, 1, 60_000), 10_000);
        assert_eq!(retry_delay_ms(RetryKind::Failure, 2, 60_000), 20_000);
        assert_eq!(retry_delay_ms(RetryKind::Failure, 3, 60_000), 40_000);
        assert_eq!(retry_delay_ms(RetryKind::Failure, 4, 60_000), 60_000);
        assert_eq!(next_retry_attempt(None), 1);
        assert_eq!(next_retry_attempt(Some(1)), 2);
    }

    #[test]
    fn extract_usage_prefers_total_token_usage_and_maps_aliases() {
        let usage = extract_usage(&json!({
            "method": "codex/event/token_count",
            "params": {
                "msg": {
                    "type": "event_msg",
                    "payload": {
                        "type": "token_count",
                        "info": {
                            "last_token_usage": {
                                "input_tokens": 2,
                                "output_tokens": 1,
                                "total_tokens": 3
                            },
                            "total_token_usage": {
                                "prompt_tokens": "10",
                                "completion_tokens": 5,
                                "total_tokens": 15
                            }
                        }
                    }
                }
            }
        }))
        .unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn extract_usage_ignores_last_token_usage_without_cumulative_totals() {
        assert!(
            extract_usage(&json!({
                "method": "codex/event/token_count",
                "params": {
                    "msg": {
                        "type": "event_msg",
                        "payload": {
                            "type": "token_count",
                            "info": {
                                "last_token_usage": {
                                    "input_tokens": 8,
                                    "output_tokens": 3,
                                    "total_tokens": 11
                                }
                            }
                        }
                    }
                }
            }))
            .is_none()
        );
    }

    #[test]
    fn extract_usage_reads_thread_totals_and_turn_completed_payloads() {
        let thread_usage = extract_usage(&json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "tokenUsage": {
                    "total": {
                        "inputTokens": 12,
                        "outputTokens": 4,
                        "totalTokens": 16
                    }
                }
            }
        }))
        .unwrap();
        assert_eq!(thread_usage.input_tokens, 12);
        assert_eq!(thread_usage.output_tokens, 4);
        assert_eq!(thread_usage.total_tokens, 16);

        let turn_usage = extract_usage(&json!({
            "method": "turn/completed",
            "usage": {
                "input_tokens": "12",
                "output_tokens": 4,
                "total_tokens": 16
            }
        }))
        .unwrap();
        assert_eq!(turn_usage.input_tokens, 12);
        assert_eq!(turn_usage.output_tokens, 4);
        assert_eq!(turn_usage.total_tokens, 16);
    }

    #[test]
    fn extract_rate_limits_finds_nested_rate_limit_payloads() {
        let rate_limits = extract_rate_limits(&json!({
            "method": "codex/event/token_count",
            "params": {
                "msg": {
                    "type": "event_msg",
                    "payload": {
                        "type": "token_count",
                        "rate_limits": {
                            "limit_id": "codex",
                            "primary": { "remaining": 90, "limit": 100 },
                            "secondary": null,
                            "credits": { "has_credits": false }
                        }
                    }
                }
            }
        }))
        .unwrap();
        assert_eq!(rate_limits["limit_id"], "codex");
        assert_eq!(rate_limits["primary"]["remaining"], 90);
    }

    #[tokio::test]
    async fn apply_usage_update_accumulates_monotonic_thread_totals() {
        let task = tokio::spawn(async {});
        let mut entry = RunningEntry {
            identifier: "MT-1".to_string(),
            issue: issue(),
            started_at: Utc::now(),
            session_id: None,
            codex_app_server_pid: None,
            codex_input_tokens: 0,
            codex_output_tokens: 0,
            codex_total_tokens: 0,
            codex_last_reported_input_tokens: 0,
            codex_last_reported_output_tokens: 0,
            codex_last_reported_total_tokens: 0,
            turn_count: 0,
            last_codex_timestamp: None,
            last_codex_message: None,
            last_codex_event: None,
            runtime_seconds: 0,
            workspace_path: None,
            worker_host: None,
            task,
            attempt: None,
        };

        let first = apply_usage_update(
            &mut entry,
            Usage {
                input_tokens: 8,
                output_tokens: 3,
                total_tokens: 11,
            },
        );
        assert_eq!(first.input_tokens, 8);
        assert_eq!(first.output_tokens, 3);
        assert_eq!(first.total_tokens, 11);

        let second = apply_usage_update(
            &mut entry,
            Usage {
                input_tokens: 10,
                output_tokens: 4,
                total_tokens: 14,
            },
        );
        assert_eq!(second.input_tokens, 2);
        assert_eq!(second.output_tokens, 1);
        assert_eq!(second.total_tokens, 3);
        assert_eq!(entry.codex_input_tokens, 10);
        assert_eq!(entry.codex_output_tokens, 4);
        assert_eq!(entry.codex_total_tokens, 14);
    }

    #[tokio::test]
    async fn select_worker_host_prefers_preferred_host_when_capacity_exists() {
        let mut running = HashMap::new();
        running.insert(
            "issue-1".to_string(),
            running_entry(issue(), Some("host-a")),
        );
        let state = OrchestratorState {
            running,
            ..OrchestratorState::default()
        };
        let ssh_hosts = vec!["host-a".to_string(), "host-b".to_string()];
        let selection = select_worker_host_for_state(&state, Some("host-a"), &ssh_hosts, Some(2));
        assert_eq!(selection, WorkerSelection::Host("host-a".to_string()));
    }

    #[tokio::test]
    async fn select_worker_host_chooses_least_loaded_available_host() {
        let mut running = HashMap::new();
        for (issue_id, host) in [
            ("issue-1", "host-a"),
            ("issue-2", "host-a"),
            ("issue-3", "host-b"),
        ] {
            let mut issue = issue();
            issue.identifier = issue_id.to_string();
            running.insert(issue_id.to_string(), running_entry(issue, Some(host)));
        }
        let state = OrchestratorState {
            running,
            ..OrchestratorState::default()
        };
        let ssh_hosts = vec![
            "host-a".to_string(),
            "host-b".to_string(),
            "host-c".to_string(),
        ];
        let selection = select_worker_host_for_state(&state, None, &ssh_hosts, Some(3));
        assert_eq!(selection, WorkerSelection::Host("host-c".to_string()));
    }

    #[tokio::test]
    async fn select_worker_host_reports_no_capacity_when_all_hosts_are_full() {
        let mut running = HashMap::new();
        for (issue_id, host) in [("issue-1", "host-a"), ("issue-2", "host-b")] {
            let mut issue = issue();
            issue.identifier = issue_id.to_string();
            running.insert(issue_id.to_string(), running_entry(issue, Some(host)));
        }
        let state = OrchestratorState {
            running,
            ..OrchestratorState::default()
        };
        let ssh_hosts = vec!["host-a".to_string(), "host-b".to_string()];
        let selection = select_worker_host_for_state(&state, None, &ssh_hosts, Some(1));
        assert_eq!(selection, WorkerSelection::NoCapacity);
    }

    #[test]
    fn retry_candidate_issue_requires_routable_and_unblocked_active_issue() {
        let settings = Settings::from_workflow(
            &crate::workflow::LoadedWorkflow {
                config: serde_yaml::from_str(
                    "tracker:\n  kind: memory\n  active_states: [Todo, In Progress]\n  terminal_states: [Done]\n",
                )
                .unwrap(),
                prompt_template: String::new(),
                prompt: String::new(),
            },
            &crate::config::CliOverrides::default(),
        )
        .unwrap();

        let mut blocked = issue();
        blocked.state = "Todo".to_string();
        blocked.blocked_by = vec![crate::tracker::BlockerRef {
            id: Some("issue-2".to_string()),
            identifier: Some("MT-2".to_string()),
            state: Some("In Progress".to_string()),
        }];
        assert!(!retry_candidate_issue(&blocked, &settings));

        let mut unroutable = issue();
        unroutable.assigned_to_worker = false;
        assert!(!retry_candidate_issue(&unroutable, &settings));

        let mut valid = issue();
        valid.state = "In Progress".to_string();
        assert!(retry_candidate_issue(&valid, &settings));
    }
}
