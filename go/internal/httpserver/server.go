// Package httpserver provides the optional HTTP API and dashboard server.
package httpserver

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
	"time"

	"symphony/internal/config"
	"symphony/internal/orchestrator"
	"symphony/internal/presenter"
	"symphony/internal/workflow"
)

// Server holds the HTTP server state.
type Server struct {
	handle        *orchestrator.OrchestratorHandle
	workflowStore *workflow.WorkflowStore
	overrides     *config.CliOverrides
}

// Serve starts the HTTP server if a port is configured.
func Serve(handle *orchestrator.OrchestratorHandle, workflowStore *workflow.WorkflowStore, overrides *config.CliOverrides) error {
	settings, err := config.FromWorkflow(workflowStore.Current(), overrides)
	if err != nil {
		return err
	}
	if settings.Server.Port == nil {
		return nil
	}

	srv := &Server{
		handle:        handle,
		workflowStore: workflowStore,
		overrides:     overrides,
	}

	mux := http.NewServeMux()
	mux.HandleFunc("GET /", srv.handleDashboard)
	mux.HandleFunc("GET /dashboard.css", srv.handleCSS)
	mux.HandleFunc("GET /api/v1/state", srv.handleState)
	mux.HandleFunc("POST /api/v1/refresh", srv.handleRefresh)
	mux.HandleFunc("GET /api/v1/{identifier}", srv.handleIssue)

	addr := fmt.Sprintf("%s:%d", settings.Server.Host, *settings.Server.Port)
	slog.Info("starting HTTP server", "addr", addr)
	return http.ListenAndServe(addr, mux)
}

func (s *Server) handleDashboard(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/" {
		errorResponse(w, http.StatusNotFound, "not_found", "Route not found")
		return
	}
	snapshot, err := s.snapshotWithTimeout()
	settings := s.currentSettings()

	if err != nil || settings == nil {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		w.Write([]byte("<html><body><h1>Symphony</h1><p>snapshot_unavailable</p></body></html>"))
		return
	}

	html := presenter.RenderDashboardHTML(snapshot, settings)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Write([]byte(html))
}

func (s *Server) handleCSS(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "text/css; charset=utf-8")
	w.Write([]byte(dashboardCSS))
}

func (s *Server) handleState(w http.ResponseWriter, r *http.Request) {
	snapshot, snapErr := s.snapshotWithTimeout()
	var errType presenter.SnapshotError
	if snapErr != nil {
		if strings.Contains(snapErr.Error(), "timeout") {
			errType = presenter.SnapshotTimeout
		} else {
			errType = presenter.SnapshotUnavailable
		}
	}
	payload := presenter.StatePayload(snapshot, errType)
	writeJSON(w, http.StatusOK, payload)
}

func (s *Server) handleRefresh(w http.ResponseWriter, r *http.Request) {
	payload, err := s.handle.RequestRefresh()
	if err != nil {
		errorResponse(w, http.StatusServiceUnavailable, "orchestrator_unavailable", "Orchestrator is unavailable")
		return
	}
	writeJSON(w, http.StatusAccepted, payload)
}

func (s *Server) handleIssue(w http.ResponseWriter, r *http.Request) {
	identifier := r.PathValue("identifier")
	snapshot, err := s.snapshotWithTimeout()
	settings := s.currentSettings()
	if err != nil || snapshot == nil {
		errorResponse(w, http.StatusNotFound, "issue_not_found", "Issue not found")
		return
	}

	payload := presenter.IssuePayload(snapshot, identifier, settings)
	if payload == nil {
		errorResponse(w, http.StatusNotFound, "issue_not_found", "Issue not found")
		return
	}
	writeJSON(w, http.StatusOK, payload)
}

func (s *Server) snapshotWithTimeout() (*orchestrator.Snapshot, error) {
	type result struct {
		snapshot orchestrator.Snapshot
		err      error
	}
	ch := make(chan result, 1)
	go func() {
		snap, err := s.handle.Snapshot()
		ch <- result{snap, err}
	}()

	select {
	case r := <-ch:
		if r.err != nil {
			return nil, r.err
		}
		return &r.snapshot, nil
	case <-time.After(15 * time.Second):
		return nil, fmt.Errorf("snapshot_timeout")
	}
}

func (s *Server) currentSettings() *config.Settings {
	settings, err := config.FromWorkflow(s.workflowStore.Current(), s.overrides)
	if err != nil {
		return nil
	}
	return settings
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(v)
}

func errorResponse(w http.ResponseWriter, status int, code, message string) {
	writeJSON(w, status, map[string]any{
		"error": map[string]any{
			"code":    code,
			"message": message,
		},
	})
}

const dashboardCSS = `
:root {
  color-scheme: light;
  --page: #f5f6f7;
  --card: rgba(255,255,255,0.94);
  --ink: #252930;
  --muted: #717887;
  --line: #e7e9ef;
  --accent: #4b7f8a;
  --shadow-sm: 0 1px 2px rgba(16,24,40,0.05);
}
* { box-sizing: border-box; }
html { background: var(--page); }
body {
  margin: 0; min-height: 100vh;
  color: var(--ink);
  font-family: "SF Pro Text","Helvetica Neue","Segoe UI",sans-serif;
  line-height: 1.42;
}
.app-shell { max-width: 1280px; margin: 0 auto; padding: 2rem 1rem 3.5rem; }
.dashboard-shell { display: grid; gap: 1rem; }
.hero-card, .section-card, .metric-card {
  background: var(--card); border: 1px solid rgba(217,217,227,0.82);
  box-shadow: var(--shadow-sm); border-radius: 20px; padding: 0.9rem;
}
.hero-card { border-radius: 28px; padding: clamp(1.25rem,3vw,2rem); }
.hero-grid { display: grid; gap: 1.25rem; }
.eyebrow { margin: 0; color: var(--muted); text-transform: uppercase; letter-spacing: 0.08em; font-size: 0.76rem; font-weight: 600; }
.hero-title { margin: 0.35rem 0 0; font-size: clamp(1.85rem,4vw,2.85rem); line-height: 0.98; letter-spacing: -0.04em; }
.hero-copy { margin: 0.75rem 0 0; color: var(--muted); font-size: 0.96rem; }
.metric-grid { display: grid; gap: 0.72rem; grid-template-columns: repeat(auto-fit,minmax(180px,1fr)); }
.metric-card { border-radius: 20px; padding: 0.88rem 0.95rem; }
.metric-label { margin: 0; color: var(--muted); font-size: 0.82rem; font-weight: 600; }
.metric-value { margin: 0.3rem 0 0; font-size: clamp(1.45rem,2vw,1.95rem); line-height: 1.05; letter-spacing: -0.03em; }
.section-title { margin: 0; font-size: 0.95rem; }
.terminal-frame {
  border-radius: 20px; background: #2a2d34;
  border: 1px solid rgba(255,255,255,0.03); overflow: auto;
}
.terminal-dashboard { min-width: max-content; padding: 0.95rem 1rem; color: #f3f4f6; font-size: 0.91rem; white-space: pre; }
.table-wrap { overflow-x: auto; border-radius: 16px; border: 1px solid var(--line); }
.data-table { width: 100%; min-width: 760px; border-collapse: collapse; background: white; }
.data-table th, .data-table td { text-align: left; padding: 0.72rem 0.82rem; border-bottom: 1px solid var(--line); }
.data-table th { color: var(--muted); font-size: 0.72rem; font-weight: 700; text-transform: uppercase; letter-spacing: 0.03em; }
.muted { color: var(--muted); }
.mono { font-family: "SFMono-Regular",Consolas,monospace; }
.numeric { font-variant-numeric: tabular-nums slashed-zero; }
`
