use anyhow::Result;
use tokio::sync::mpsc;

use crate::codex::{self, CodexUpdate};
use crate::config::{Settings, default_prompt_template, render_prompt};
use crate::tracker::{Issue, Tracker};
use crate::workspace;

#[derive(Clone, Debug)]
pub struct WorkerRuntimeInfo {
    pub worker_host: Option<String>,
    pub workspace_path: String,
}

#[derive(Clone, Debug)]
pub enum WorkerEvent {
    RuntimeInfo {
        issue_id: String,
        runtime: WorkerRuntimeInfo,
    },
    CodexUpdate {
        issue_id: String,
        update: CodexUpdate,
    },
    Exit {
        issue_id: String,
        reason: Result<(), String>,
    },
}

pub async fn run(
    issue: Issue,
    settings: Settings,
    workflow_prompt: String,
    tracker: std::sync::Arc<dyn Tracker>,
    worker_events: mpsc::Sender<WorkerEvent>,
    worker_host: Option<String>,
    starting_attempt: Option<u32>,
) {
    let result = run_inner(
        issue.clone(),
        settings,
        workflow_prompt,
        tracker,
        worker_events.clone(),
        worker_host.clone(),
        starting_attempt,
    )
    .await;

    let _ = worker_events
        .send(WorkerEvent::Exit {
            issue_id: issue.id,
            reason: result.map_err(|error| error.to_string()),
        })
        .await;
}

async fn run_inner(
    issue: Issue,
    settings: Settings,
    workflow_prompt: String,
    tracker: std::sync::Arc<dyn Tracker>,
    worker_events: mpsc::Sender<WorkerEvent>,
    worker_host: Option<String>,
    starting_attempt: Option<u32>,
) -> Result<()> {
    let workspace =
        workspace::create_for_issue(&issue.identifier, &settings, worker_host.as_deref()).await?;
    let _ = worker_events
        .send(WorkerEvent::RuntimeInfo {
            issue_id: issue.id.clone(),
            runtime: WorkerRuntimeInfo {
                worker_host: worker_host.clone(),
                workspace_path: workspace.path.to_string_lossy().to_string(),
            },
        })
        .await;

    workspace::run_before_run_hook(&workspace, &issue.identifier, &settings).await?;

    let mut codex_session = codex::start_session(
        &workspace.path.to_string_lossy(),
        worker_host.as_deref(),
        &settings,
    )
    .await?;
    let (updates_tx, mut updates_rx) = mpsc::channel(64);
    let events_tx = worker_events.clone();
    let issue_id = issue.id.clone();
    tokio::spawn(async move {
        while let Some(update) = updates_rx.recv().await {
            let _ = events_tx
                .send(WorkerEvent::CodexUpdate {
                    issue_id: issue_id.clone(),
                    update,
                })
                .await;
        }
    });

    let mut current_issue = issue;
    let mut turn_number = 1usize;
    let mut attempt = starting_attempt;
    loop {
        let prompt = if turn_number == 1 {
            let template = if workflow_prompt.trim().is_empty() {
                default_prompt_template()
            } else {
                workflow_prompt.clone()
            };
            render_prompt(&template, &current_issue, attempt)?
        } else {
            format!(
                "Continuation guidance:\n\n- The previous Codex turn completed normally, but the Linear issue is still in an active state.\n- This is continuation turn #{turn_number} of {} for the current agent run.\n- Resume from the current workspace and workpad state instead of restarting from scratch.\n- The original task instructions and prior turn context are already present in this thread, so do not restate them before acting.\n- Focus on the remaining ticket work and do not end the turn while the issue stays active unless you are truly blocked.",
                settings.agent.max_turns
            )
        };

        codex::run_turn(
            &mut codex_session,
            &prompt,
            &current_issue,
            &settings,
            &updates_tx,
        )
        .await?;
        if turn_number >= settings.agent.max_turns {
            break;
        }

        let refreshed = tracker
            .fetch_issue_states_by_ids(std::slice::from_ref(&current_issue.id), &settings)
            .await?;
        let Some(refreshed_issue) = refreshed.into_iter().next() else {
            break;
        };
        if !settings.tracker.active_states.iter().any(|state| {
            crate::config::normalize_issue_state(state)
                == crate::config::normalize_issue_state(&refreshed_issue.state)
        }) {
            break;
        }
        current_issue = refreshed_issue;
        turn_number += 1;
        attempt = Some(attempt.unwrap_or(0) + 1);
    }

    codex::stop_session(codex_session).await?;
    workspace::run_after_run_hook(&workspace, &current_issue.identifier, &settings).await;
    Ok(())
}
