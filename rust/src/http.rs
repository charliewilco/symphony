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

use crate::config::{CliOverrides, RefreshPayload, Settings};
use crate::orchestrator::{OrchestratorHandle, Snapshot};
use crate::presenter::{self, SnapshotError};
use crate::workflow_store::WorkflowStore;

#[derive(Clone)]
pub struct HttpState {
    backend: Arc<dyn ObservabilityBackend>,
    workflow_store: WorkflowStore,
    overrides: CliOverrides,
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
    workflow_store: WorkflowStore,
    overrides: CliOverrides,
) -> Result<()> {
    let settings = Settings::from_workflow(&workflow_store.current().await, &overrides)?;
    let Some(port) = settings.server.port else {
        return Ok(());
    };
    let state = HttpState {
        backend: Arc::new(OrchestratorBackend { orchestrator }),
        workflow_store,
        overrides,
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
    let settings = Settings::from_workflow(&state.workflow_store.current().await, &state.overrides)
        .map_err(|_| SnapshotError::Unavailable);
    match (snapshot_with_timeout(&state).await, settings) {
        (Ok(snapshot), Ok(settings)) => {
            Html(presenter::render_dashboard_html(&snapshot, &settings))
        }
        (Err(SnapshotError::Timeout), _) => {
            Html("<html><body><h1>Symphony</h1><p>snapshot_timeout</p></body></html>".to_string())
        }
        _ => Html(
            "<html><body><h1>Symphony</h1><p>snapshot_unavailable</p></body></html>".to_string(),
        ),
    }
}

async fn styles() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "text/css; charset=utf-8")],
        [
            ":root { color-scheme: light; --accent: #0f766e; }",
            "body { font-family: ui-sans-serif, sans-serif; margin: 2rem; background: linear-gradient(180deg, #f8fafc 0%, #e2e8f0 100%); }",
            ".status-badge-live { color: var(--accent); }",
            ".status-badge-offline { color: #991b1b; }",
            ".terminal-dashboard { white-space: pre-wrap; padding: 1rem; border-radius: 12px; background: #0f172a; color: #e2e8f0; overflow-x: auto; }",
            "[data-phx-main].phx-connected .status-badge-live { opacity: 1; }",
            "[data-phx-main].phx-connected .status-badge-offline { opacity: 0.5; }",
        ]
        .join("\n"),
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
    let settings =
        Settings::from_workflow(&state.workflow_store.current().await, &state.overrides).ok();

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
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::Value as JsonValue;
    use tokio::time::sleep;
    use tower::util::ServiceExt;

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
        std::fs::write(
            &workflow_path,
            "---\ntracker:\n  kind: memory\nworkspace:\n  root: /tmp/symphony-http\n---\n",
        )
        .unwrap();
        let workflow_store = WorkflowStore::new(workflow_path).await.unwrap();
        HttpState {
            backend,
            workflow_store,
            overrides: CliOverrides::default(),
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
            "/",
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
