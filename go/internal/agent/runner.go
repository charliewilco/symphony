// Package agent implements the worker agent runner that manages codex sessions per-issue.
package agent

import (
	"context"
	"fmt"
	"log/slog"

	"symphony/internal/codex"
	"symphony/internal/config"
	"symphony/internal/tracker"
	"symphony/internal/workspace"
)

// WorkerEventType identifies the kind of worker event.
type WorkerEventType int

const (
	// EventRuntimeInfo provides worker runtime info (host, workspace path).
	EventRuntimeInfo WorkerEventType = iota
	// EventCodexUpdate provides a codex session update.
	EventCodexUpdate
	// EventExit indicates the worker has exited.
	EventExit
)

// WorkerRuntimeInfo carries workspace and host info.
type WorkerRuntimeInfo struct {
	WorkerHost    string
	WorkspacePath string
}

// WorkerEvent is sent from the agent runner to the orchestrator.
type WorkerEvent struct {
	Type          WorkerEventType
	IssueID       string
	Runtime       *WorkerRuntimeInfo
	Update        *codex.CodexUpdate
	Err           error
	ExitNormal    bool
}

// Run launches a worker for the given issue, including workspace setup, codex session, and turn loop.
// It sends events on the provided channel and always sends a final Exit event.
func Run(
	ctx context.Context,
	issue tracker.Issue,
	settings *config.Settings,
	workflowPrompt string,
	t tracker.Tracker,
	workerEvents chan<- WorkerEvent,
	workerHost string,
	startingAttempt *int,
) {
	err := runInner(ctx, issue, settings, workflowPrompt, t, workerEvents, workerHost, startingAttempt)

	exitEvent := WorkerEvent{
		Type:    EventExit,
		IssueID: issue.ID,
	}
	if err != nil {
		exitEvent.Err = err
	} else {
		exitEvent.ExitNormal = true
	}
	select {
	case workerEvents <- exitEvent:
	default:
	}
}

func runInner(
	ctx context.Context,
	issue tracker.Issue,
	settings *config.Settings,
	workflowPrompt string,
	t tracker.Tracker,
	workerEvents chan<- WorkerEvent,
	workerHost string,
	startingAttempt *int,
) error {
	// Create workspace
	ws, err := workspace.CreateForIssue(ctx, issue.Identifier, settings, workerHost)
	if err != nil {
		return fmt.Errorf("workspace creation failed: %w", err)
	}

	// Send runtime info
	select {
	case workerEvents <- WorkerEvent{
		Type:    EventRuntimeInfo,
		IssueID: issue.ID,
		Runtime: &WorkerRuntimeInfo{
			WorkerHost:    workerHost,
			WorkspacePath: ws.Path,
		},
	}:
	default:
	}

	// Before run hook
	if err := workspace.RunBeforeRunHook(ctx, ws, issue.Identifier, settings); err != nil {
		return fmt.Errorf("before_run hook failed: %w", err)
	}

	// Start codex session
	session, err := codex.StartSession(ctx, ws.Path, workerHost, settings)
	if err != nil {
		return fmt.Errorf("codex session start failed: %w", err)
	}
	defer codex.StopSession(session)

	// Relay codex updates to worker events channel
	updatesCh := make(chan codex.CodexUpdate, 64)
	go func() {
		for update := range updatesCh {
			updateCopy := update
			select {
			case workerEvents <- WorkerEvent{
				Type:    EventCodexUpdate,
				IssueID: issue.ID,
				Update:  &updateCopy,
			}:
			default:
			}
		}
	}()

	currentIssue := issue
	turnNumber := 1
	attempt := startingAttempt

	for {
		var prompt string
		if turnNumber == 1 {
			template := workflowPrompt
			if template == "" {
				template = config.DefaultPromptTemplate()
			}
			issueObj := currentIssue.ToLiquidObject()
			var err error
			prompt, err = config.RenderPrompt(template, issueObj, attempt)
			if err != nil {
				close(updatesCh)
				return fmt.Errorf("prompt render failed: %w", err)
			}
		} else {
			prompt = fmt.Sprintf(
				"Continuation guidance:\n\n- The previous Codex turn completed normally, but the Linear issue is still in an active state.\n- This is continuation turn #%d of %d for the current agent run.\n- Resume from the current workspace and workpad state instead of restarting from scratch.\n- The original task instructions and prior turn context are already present in this thread, so do not restate them before acting.\n- Focus on the remaining ticket work and do not end the turn while the issue stays active unless you are truly blocked.",
				turnNumber, settings.Agent.MaxTurns,
			)
		}

		_, err := codex.RunTurn(ctx, session, prompt, &currentIssue, settings, updatesCh)
		if err != nil {
			close(updatesCh)
			return err
		}

		if turnNumber >= settings.Agent.MaxTurns {
			break
		}

		// Check if issue is still active
		refreshed, err := t.FetchIssueStatesByIDs(ctx, []string{currentIssue.ID}, settings)
		if err != nil {
			slog.Warn("failed to refresh issue state after turn", "issue", currentIssue.Identifier, "error", err)
			break
		}
		if len(refreshed) == 0 {
			break
		}
		refreshedIssue := refreshed[0]
		if !tracker.IsActiveState(&refreshedIssue, settings) {
			break
		}
		currentIssue = refreshedIssue
		turnNumber++
		if attempt == nil {
			a := 1
			attempt = &a
		} else {
			a := *attempt + 1
			attempt = &a
		}
	}

	close(updatesCh)
	workspace.RunAfterRunHook(ctx, ws, currentIssue.Identifier, settings)
	return nil
}
