use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::config::{CliOverrides, RefreshPayload};
use crate::config_store::ConfigStore;
use crate::orchestrator::{OrchestratorHandle, Snapshot};
use crate::presenter::{self, SnapshotError};

#[derive(Clone)]
pub struct HttpState {
    backend: Arc<dyn ObservabilityBackend>,
    config_store: ConfigStore,
    snapshot_timeout_ms: u64,
}

#[async_trait]
pub trait ObservabilityBackend: Send + Sync {
    async fn snapshot(&self) -> Result<Snapshot>;
    async fn request_refresh(&self) -> Result<RefreshPayload>;
}

#[derive(Clone)]
struct OrchestratorBackend {
    orchestrator: OrchestratorHandle,
}

#[async_trait]
impl ObservabilityBackend for OrchestratorBackend {
    async fn snapshot(&self) -> Result<Snapshot> {
        self.orchestrator.snapshot().await
    }

    async fn request_refresh(&self) -> Result<RefreshPayload> {
        self.orchestrator.request_refresh().await
    }
}

pub async fn serve(
    orchestrator: OrchestratorHandle,
    config_store: ConfigStore,
    _workflow_store: crate::workflow_store::WorkflowStore,
    _overrides: CliOverrides,
) -> Result<()> {
    let settings = config_store.current_settings().await;
    let Some(port) = settings.server.port else {
        return Ok(());
    };
    let state = HttpState {
        backend: Arc::new(OrchestratorBackend { orchestrator }),
        config_store,
        snapshot_timeout_ms: 15_000,
    };
    let app = router(state);
    let listener = tokio::net::TcpListener::bind((settings.server.host.as_str(), port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/", get(dashboard).fallback(method_not_allowed))
        .route("/dashboard.css", get(styles))
        .route("/vendor/phoenix_html/phoenix_html.js", get(phoenix_html_js))
        .route("/vendor/phoenix/phoenix.js", get(phoenix_js))
        .route(
            "/vendor/phoenix_live_view/phoenix_live_view.js",
            get(phoenix_live_view_js),
        )
        .route(
            "/api/v1/state",
            get(state_route).fallback(method_not_allowed),
        )
        .route(
            "/api/v1/refresh",
            post(refresh).fallback(method_not_allowed),
        )
        .route(
            "/api/v1/{issue_identifier}",
            get(issue_route).fallback(method_not_allowed),
        )
        .fallback(not_found)
        .with_state(Arc::new(state))
}

async fn dashboard(State(state): State<Arc<HttpState>>) -> impl IntoResponse {
    let settings = state.config_store.current_settings().await;
    match snapshot_with_timeout(&state).await {
        Ok(snapshot) => Html(presenter::render_dashboard_html(&snapshot, &settings)),
        Err(SnapshotError::Timeout) => {
            Html("<html><body><h1>Symphony</h1><p>snapshot_timeout</p></body></html>".to_string())
        }
        Err(_) => Html(
            "<html><body><h1>Symphony</h1><p>snapshot_unavailable</p></body></html>".to_string(),
        ),
    }
}

async fn styles() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "text/css; charset=utf-8")],
        r#"
:root {
  color-scheme: light;
  --page: #f5f6f7;
  --page-soft: #fbfbfc;
  --page-deep: #e9ebef;
  --card: rgba(255, 255, 255, 0.94);
  --card-muted: #f3f4f6;
  --ink: #252930;
  --muted: #717887;
  --line: #e7e9ef;
  --line-strong: #d6dae1;
  --accent: #4b7f8a;
  --accent-ink: #365f68;
  --accent-soft: #e8f3f4;
  --danger: #b42318;
  --danger-soft: #fef3f2;
  --shadow-sm: 0 1px 2px rgba(16, 24, 40, 0.05);
  --shadow-lg: 0 20px 50px rgba(15, 23, 42, 0.08);
  --terminal-bg: #2a2d34;
  --terminal-line: #505662;
  --terminal-ink: #f3f4f6;
  --terminal-muted: #8f97a5;
  --terminal-cyan: #99bbc4;
  --terminal-green: #a8c7bb;
  --terminal-magenta: #b8a7cf;
  --terminal-yellow: #ccb67c;
}

* { box-sizing: border-box; }
html { background: var(--page); }
body {
  margin: 0;
  min-height: 100vh;
  background:
    radial-gradient(circle at top, rgba(75, 127, 138, 0.11) 0%, rgba(75, 127, 138, 0) 30%),
    linear-gradient(180deg, var(--page-soft) 0%, var(--page) 26%, #eef1f4 100%);
  color: var(--ink);
  font-family: "Sohne", "SF Pro Text", "Helvetica Neue", "Segoe UI", sans-serif;
  line-height: 1.42;
}

a {
  color: var(--ink);
  text-decoration: none;
  transition: color 140ms ease;
}

a:hover { color: var(--accent); }

button {
  appearance: none;
  border: 1px solid var(--accent);
  background: var(--accent);
  color: white;
  border-radius: 999px;
  padding: 0.72rem 1.08rem;
  cursor: pointer;
  font: inherit;
  font-weight: 600;
  letter-spacing: -0.01em;
  box-shadow: 0 8px 20px rgba(16, 163, 127, 0.18);
  transition: transform 140ms ease, box-shadow 140ms ease, background 140ms ease, border-color 140ms ease;
}

button:hover {
  transform: translateY(-1px);
  box-shadow: 0 12px 24px rgba(16, 163, 127, 0.22);
}

.subtle-button {
  border: 1px solid var(--line-strong);
  background: rgba(255, 255, 255, 0.72);
  color: var(--muted);
  padding: 0.34rem 0.72rem;
  font-size: 0.82rem;
  box-shadow: none;
}

.subtle-button:hover {
  transform: none;
  box-shadow: none;
  background: white;
  border-color: var(--muted);
  color: var(--ink);
}

pre { margin: 0; white-space: pre-wrap; word-break: break-word; }
code, pre, .mono {
  font-family: "Sohne Mono", "SFMono-Regular", "SF Mono", Consolas, "Liberation Mono", monospace;
}

.mono, .numeric {
  font-variant-numeric: tabular-nums slashed-zero;
  font-feature-settings: "tnum" 1, "zero" 1;
}

.app-shell {
  max-width: 1280px;
  margin: 0 auto;
  padding: 2rem 1rem 3.5rem;
}

.dashboard-shell { display: grid; gap: 1rem; }

.hero-card, .section-card, .metric-card, .error-card {
  background: var(--card);
  border: 1px solid rgba(217, 217, 227, 0.82);
  box-shadow: var(--shadow-sm);
  backdrop-filter: blur(18px);
}

.hero-card {
  border-radius: 28px;
  padding: clamp(1.25rem, 3vw, 2rem);
  box-shadow: var(--shadow-lg);
}

.hero-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 1.25rem;
  align-items: start;
}

.eyebrow {
  margin: 0;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.08em;
  font-size: 0.76rem;
  font-weight: 600;
}

.hero-title {
  margin: 0.35rem 0 0;
  font-size: clamp(1.85rem, 4vw, 2.85rem);
  line-height: 0.98;
  letter-spacing: -0.04em;
}

.hero-copy {
  margin: 0.75rem 0 0;
  max-width: 42rem;
  color: var(--muted);
  font-size: 0.96rem;
}

.status-stack {
  display: grid;
  justify-items: end;
  align-content: start;
  min-width: min(100%, 9rem);
}

.status-badge {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  min-height: 2rem;
  padding: 0.35rem 0.78rem;
  border-radius: 999px;
  border: 1px solid var(--line);
  background: var(--card-muted);
  color: var(--muted);
  font-size: 0.82rem;
  font-weight: 700;
  letter-spacing: 0.01em;
}

.status-badge-dot {
  width: 0.52rem;
  height: 0.52rem;
  border-radius: 999px;
  background: currentColor;
  opacity: 0.9;
}

.status-badge-live {
  display: none;
  background: var(--accent-soft);
  border-color: rgba(75, 127, 138, 0.18);
  color: var(--accent-ink);
}

.status-badge-offline {
  background: #f5f5f7;
  border-color: var(--line-strong);
  color: var(--muted);
}

[data-phx-main].phx-connected .status-badge-live { display: inline-flex; }
[data-phx-main].phx-connected .status-badge-offline { display: none; }

.metric-grid {
  display: grid;
  gap: 0.72rem;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
}

.metric-card {
  border-radius: 20px;
  padding: 0.88rem 0.95rem 0.95rem;
}

.metric-label {
  margin: 0;
  color: var(--muted);
  font-size: 0.82rem;
  font-weight: 600;
  letter-spacing: 0.01em;
}

.metric-value {
  margin: 0.3rem 0 0;
  font-size: clamp(1.45rem, 2vw, 1.95rem);
  line-height: 1.05;
  letter-spacing: -0.03em;
}

.metric-detail {
  margin: 0.38rem 0 0;
  color: var(--muted);
  font-size: 0.84rem;
}

.section-card {
  border-radius: 20px;
  padding: 0.9rem;
}

.section-header {
  display: flex;
  justify-content: space-between;
  gap: 1rem;
  align-items: start;
  margin-bottom: 0.72rem;
}

.section-title {
  margin: 0;
  font-size: 0.95rem;
  letter-spacing: -0.015em;
}

.section-copy {
  margin: 0.35rem 0 0;
  color: var(--muted);
  font-size: 0.86rem;
}

.terminal-frame {
  border-radius: 20px;
  background: linear-gradient(180deg, rgba(255,255,255,0.04), rgba(255,255,255,0.01)), var(--terminal-bg);
  border: 1px solid rgba(255,255,255,0.03);
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.035), 0 14px 28px rgba(15, 23, 42, 0.16);
  overflow: auto;
}

.terminal-dashboard {
  min-width: max-content;
  padding: 0.95rem 1rem 1.05rem;
  color: var(--terminal-ink);
  line-height: 1.22;
  font-size: 0.91rem;
  white-space: pre;
}

.terminal-dashboard .term-strong {
  color: var(--terminal-ink);
  font-weight: 700;
}

.terminal-dashboard .term-muted {
  color: var(--terminal-muted);
}

.terminal-dashboard .term-dim {
  color: var(--terminal-muted);
  opacity: 0.9;
}

.terminal-dashboard .term-cyan {
  color: var(--terminal-cyan);
}

.terminal-dashboard .term-green {
  color: var(--terminal-green);
}

.terminal-dashboard .term-yellow,
.terminal-dashboard .term-orange {
  color: var(--terminal-yellow);
}

.terminal-dashboard .term-magenta {
  color: var(--terminal-magenta);
}

.terminal-dashboard .term-blue {
  color: #8fa8c3;
}

.terminal-dashboard .term-red {
  color: #cb9aa4;
}

.code-panel {
  border-radius: 16px;
  padding: 0.9rem;
  background: var(--card-muted);
  border: 1px solid var(--line);
  color: var(--ink);
}

.table-wrap {
  overflow-x: auto;
  border-radius: 16px;
  border: 1px solid var(--line);
}

.data-table {
  width: 100%;
  min-width: 760px;
  border-collapse: collapse;
  background: white;
}

.data-table th, .data-table td {
  text-align: left;
  vertical-align: top;
  padding: 0.72rem 0.82rem;
  border-bottom: 1px solid var(--line);
}

.data-table th {
  color: var(--muted);
  font-size: 0.72rem;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  background: rgba(243, 244, 246, 0.55);
}

.data-table tbody tr:last-child td { border-bottom: none; }

.issue-stack, .detail-stack, .token-stack, .session-stack {
  display: grid;
  gap: 0.14rem;
}

.session-stack { justify-items: start; }

.issue-id {
  font-weight: 700;
  letter-spacing: -0.01em;
}

.issue-link {
  color: var(--muted);
  font-size: 0.8rem;
}

.muted {
  color: var(--muted);
}

.event-text {
  color: var(--ink);
  display: inline-block;
  max-width: 34rem;
}

.event-meta {
  font-size: 0.78rem;
}

.state-badge {
  display: inline-flex;
  align-items: center;
  border-radius: 999px;
  padding: 0.22rem 0.58rem;
  font-size: 0.78rem;
  font-weight: 700;
  white-space: nowrap;
}

.state-badge--todo {
  background: #eef2f5;
  color: #5c6774;
}

.state-badge--active {
  background: rgba(75, 127, 138, 0.14);
  color: var(--accent-ink);
}

.state-badge--rework {
  background: rgba(204, 182, 124, 0.2);
  color: #6d5923;
}

.state-badge--done {
  background: rgba(102, 112, 133, 0.12);
  color: #485467;
}

.state-badge--neutral {
  background: #eef2f5;
  color: #5c6774;
}

.running-col-issue {
  color: #93b9c1;
}

.running-col-state {
  color: #a5c3b8;
}

.running-col-session {
  color: #90b7c0;
}

.running-col-runtime {
  color: #b2a4ce;
}

.running-col-event {
  color: var(--terminal-ink);
}

.running-col-tokens {
  color: #cab77d;
}

.running-col .issue-id,
.running-col .event-text,
.running-col .token-stack,
.running-col .session-stack {
  color: inherit;
}

.empty-state {
  margin: 0;
  color: var(--muted);
  padding: 0.25rem 0.1rem 0.1rem;
}

@media (max-width: 860px) {
  .hero-grid { grid-template-columns: 1fr; }
  .status-stack { justify-items: start; }
  .app-shell { padding: 1rem 0.75rem 2rem; }
  .section-card, .metric-card, .hero-card { border-radius: 18px; }
}
"#
        .to_string(),
    )
}

async fn phoenix_html_js() -> impl IntoResponse {
    javascript_response("var phoenix = { link: { click() {} } }; phoenix.link.click();")
}

async fn phoenix_js() -> impl IntoResponse {
    javascript_response("var Phoenix = (() => { return {}; })();")
}

async fn phoenix_live_view_js() -> impl IntoResponse {
    javascript_response("var LiveView = (() => { return {}; })();")
}

async fn state_route(State(state): State<Arc<HttpState>>) -> impl IntoResponse {
    Json(presenter::state_payload(
        snapshot_with_timeout(&state)
            .await
            .as_ref()
            .map_err(Clone::clone),
    ))
}

async fn refresh(State(state): State<Arc<HttpState>>) -> Response {
    match state.backend.request_refresh().await {
        Ok(payload) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "queued": payload.queued,
                "coalesced": payload.coalesced,
                "requested_at": payload.requested_at.to_rfc3339(),
                "operations": payload.operations
            })),
        )
            .into_response(),
        Err(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "orchestrator_unavailable",
            "Orchestrator is unavailable",
        ),
    }
}

async fn issue_route(
    Path(issue_identifier): Path<String>,
    State(state): State<Arc<HttpState>>,
) -> Response {
    let settings = Some(state.config_store.current_settings().await);

    match snapshot_with_timeout(&state).await {
        Ok(snapshot) => {
            match presenter::issue_payload(&snapshot, &issue_identifier, settings.as_ref()) {
                Some(payload) => Json(payload).into_response(),
                None => error_response(StatusCode::NOT_FOUND, "issue_not_found", "Issue not found"),
            }
        }
        Err(_) => error_response(StatusCode::NOT_FOUND, "issue_not_found", "Issue not found"),
    }
}

async fn method_not_allowed(method: Method) -> Response {
    let _ = method;
    error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "Method not allowed",
    )
}

async fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "Route not found")
}

fn javascript_response(source: &str) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/javascript; charset=utf-8")],
        source.to_string(),
    )
        .into_response()
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

async fn snapshot_with_timeout(state: &HttpState) -> std::result::Result<Snapshot, SnapshotError> {
    match tokio::time::timeout(
        Duration::from_millis(state.snapshot_timeout_ms),
        state.backend.snapshot(),
    )
    .await
    {
        Ok(Ok(snapshot)) => Ok(snapshot),
        Ok(Err(_)) => Err(SnapshotError::Unavailable),
        Err(_) => Err(SnapshotError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use serde_json::Value as JsonValue;
    use tokio::time::sleep;
    use tower::util::ServiceExt;

    use crate::config::CliOverrides;
    use crate::config_store::ConfigStore;
    use crate::orchestrator::{
        PollingSnapshot, RetrySnapshot, RunningSnapshot, Snapshot, TokenTotals,
    };

    #[derive(Clone)]
    struct MockBackend {
        snapshot: std::result::Result<Snapshot, String>,
        refresh: std::result::Result<RefreshPayload, String>,
        snapshot_delay_ms: u64,
    }

    #[async_trait]
    impl ObservabilityBackend for MockBackend {
        async fn snapshot(&self) -> Result<Snapshot> {
            if self.snapshot_delay_ms > 0 {
                sleep(Duration::from_millis(self.snapshot_delay_ms)).await;
            }
            self.snapshot.clone().map_err(|error| anyhow!(error))
        }

        async fn request_refresh(&self) -> Result<RefreshPayload> {
            self.refresh.clone().map_err(|error| anyhow!(error))
        }
    }

    async fn test_state(backend: Arc<dyn ObservabilityBackend>) -> HttpState {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let workflow_dir = std::env::temp_dir().join(format!(
            "symphony-http-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&workflow_dir).unwrap();
        let workflow_path = workflow_dir.join("WORKFLOW.md");
        let config_path = workflow_dir.join(".symphony.toml");
        std::fs::write(&workflow_path, "Prompt body\n").unwrap();
        std::fs::write(
            &config_path,
            "[tracker]\nkind = \"memory\"\n[workspace]\nroot = \"/tmp/symphony-http\"\n",
        )
        .unwrap();
        let config_store = ConfigStore::new(config_path, workflow_path, CliOverrides::default())
            .await
            .unwrap();
        HttpState {
            backend,
            config_store,
            snapshot_timeout_ms: 5,
        }
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            running: vec![RunningSnapshot {
                issue_id: "issue-http".to_string(),
                identifier: "MT-HTTP".to_string(),
                state: "In Progress".to_string(),
                worker_host: None,
                workspace_path: None,
                session_id: Some("thread-http".to_string()),
                codex_app_server_pid: None,
                codex_input_tokens: 4,
                codex_output_tokens: 8,
                codex_total_tokens: 12,
                turn_count: 7,
                started_at: chrono::Utc::now(),
                last_codex_timestamp: None,
                last_codex_message: Some(JsonValue::String("rendered".to_string())),
                last_codex_event: Some("notification".to_string()),
                runtime_seconds: 42,
            }],
            retrying: vec![RetrySnapshot {
                issue_id: "issue-retry".to_string(),
                attempt: 2,
                due_in_ms: 5_000,
                identifier: Some("MT-RETRY".to_string()),
                error: Some("boom".to_string()),
                worker_host: None,
                workspace_path: None,
            }],
            codex_totals: TokenTotals {
                input_tokens: 4,
                output_tokens: 8,
                total_tokens: 12,
                seconds_running: 42,
            },
            rate_limits: Some(json!({ "primary": { "remaining": 11 } })),
            polling: PollingSnapshot {
                checking: false,
                next_poll_in_ms: Some(5_000),
                poll_interval_ms: 30_000,
            },
        }
    }

    fn assert_terminal_contract_fragments(body: &str) {
        assert!(body.contains("SYMPHONY STATUS"));
        assert!(body.contains("ID"));
        assert!(body.contains("STAGE"));
        assert!(body.contains("AGE / TURN"));
        assert!(body.contains("TOKENS"));
        assert!(body.contains("SESSION"));
        assert!(body.contains("EVENT"));
        assert!(body.contains("Backoff queue"));
    }

    #[tokio::test]
    async fn api_state_issue_and_refresh_payloads_match_contract() {
        let state = test_state(Arc::new(MockBackend {
            snapshot: Ok(sample_snapshot()),
            refresh: Ok(RefreshPayload {
                queued: true,
                coalesced: false,
                requested_at: chrono::Utc::now(),
                operations: vec!["poll".to_string(), "reconcile".to_string()],
            }),
            snapshot_delay_ms: 0,
        }))
        .await;
        let app = router(state);

        let state_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(state_response.status(), StatusCode::OK);

        let issue_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/MT-HTTP")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(issue_response.status(), StatusCode::OK);

        let refresh_response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(refresh_response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn api_preserves_method_not_allowed_unavailable_and_timeout_behavior() {
        let unavailable = test_state(Arc::new(MockBackend {
            snapshot: Err("unavailable".to_string()),
            refresh: Err("unavailable".to_string()),
            snapshot_delay_ms: 0,
        }))
        .await;
        let timeout = test_state(Arc::new(MockBackend {
            snapshot: Ok(sample_snapshot()),
            refresh: Ok(RefreshPayload {
                queued: true,
                coalesced: false,
                requested_at: chrono::Utc::now(),
                operations: vec!["poll".to_string()],
            }),
            snapshot_delay_ms: 25,
        }))
        .await;

        let unavailable_app = router(unavailable);
        let timeout_app = router(timeout);

        assert_eq!(
            unavailable_app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/v1/state")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );

        assert_eq!(
            unavailable_app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/unknown")
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );

        assert_eq!(
            unavailable_app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/v1/refresh")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        let unavailable_dashboard = unavailable_app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unavailable_dashboard.status(), StatusCode::OK);
        let unavailable_dashboard_body =
            to_bytes(unavailable_dashboard.into_body(), usize::MAX).await.unwrap();
        let unavailable_dashboard_body =
            String::from_utf8(unavailable_dashboard_body.to_vec()).unwrap();
        assert!(unavailable_dashboard_body.contains("snapshot_unavailable"));
        assert!(unavailable_dashboard_body.contains("Symphony"));

        let timeout_response = timeout_app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(timeout_response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn dashboard_and_static_assets_exist() {
        let state = test_state(Arc::new(MockBackend {
            snapshot: Ok(sample_snapshot()),
            refresh: Ok(RefreshPayload {
                queued: true,
                coalesced: false,
                requested_at: chrono::Utc::now(),
                operations: vec!["poll".to_string()],
            }),
            snapshot_delay_ms: 0,
        }))
        .await;
        let app = router(state);

        for path in [
            "/dashboard.css",
            "/vendor/phoenix_html/phoenix_html.js",
            "/vendor/phoenix/phoenix.js",
            "/vendor/phoenix_live_view/phoenix_live_view.js",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }

        let dashboard_response = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(dashboard_response.status(), StatusCode::OK, "/");
        let dashboard_body = to_bytes(dashboard_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let dashboard_body = String::from_utf8(dashboard_body.to_vec()).unwrap();
        assert_terminal_contract_fragments(&dashboard_body);
    }

    #[tokio::test]
    async fn unknown_routes_return_404() {
        let state = test_state(Arc::new(MockBackend {
            snapshot: Ok(sample_snapshot()),
            refresh: Ok(RefreshPayload {
                queued: true,
                coalesced: false,
                requested_at: chrono::Utc::now(),
                operations: vec!["poll".to_string()],
            }),
            snapshot_delay_ms: 0,
        }))
        .await;
        let app = router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
