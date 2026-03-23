// Package dashboard renders terminal status output with ANSI colors.
package dashboard

import (
	"fmt"
	"os"
	"sort"
	"strings"
	"time"

	"symphony/internal/config"
	"symphony/internal/orchestrator"
)

const (
	runningIDWidth      = 8
	runningStageWidth   = 14
	runningPIDWidth     = 8
	runningAgeWidth     = 12
	runningTokensWidth  = 10
	runningSessionWidth = 14
	runningEventDefault = 44
)

// noColor checks the NO_COLOR env var.
func noColor() bool {
	return os.Getenv("NO_COLOR") != ""
}

func ansiCode(code string) string {
	if noColor() {
		return ""
	}
	return code
}

var (
	bold    = func() string { return ansiCode("\033[1m") }
	reset   = func() string { return ansiCode("\033[0m") }
	cyan    = func() string { return ansiCode("\033[36m") }
	green   = func() string { return ansiCode("\033[32m") }
	yellow  = func() string { return ansiCode("\033[33m") }
	red     = func() string { return ansiCode("\033[31m") }
	magenta = func() string { return ansiCode("\033[35m") }
	dim     = func() string { return ansiCode("\033[2m") }
)

// FormatSnapshot renders the terminal dashboard as a string.
func FormatSnapshot(snapshot *orchestrator.Snapshot, settings *config.Settings, tps float64, terminalColumns int) string {
	width := terminalColumns
	if width < 80 {
		width = 80
	}
	if width == 0 {
		width = 115
	}

	eventWidth := width - runningIDWidth - runningStageWidth - runningPIDWidth - runningAgeWidth - runningTokensWidth - runningSessionWidth - 10
	if eventWidth < 12 {
		eventWidth = 12
	}

	var lines []string
	lines = append(lines, fmt.Sprintf("%s%s╭─ SYMPHONY STATUS%s", bold(), cyan(), reset()))

	if snapshot == nil {
		lines = append(lines, "│ Orchestrator snapshot unavailable")
		lines = append(lines, closingBorder(width))
		return strings.Join(lines, "\n")
	}

	lines = append(lines, fmt.Sprintf("%s├─ Status%s", bold(), reset()))
	lines = append(lines, "│")

	// Status line
	lines = append(lines, fmt.Sprintf("│  Agents: %d/%d    Throughput: %.1f tps    Runtime: %s",
		len(snapshot.Running), settings.Agent.MaxConcurrentAgents,
		tps, formatRuntimeSeconds(snapshot.CodexTotals.SecondsRunning)))

	// Token line
	lines = append(lines, fmt.Sprintf("│  Tokens: in %s | out %s | total %s",
		formatCount(snapshot.CodexTotals.InputTokens),
		formatCount(snapshot.CodexTotals.OutputTokens),
		formatCount(snapshot.CodexTotals.TotalTokens)))

	lines = append(lines, "│")

	// Running agents
	lines = append(lines, fmt.Sprintf("%s├─ Running%s", bold(), reset()))
	lines = append(lines, "│")

	// Header row
	lines = append(lines, fmt.Sprintf("│  %s%-*s %-*s %-*s %-*s %-*s %-*s %-*s%s",
		dim(),
		runningIDWidth, "ID",
		runningStageWidth, "STAGE",
		runningPIDWidth, "PID",
		runningAgeWidth, "AGE/TURN",
		runningTokensWidth, "TOKENS",
		runningSessionWidth, "SESSION",
		eventWidth, "EVENT",
		reset()))

	// Separator
	sepWidth := runningIDWidth + runningStageWidth + runningPIDWidth + runningAgeWidth + runningTokensWidth + runningSessionWidth + eventWidth + 6
	lines = append(lines, "│  "+strings.Repeat("─", sepWidth))

	if len(snapshot.Running) == 0 {
		lines = append(lines, "│  No active agents")
	} else {
		sorted := make([]orchestrator.RunningSnapshot, len(snapshot.Running))
		copy(sorted, snapshot.Running)
		sort.Slice(sorted, func(i, j int) bool {
			return sorted[i].Identifier < sorted[j].Identifier
		})
		for _, entry := range sorted {
			lines = append(lines, formatRunningEntry(&entry, eventWidth))
		}
	}

	lines = append(lines, "│")
	lines = append(lines, fmt.Sprintf("%s├─ Backoff queue%s", bold(), reset()))
	lines = append(lines, "│")

	if len(snapshot.Retrying) == 0 {
		lines = append(lines, "│  No queued retries")
	} else {
		sorted := make([]orchestrator.RetrySnapshot, len(snapshot.Retrying))
		copy(sorted, snapshot.Retrying)
		sort.Slice(sorted, func(i, j int) bool {
			return sorted[i].DueInMs < sorted[j].DueInMs
		})
		for _, entry := range sorted {
			lines = append(lines, formatRetryEntry(&entry))
		}
	}

	lines = append(lines, closingBorder(width))
	return strings.Join(lines, "\n")
}

func formatRunningEntry(entry *orchestrator.RunningSnapshot, eventWidth int) string {
	id := truncate(entry.Identifier, runningIDWidth)
	stage := truncate(entry.State, runningStageWidth)
	pid := truncate(entry.CodexAppServerPID, runningPIDWidth)
	if pid == "" {
		pid = "-"
	}
	age := formatAge(entry.StartedAt, entry.TurnCount)
	tokens := formatCount(entry.CodexTotalTokens)
	session := truncate(entry.SessionID, runningSessionWidth)
	if session == "" {
		session = "-"
	}
	event := truncate(humanizeCodexMessage(entry.LastCodexMessage, entry.LastCodexEvent), eventWidth)

	return fmt.Sprintf("│  %s%-*s%s %s%-*s%s %-*s %s%-*s%s %s%-*s%s %s%-*s%s %s",
		cyan(), runningIDWidth, id, reset(),
		green(), runningStageWidth, stage, reset(),
		runningPIDWidth, pid,
		magenta(), runningAgeWidth, age, reset(),
		yellow(), runningTokensWidth, tokens, reset(),
		cyan(), runningSessionWidth, session, reset(),
		event)
}

func formatRetryEntry(entry *orchestrator.RetrySnapshot) string {
	id := entry.Identifier
	if id == "" {
		id = entry.IssueID
	}
	dueIn := formatDuration(time.Duration(entry.DueInMs) * time.Millisecond)
	errMsg := entry.Error
	if errMsg == "" {
		errMsg = "continuation"
	}
	return fmt.Sprintf("│  %s%s%s  attempt=%d  due_in=%s  %s%s%s",
		yellow(), id, reset(),
		entry.Attempt,
		dueIn,
		dim(), truncate(errMsg, 60), reset())
}

func humanizeCodexMessage(message any, event string) string {
	if message == nil {
		return "no codex message yet"
	}
	if s, ok := message.(string); ok {
		return s
	}
	if event != "" {
		return event
	}
	return "..."
}

func closingBorder(width int) string {
	return "╰" + strings.Repeat("─", width-2) + "╯"
}

func formatAge(startedAt time.Time, turnCount int64) string {
	elapsed := time.Since(startedAt)
	return fmt.Sprintf("%s/t%d", formatDuration(elapsed), turnCount)
}

func formatDuration(d time.Duration) string {
	if d < time.Minute {
		return fmt.Sprintf("%ds", int(d.Seconds()))
	}
	if d < time.Hour {
		return fmt.Sprintf("%dm%ds", int(d.Minutes()), int(d.Seconds())%60)
	}
	return fmt.Sprintf("%dh%dm", int(d.Hours()), int(d.Minutes())%60)
}

func formatCount(n int64) string {
	if n >= 1_000_000 {
		return fmt.Sprintf("%.1fM", float64(n)/1_000_000)
	}
	if n >= 1_000 {
		return fmt.Sprintf("%.1fK", float64(n)/1_000)
	}
	return fmt.Sprintf("%d", n)
}

func formatRuntimeSeconds(seconds int64) string {
	d := time.Duration(seconds) * time.Second
	return formatDuration(d)
}

func truncate(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	if maxLen <= 3 {
		return s[:maxLen]
	}
	return s[:maxLen-3] + "..."
}
