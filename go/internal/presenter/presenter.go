// Package presenter converts orchestrator snapshots to HTTP API payloads and HTML.
package presenter

import (
	"encoding/json"
	"fmt"
	"html"
	"strings"
	"time"

	"symphony/internal/config"
	"symphony/internal/dashboard"
	"symphony/internal/orchestrator"
)

// SnapshotError represents a snapshot fetch error.
type SnapshotError int

const (
	SnapshotUnavailable SnapshotError = iota
	SnapshotTimeout
)

// StatePayload builds the JSON payload for GET /api/v1/state.
func StatePayload(snapshot *orchestrator.Snapshot, err SnapshotError) map[string]any {
	generatedAt := time.Now().UTC().Format(time.RFC3339)

	if snapshot == nil {
		code := "snapshot_unavailable"
		message := "Snapshot unavailable"
		if err == SnapshotTimeout {
			code = "snapshot_timeout"
			message = "Snapshot timed out"
		}
		return map[string]any{
			"generated_at": generatedAt,
			"error": map[string]any{
				"code":    code,
				"message": message,
			},
		}
	}

	running := make([]map[string]any, 0, len(snapshot.Running))
	for _, entry := range snapshot.Running {
		running = append(running, projectRunningEntry(&entry))
	}
	retrying := make([]map[string]any, 0, len(snapshot.Retrying))
	for _, entry := range snapshot.Retrying {
		retrying = append(retrying, projectRetryEntry(&entry))
	}

	return map[string]any{
		"generated_at": generatedAt,
		"counts": map[string]any{
			"running":  len(snapshot.Running),
			"retrying": len(snapshot.Retrying),
		},
		"running":      running,
		"retrying":     retrying,
		"codex_totals": snapshot.CodexTotals,
		"rate_limits":  json.RawMessage(snapshot.RateLimits),
	}
}

// IssuePayload builds the JSON payload for GET /api/v1/:identifier.
func IssuePayload(snapshot *orchestrator.Snapshot, issueIdentifier string, settings *config.Settings) map[string]any {
	var runningEntry *orchestrator.RunningSnapshot
	var retryEntry *orchestrator.RetrySnapshot

	for i := range snapshot.Running {
		if snapshot.Running[i].Identifier == issueIdentifier {
			runningEntry = &snapshot.Running[i]
			break
		}
	}
	for i := range snapshot.Retrying {
		if snapshot.Retrying[i].Identifier == issueIdentifier {
			retryEntry = &snapshot.Retrying[i]
			break
		}
	}

	if runningEntry == nil && retryEntry == nil {
		return nil
	}

	status := "retrying"
	if runningEntry != nil {
		status = "running"
	}

	issueID := ""
	if runningEntry != nil {
		issueID = runningEntry.IssueID
	} else if retryEntry != nil {
		issueID = retryEntry.IssueID
	}

	workspacePath := ""
	workerHost := ""
	if runningEntry != nil {
		workspacePath = runningEntry.WorkspacePath
		workerHost = runningEntry.WorkerHost
	} else if retryEntry != nil {
		workspacePath = retryEntry.WorkspacePath
		workerHost = retryEntry.WorkerHost
	}
	if workspacePath == "" && settings != nil {
		workspacePath = settings.Workspace.Root + "/" + issueIdentifier
	}
	if workspacePath == "" {
		workspacePath = issueIdentifier
	}

	restartCount := 0
	currentRetry := 0
	if retryEntry != nil {
		if retryEntry.Attempt > 1 {
			restartCount = retryEntry.Attempt - 1
		}
		currentRetry = retryEntry.Attempt
	}

	var lastError *string
	if retryEntry != nil && retryEntry.Error != "" {
		lastError = &retryEntry.Error
	}

	result := map[string]any{
		"issue_identifier": issueIdentifier,
		"issue_id":         issueID,
		"status":           status,
		"workspace": map[string]any{
			"path": workspacePath,
			"host": workerHost,
		},
		"attempts": map[string]any{
			"restart_count":        restartCount,
			"current_retry_attempt": currentRetry,
		},
		"logs":          map[string]any{"codex_session_logs": []any{}},
		"recent_events": []any{},
		"last_error":    lastError,
		"tracked":       map[string]any{},
	}

	if runningEntry != nil {
		result["running"] = projectRunningEntry(runningEntry)
	}
	if retryEntry != nil {
		result["retry"] = projectRetryEntry(retryEntry)
	}

	return result
}

// RenderDashboardHTML renders the full HTML dashboard page.
func RenderDashboardHTML(snapshot *orchestrator.Snapshot, settings *config.Settings) string {
	terminalView := dashboard.FormatSnapshot(snapshot, settings, 0, 115)

	var b strings.Builder
	b.WriteString("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">")
	b.WriteString("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">")
	b.WriteString("<title>Symphony Observability</title>")
	b.WriteString("<link rel=\"stylesheet\" href=\"/dashboard.css\">")
	b.WriteString("</head><body>")
	b.WriteString("<main class=\"app-shell\"><section class=\"dashboard-shell\">")

	// Hero
	b.WriteString("<header class=\"hero-card\"><div class=\"hero-grid\"><div>")
	b.WriteString("<p class=\"eyebrow\">Symphony Observability</p>")
	b.WriteString("<h1 class=\"hero-title\">Operations Dashboard</h1>")
	b.WriteString("<p class=\"hero-copy\">Current state, retry pressure, token usage, and orchestration health.</p>")
	b.WriteString("</div></div></header>")

	// Metrics
	b.WriteString("<section class=\"metric-grid\">")
	b.WriteString(fmt.Sprintf("<article class=\"metric-card\"><p class=\"metric-label\">Running</p><p class=\"metric-value numeric\">%d</p></article>", len(snapshot.Running)))
	b.WriteString(fmt.Sprintf("<article class=\"metric-card\"><p class=\"metric-label\">Retrying</p><p class=\"metric-value numeric\">%d</p></article>", len(snapshot.Retrying)))
	b.WriteString(fmt.Sprintf("<article class=\"metric-card\"><p class=\"metric-label\">Total tokens</p><p class=\"metric-value numeric\">%s</p></article>", formatInt(snapshot.CodexTotals.TotalTokens)))
	b.WriteString("</section>")

	// Terminal status
	b.WriteString("<section class=\"section-card\"><div class=\"section-header\"><div><h2 class=\"section-title\">Terminal status</h2></div></div>")
	b.WriteString("<div class=\"terminal-frame\"><pre class=\"terminal-dashboard\">")
	b.WriteString(html.EscapeString(terminalView))
	b.WriteString("</pre></div></section>")

	// Running table
	b.WriteString("<section class=\"section-card\"><div class=\"section-header\"><div><h2 class=\"section-title\">Running sessions</h2></div></div>")
	b.WriteString("<div class=\"table-wrap\"><table class=\"data-table\">")
	b.WriteString("<thead><tr><th>Issue</th><th>State</th><th>Session</th><th>Tokens</th><th>Runtime</th></tr></thead><tbody>")
	if len(snapshot.Running) == 0 {
		b.WriteString("<tr><td colspan=\"5\" class=\"muted\">No active agents</td></tr>")
	} else {
		for _, entry := range snapshot.Running {
			b.WriteString(fmt.Sprintf("<tr><td>%s</td><td>%s</td><td class=\"mono\">%s</td><td class=\"numeric\">%s</td><td class=\"numeric\">%ds</td></tr>",
				html.EscapeString(entry.Identifier),
				html.EscapeString(entry.State),
				html.EscapeString(entry.SessionID),
				formatInt(entry.CodexTotalTokens),
				entry.RuntimeSeconds))
		}
	}
	b.WriteString("</tbody></table></div></section>")

	// Retry table
	b.WriteString("<section class=\"section-card\"><div class=\"section-header\"><div><h2 class=\"section-title\">Retry queue</h2></div></div>")
	b.WriteString("<div class=\"table-wrap\"><table class=\"data-table\">")
	b.WriteString("<thead><tr><th>Issue</th><th>Attempt</th><th>Due in</th><th>Error</th></tr></thead><tbody>")
	if len(snapshot.Retrying) == 0 {
		b.WriteString("<tr><td colspan=\"4\" class=\"muted\">No queued retries</td></tr>")
	} else {
		for _, entry := range snapshot.Retrying {
			id := entry.Identifier
			if id == "" {
				id = entry.IssueID
			}
			b.WriteString(fmt.Sprintf("<tr><td>%s</td><td class=\"numeric\">%d</td><td class=\"numeric\">%dms</td><td>%s</td></tr>",
				html.EscapeString(id),
				entry.Attempt,
				entry.DueInMs,
				html.EscapeString(entry.Error)))
		}
	}
	b.WriteString("</tbody></table></div></section>")

	b.WriteString("</section></main></body></html>")
	return b.String()
}

func projectRunningEntry(entry *orchestrator.RunningSnapshot) map[string]any {
	return map[string]any{
		"issue_id":             entry.IssueID,
		"identifier":           entry.Identifier,
		"state":                entry.State,
		"worker_host":          entry.WorkerHost,
		"workspace_path":       entry.WorkspacePath,
		"session_id":           entry.SessionID,
		"codex_app_server_pid": entry.CodexAppServerPID,
		"codex_input_tokens":   entry.CodexInputTokens,
		"codex_output_tokens":  entry.CodexOutputTokens,
		"codex_total_tokens":   entry.CodexTotalTokens,
		"turn_count":           entry.TurnCount,
		"started_at":           entry.StartedAt.Format(time.RFC3339),
		"last_codex_timestamp": formatTimePtr(entry.LastCodexTimestamp),
		"last_codex_message":   entry.LastCodexMessage,
		"last_codex_event":     entry.LastCodexEvent,
		"runtime_seconds":      entry.RuntimeSeconds,
	}
}

func projectRetryEntry(entry *orchestrator.RetrySnapshot) map[string]any {
	return map[string]any{
		"issue_id":       entry.IssueID,
		"attempt":        entry.Attempt,
		"due_in_ms":      entry.DueInMs,
		"identifier":     entry.Identifier,
		"error":          entry.Error,
		"worker_host":    entry.WorkerHost,
		"workspace_path": entry.WorkspacePath,
	}
}

func formatTimePtr(t *time.Time) any {
	if t == nil {
		return nil
	}
	return t.Format(time.RFC3339)
}

func formatInt(n int64) string {
	if n >= 1_000_000 {
		return fmt.Sprintf("%.1fM", float64(n)/1_000_000)
	}
	if n >= 1_000 {
		return fmt.Sprintf("%.1fK", float64(n)/1_000)
	}
	return fmt.Sprintf("%d", n)
}
