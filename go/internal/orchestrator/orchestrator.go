// Package orchestrator implements the main scheduling loop, state machine, and dispatch logic.
package orchestrator

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"math"
	"sort"
	"sync"
	"time"

	"symphony/internal/agent"
	"symphony/internal/codex"
	"symphony/internal/config"
	"symphony/internal/tracker"
	"symphony/internal/workflow"
	"symphony/internal/workspace"
)

const (
	continuationRetryDelayMs = 1000
	failureRetryBaseMs       = 10000
)

// RunningEntry tracks state for an actively running issue.
type RunningEntry struct {
	Identifier                  string
	Issue                       tracker.Issue
	StartedAt                   time.Time
	SessionID                   string
	CodexAppServerPID           string
	CodexInputTokens            int64
	CodexOutputTokens           int64
	CodexTotalTokens            int64
	CodexLastReportedInput      int64
	CodexLastReportedOutput     int64
	CodexLastReportedTotal      int64
	TurnCount                   int64
	LastCodexTimestamp           *time.Time
	LastCodexMessage             any
	LastCodexEvent              string
	RuntimeSeconds              int64
	WorkspacePath               string
	WorkerHost                  string
	Attempt                     *int
	Cancel                      context.CancelFunc
}

// RetryEntry tracks a pending retry.
type RetryEntry struct {
	IssueID       string `json:"issue_id"`
	Identifier    string `json:"identifier,omitempty"`
	Attempt       int    `json:"attempt"`
	DueAtMs       int64  `json:"due_at_ms"`
	Error         string `json:"error,omitempty"`
	WorkerHost    string `json:"worker_host,omitempty"`
	WorkspacePath string `json:"workspace_path,omitempty"`
	Token         uint64 `json:"token"`
}

// TokenTotals holds aggregate token counts.
type TokenTotals struct {
	InputTokens    int64 `json:"input_tokens"`
	OutputTokens   int64 `json:"output_tokens"`
	TotalTokens    int64 `json:"total_tokens"`
	SecondsRunning int64 `json:"seconds_running"`
}

// OrchestratorState is the single authoritative in-memory state.
type OrchestratorState struct {
	Running             map[string]*RunningEntry
	Claimed             map[string]struct{}
	RetryAttempts       map[string]*RetryEntry
	Completed           map[string]struct{}
	CodexTotals         TokenTotals
	CodexRateLimits     json.RawMessage
	PollIntervalMs      int64
	MaxConcurrentAgents int
	MaxRetryBackoffMs   int64
	retryTokenCounter   uint64
}

// Snapshot types for external consumption.
type Snapshot struct {
	Running    []RunningSnapshot `json:"running"`
	Retrying   []RetrySnapshot   `json:"retrying"`
	CodexTotals TokenTotals      `json:"codex_totals"`
	RateLimits json.RawMessage   `json:"rate_limits"`
	Polling    PollingSnapshot   `json:"polling"`
}

type RunningSnapshot struct {
	IssueID            string     `json:"issue_id"`
	Identifier         string     `json:"identifier"`
	State              string     `json:"state"`
	WorkerHost         string     `json:"worker_host,omitempty"`
	WorkspacePath      string     `json:"workspace_path,omitempty"`
	SessionID          string     `json:"session_id,omitempty"`
	CodexAppServerPID  string     `json:"codex_app_server_pid,omitempty"`
	CodexInputTokens   int64      `json:"codex_input_tokens"`
	CodexOutputTokens  int64      `json:"codex_output_tokens"`
	CodexTotalTokens   int64      `json:"codex_total_tokens"`
	TurnCount          int64      `json:"turn_count"`
	StartedAt          time.Time  `json:"started_at"`
	LastCodexTimestamp  *time.Time `json:"last_codex_timestamp,omitempty"`
	LastCodexMessage   any        `json:"last_codex_message,omitempty"`
	LastCodexEvent     string     `json:"last_codex_event,omitempty"`
	RuntimeSeconds     int64      `json:"runtime_seconds"`
}

type RetrySnapshot struct {
	IssueID       string `json:"issue_id"`
	Attempt       int    `json:"attempt"`
	DueInMs       int64  `json:"due_in_ms"`
	Identifier    string `json:"identifier,omitempty"`
	Error         string `json:"error,omitempty"`
	WorkerHost    string `json:"worker_host,omitempty"`
	WorkspacePath string `json:"workspace_path,omitempty"`
}

type PollingSnapshot struct {
	Checking       bool   `json:"checking"`
	NextPollInMs   *int64 `json:"next_poll_in_ms,omitempty"`
	PollIntervalMs int64  `json:"poll_interval_ms"`
}

// OrchestratorHandle is the external interface to the orchestrator.
type OrchestratorHandle struct {
	snapshotCh chan chan Snapshot
	refreshCh  chan chan config.RefreshPayload
}

// Snapshot requests a snapshot of the orchestrator state.
func (h *OrchestratorHandle) Snapshot() (Snapshot, error) {
	reply := make(chan Snapshot, 1)
	h.snapshotCh <- reply
	snap := <-reply
	return snap, nil
}

// RequestRefresh triggers an immediate poll cycle.
func (h *OrchestratorHandle) RequestRefresh() (config.RefreshPayload, error) {
	reply := make(chan config.RefreshPayload, 1)
	h.refreshCh <- reply
	payload := <-reply
	return payload, nil
}

// OrchestratorRuntime is the main orchestrator.
type OrchestratorRuntime struct {
	state         *OrchestratorState
	mu            sync.Mutex // protects state for snapshot reads
	workflowStore *workflow.WorkflowStore
	overrides     *config.CliOverrides
	workerEvents  chan agent.WorkerEvent
	snapshotCh    chan chan Snapshot
	refreshCh     chan chan config.RefreshPayload

	nextPollDueAt int64
	pollInProgress bool
}

// Start creates and launches the orchestrator in a goroutine.
func Start(ctx context.Context, workflowStore *workflow.WorkflowStore, overrides *config.CliOverrides) (*OrchestratorHandle, error) {
	currentWorkflow := workflowStore.Current()
	settings, err := config.FromWorkflow(currentWorkflow, overrides)
	if err != nil {
		return nil, fmt.Errorf("initial config failed: %w", err)
	}

	state := &OrchestratorState{
		Running:             make(map[string]*RunningEntry),
		Claimed:             make(map[string]struct{}),
		RetryAttempts:       make(map[string]*RetryEntry),
		Completed:           make(map[string]struct{}),
		PollIntervalMs:      settings.Polling.IntervalMs,
		MaxConcurrentAgents: settings.Agent.MaxConcurrentAgents,
		MaxRetryBackoffMs:   settings.Agent.MaxRetryBackoffMs,
	}

	snapshotCh := make(chan chan Snapshot, 16)
	refreshCh := make(chan chan config.RefreshPayload, 16)

	runtime := &OrchestratorRuntime{
		state:         state,
		workflowStore: workflowStore,
		overrides:     overrides,
		workerEvents:  make(chan agent.WorkerEvent, 256),
		snapshotCh:    snapshotCh,
		refreshCh:     refreshCh,
		nextPollDueAt: nowMillis(),
	}

	handle := &OrchestratorHandle{
		snapshotCh: snapshotCh,
		refreshCh:  refreshCh,
	}

	go runtime.run(ctx)
	return handle, nil
}

func (rt *OrchestratorRuntime) run(ctx context.Context) {
	// Startup cleanup
	rt.runTerminalCleanup(ctx)

	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return

		case <-ticker.C:
			rt.handleTick(ctx)

		case event := <-rt.workerEvents:
			rt.handleWorkerEvent(ctx, event)

		case reply := <-rt.snapshotCh:
			reply <- rt.buildSnapshot()

		case reply := <-rt.refreshCh:
			rt.nextPollDueAt = nowMillis()
			reply <- config.RefreshPayload{
				Queued:      true,
				Coalesced:   false,
				RequestedAt: time.Now().UTC().Format(time.RFC3339),
				Operations:  []string{"poll", "reconcile"},
			}
		}
	}
}

func (rt *OrchestratorRuntime) handleTick(ctx context.Context) {
	settings, err := rt.refreshSettings()
	if err != nil {
		slog.Error("failed to refresh settings", "error", err)
		return
	}

	now := nowMillis()
	if now < rt.nextPollDueAt {
		return
	}
	rt.pollInProgress = true

	// 1. Reconcile running issues
	rt.reconcileRunningIssues(ctx, settings)

	// 2. Dispatch ready retries
	rt.dispatchReadyRetries(ctx, settings)

	// 3. Fetch and dispatch candidate issues
	if err := settings.Validate(); err != nil {
		slog.Error("dispatch preflight validation failed", "error", err)
	} else {
		rt.dispatchCandidateIssues(ctx, settings)
	}

	rt.pollInProgress = false
	rt.nextPollDueAt = nowMillis() + settings.Polling.IntervalMs
}

func (rt *OrchestratorRuntime) refreshSettings() (*config.Settings, error) {
	if err := rt.workflowStore.MaybeReload(); err != nil {
		slog.Error("failed to reload workflow", "error", err)
	}
	settings, err := config.FromWorkflow(rt.workflowStore.Current(), rt.overrides)
	if err != nil {
		return nil, err
	}
	rt.state.PollIntervalMs = settings.Polling.IntervalMs
	rt.state.MaxConcurrentAgents = settings.Agent.MaxConcurrentAgents
	rt.state.MaxRetryBackoffMs = settings.Agent.MaxRetryBackoffMs
	return settings, nil
}

func (rt *OrchestratorRuntime) runTerminalCleanup(ctx context.Context) {
	settings, err := config.FromWorkflow(rt.workflowStore.Current(), rt.overrides)
	if err != nil {
		slog.Error("terminal cleanup: config failed", "error", err)
		return
	}
	t := tracker.TrackerForSettings(settings)
	issues, err := t.FetchIssuesByStates(ctx, settings.Tracker.TerminalStates, settings)
	if err != nil {
		slog.Error("terminal cleanup: fetch failed", "error", err)
		return
	}
	for _, issue := range issues {
		if len(settings.Worker.SSHHosts) == 0 {
			workspace.RemoveIssueWorkspace(ctx, issue.Identifier, settings, "")
		} else {
			for _, host := range settings.Worker.SSHHosts {
				workspace.RemoveIssueWorkspace(ctx, issue.Identifier, settings, host)
			}
		}
	}
}

func (rt *OrchestratorRuntime) dispatchCandidateIssues(ctx context.Context, settings *config.Settings) {
	t := tracker.TrackerForSettings(settings)
	issues, err := t.FetchCandidateIssues(ctx, settings)
	if err != nil {
		slog.Error("failed to fetch candidate issues", "error", err)
		return
	}
	sortIssuesForDispatch(issues)

	for _, issue := range issues {
		if !rt.shouldDispatchIssue(&issue, settings) {
			continue
		}
		rt.dispatchIssue(ctx, issue, nil, "", settings, t)
	}
}

func (rt *OrchestratorRuntime) dispatchReadyRetries(ctx context.Context, settings *config.Settings) {
	now := nowMillis()
	var ready []*RetryEntry
	for _, entry := range rt.state.RetryAttempts {
		if entry.DueAtMs <= now {
			entryCopy := *entry
			ready = append(ready, &entryCopy)
		}
	}
	if len(ready) == 0 {
		return
	}

	t := tracker.TrackerForSettings(settings)
	for _, retry := range ready {
		// Verify token hasn't changed
		current, ok := rt.state.RetryAttempts[retry.IssueID]
		if !ok || current.Token != retry.Token {
			continue
		}

		// Fetch current issue state
		issues, err := t.FetchIssueStatesByIDs(ctx, []string{retry.IssueID}, settings)
		if err != nil {
			slog.Error("retry: failed to fetch issue", "issue_id", retry.IssueID, "error", err)
			continue
		}
		if len(issues) == 0 {
			delete(rt.state.RetryAttempts, retry.IssueID)
			delete(rt.state.Claimed, retry.IssueID)
			continue
		}
		issue := issues[0]
		if !rt.isDispatchEligible(&issue, settings) {
			delete(rt.state.RetryAttempts, retry.IssueID)
			delete(rt.state.Claimed, retry.IssueID)
			continue
		}

		// Check capacity
		if len(rt.state.Running) >= settings.Agent.MaxConcurrentAgents {
			rt.scheduleRetry(retryRequest{
				issueID:       issue.ID,
				attempt:       retry.Attempt + 1,
				retryKind:     retryFailure,
				identifier:    issue.Identifier,
				error:         "no available orchestrator slots",
				workerHost:    retry.WorkerHost,
				workspacePath: retry.WorkspacePath,
			})
			continue
		}

		attempt := retry.Attempt
		rt.dispatchIssue(ctx, issue, &attempt, retry.WorkerHost, settings, t)
	}
}

func (rt *OrchestratorRuntime) reconcileRunningIssues(ctx context.Context, settings *config.Settings) {
	// Part A: stall detection
	rt.reconcileStalledRuns(settings)

	// Part B: tracker state refresh
	runningIDs := make([]string, 0, len(rt.state.Running))
	for id := range rt.state.Running {
		runningIDs = append(runningIDs, id)
	}
	if len(runningIDs) == 0 {
		return
	}

	t := tracker.TrackerForSettings(settings)
	issues, err := t.FetchIssueStatesByIDs(ctx, runningIDs, settings)
	if err != nil {
		slog.Debug("failed to refresh running issues", "error", err)
		return
	}

	issuesByID := make(map[string]tracker.Issue)
	for _, issue := range issues {
		issuesByID[issue.ID] = issue
	}

	for _, issueID := range runningIDs {
		issue, found := issuesByID[issueID]
		if !found {
			rt.terminateRunningIssue(issueID, false, settings)
			continue
		}
		if tracker.IsTerminalState(&issue, settings) {
			rt.terminateRunningIssue(issueID, true, settings)
		} else if !tracker.IsActiveState(&issue, settings) {
			rt.terminateRunningIssue(issueID, false, settings)
		} else {
			// Update the running entry's issue snapshot
			if entry, ok := rt.state.Running[issueID]; ok {
				entry.Issue = issue
			}
		}
	}
}

func (rt *OrchestratorRuntime) reconcileStalledRuns(settings *config.Settings) {
	if settings.Codex.StallTimeoutMs <= 0 {
		return
	}
	now := time.Now().UTC()
	var stalled []string
	for issueID, entry := range rt.state.Running {
		ref := entry.StartedAt
		if entry.LastCodexTimestamp != nil {
			ref = *entry.LastCodexTimestamp
		}
		elapsed := now.Sub(ref).Milliseconds()
		if elapsed > settings.Codex.StallTimeoutMs {
			stalled = append(stalled, issueID)
		}
	}
	for _, issueID := range stalled {
		entry := rt.state.Running[issueID]
		slog.Warn("stall detected, terminating", "issue", entry.Identifier, "issue_id", issueID)
		attempt := nextRetryAttempt(entry.Attempt)
		workerHost := entry.WorkerHost
		wsPath := entry.WorkspacePath
		identifier := entry.Identifier
		rt.terminateRunningIssue(issueID, false, settings)
		rt.scheduleRetry(retryRequest{
			issueID:       issueID,
			attempt:       attempt,
			retryKind:     retryFailure,
			identifier:    identifier,
			error:         "stall_timeout",
			workerHost:    workerHost,
			workspacePath: wsPath,
		})
	}
}

func (rt *OrchestratorRuntime) shouldDispatchIssue(issue *tracker.Issue, settings *config.Settings) bool {
	if issue.ID == "" || issue.Identifier == "" || issue.Title == "" || issue.State == "" {
		return false
	}
	if !tracker.IsActiveState(issue, settings) {
		return false
	}
	if tracker.IsTerminalState(issue, settings) {
		return false
	}
	if _, running := rt.state.Running[issue.ID]; running {
		return false
	}
	if _, claimed := rt.state.Claimed[issue.ID]; claimed {
		return false
	}
	if !issue.AssignedToWorker {
		return false
	}

	// Global capacity
	if len(rt.state.Running) >= settings.Agent.MaxConcurrentAgents {
		return false
	}

	// Per-state capacity
	stateLimit := settings.MaxConcurrentAgentsForState(issue.State)
	stateCount := 0
	normalizedState := config.NormalizeIssueState(issue.State)
	for _, entry := range rt.state.Running {
		if config.NormalizeIssueState(entry.Issue.State) == normalizedState {
			stateCount++
		}
	}
	if stateCount >= stateLimit {
		return false
	}

	// Blocker check for "todo" state
	if config.NormalizeIssueState(issue.State) == "todo" {
		for _, blocker := range issue.BlockedBy {
			if blocker.State != nil && !isTerminalStateStr(*blocker.State, settings) {
				return false
			}
		}
	}

	return true
}

func (rt *OrchestratorRuntime) isDispatchEligible(issue *tracker.Issue, settings *config.Settings) bool {
	if issue.ID == "" || issue.Identifier == "" || issue.Title == "" || issue.State == "" {
		return false
	}
	if !tracker.IsActiveState(issue, settings) {
		return false
	}
	if tracker.IsTerminalState(issue, settings) {
		return false
	}
	return true
}

func (rt *OrchestratorRuntime) dispatchIssue(ctx context.Context, issue tracker.Issue, attempt *int, workerHost string, settings *config.Settings, t tracker.Tracker) {
	slog.Info("dispatching issue", "issue_id", issue.ID, "identifier", issue.Identifier, "state", issue.State)

	// Claim
	rt.state.Claimed[issue.ID] = struct{}{}
	delete(rt.state.RetryAttempts, issue.ID)

	// Create running entry
	childCtx, cancel := context.WithCancel(ctx)
	entry := &RunningEntry{
		Identifier: issue.Identifier,
		Issue:      issue,
		StartedAt:  time.Now().UTC(),
		Attempt:    attempt,
		Cancel:     cancel,
	}
	rt.state.Running[issue.ID] = entry

	// Launch worker in goroutine
	settingsCopy := *settings
	prompt := rt.workflowStore.Current().PromptTemplate
	workerEvents := rt.workerEvents
	issueCopy := issue

	go agent.Run(childCtx, issueCopy, &settingsCopy, prompt, t, workerEvents, workerHost, attempt)
}

func (rt *OrchestratorRuntime) terminateRunningIssue(issueID string, terminal bool, settings *config.Settings) {
	entry, ok := rt.state.Running[issueID]
	if !ok {
		return
	}

	// Accumulate token totals
	rt.accumulateTokens(entry)

	if entry.Cancel != nil {
		entry.Cancel()
	}
	delete(rt.state.Running, issueID)

	if terminal {
		rt.state.Completed[issueID] = struct{}{}
		delete(rt.state.Claimed, issueID)
		// Remove workspace
		go func() {
			ctx := context.Background()
			workspace.RemoveIssueWorkspace(ctx, entry.Identifier, settings, entry.WorkerHost)
		}()
	} else {
		delete(rt.state.Claimed, issueID)
	}
}

func (rt *OrchestratorRuntime) handleWorkerEvent(ctx context.Context, event agent.WorkerEvent) {
	switch event.Type {
	case agent.EventRuntimeInfo:
		if entry, ok := rt.state.Running[event.IssueID]; ok && event.Runtime != nil {
			entry.WorkerHost = event.Runtime.WorkerHost
			entry.WorkspacePath = event.Runtime.WorkspacePath
		}

	case agent.EventCodexUpdate:
		if entry, ok := rt.state.Running[event.IssueID]; ok && event.Update != nil {
			update := event.Update
			entry.LastCodexEvent = update.Event
			ts := update.Timestamp
			entry.LastCodexTimestamp = &ts
			entry.LastCodexMessage = update.Payload

			if update.SessionID != "" {
				entry.SessionID = update.SessionID
			}
			if update.CodexAppServerPID != "" {
				entry.CodexAppServerPID = update.CodexAppServerPID
			}

			// Token accounting from payload
			rt.processTokenUpdate(entry, update)

			if update.Event == "session_started" {
				entry.TurnCount++
			}
		}

	case agent.EventExit:
		entry, ok := rt.state.Running[event.IssueID]
		if !ok {
			return
		}

		// Accumulate tokens
		rt.accumulateTokens(entry)

		if entry.Cancel != nil {
			entry.Cancel()
		}
		delete(rt.state.Running, event.IssueID)

		if event.ExitNormal {
			// Continuation retry with short delay
			rt.scheduleRetry(retryRequest{
				issueID:       event.IssueID,
				attempt:       nextRetryAttempt(entry.Attempt),
				retryKind:     retryContinuation,
				identifier:    entry.Identifier,
				workerHost:    entry.WorkerHost,
				workspacePath: entry.WorkspacePath,
			})
		} else if event.Err != nil {
			errMsg := event.Err.Error()
			slog.Warn("worker exited with error", "issue_id", event.IssueID, "identifier", entry.Identifier, "error", errMsg)
			rt.scheduleRetry(retryRequest{
				issueID:       event.IssueID,
				attempt:       nextRetryAttempt(entry.Attempt),
				retryKind:     retryFailure,
				identifier:    entry.Identifier,
				error:         errMsg,
				workerHost:    entry.WorkerHost,
				workspacePath: entry.WorkspacePath,
			})
		} else {
			delete(rt.state.Claimed, event.IssueID)
		}
	}
}

func (rt *OrchestratorRuntime) processTokenUpdate(entry *RunningEntry, update *codex.CodexUpdate) {
	if update.Payload == nil {
		return
	}
	payloadMap, ok := update.Payload.(map[string]any)
	if !ok {
		return
	}

	// Look for token usage in various locations
	var inputTokens, outputTokens, totalTokens int64
	found := false

	// Check params.tokenUsage or params.total_token_usage
	if params, ok := payloadMap["params"].(map[string]any); ok {
		if tu, ok := params["tokenUsage"].(map[string]any); ok {
			inputTokens = jsonInt64(tu, "inputTokens")
			outputTokens = jsonInt64(tu, "outputTokens")
			totalTokens = jsonInt64(tu, "totalTokens")
			found = true
		}
		if tu, ok := params["total_token_usage"].(map[string]any); ok {
			inputTokens = jsonInt64(tu, "input_tokens")
			outputTokens = jsonInt64(tu, "output_tokens")
			totalTokens = jsonInt64(tu, "total_tokens")
			found = true
		}
	}

	if found {
		// Use absolute totals, track deltas
		entry.CodexInputTokens = inputTokens
		entry.CodexOutputTokens = outputTokens
		entry.CodexTotalTokens = totalTokens
	}
}

func (rt *OrchestratorRuntime) accumulateTokens(entry *RunningEntry) {
	// Delta-based accumulation
	inputDelta := entry.CodexInputTokens - entry.CodexLastReportedInput
	outputDelta := entry.CodexOutputTokens - entry.CodexLastReportedOutput
	totalDelta := entry.CodexTotalTokens - entry.CodexLastReportedTotal

	if inputDelta > 0 {
		rt.state.CodexTotals.InputTokens += inputDelta
	}
	if outputDelta > 0 {
		rt.state.CodexTotals.OutputTokens += outputDelta
	}
	if totalDelta > 0 {
		rt.state.CodexTotals.TotalTokens += totalDelta
	}

	elapsed := time.Since(entry.StartedAt).Seconds()
	rt.state.CodexTotals.SecondsRunning += int64(elapsed)

	entry.CodexLastReportedInput = entry.CodexInputTokens
	entry.CodexLastReportedOutput = entry.CodexOutputTokens
	entry.CodexLastReportedTotal = entry.CodexTotalTokens
}

type retryKind int

const (
	retryContinuation retryKind = iota
	retryFailure
)

type retryRequest struct {
	issueID       string
	attempt       int
	retryKind     retryKind
	identifier    string
	error         string
	workerHost    string
	workspacePath string
}

func (rt *OrchestratorRuntime) scheduleRetry(req retryRequest) {
	delay := calculateRetryDelay(req.retryKind, req.attempt, rt.state.MaxRetryBackoffMs)

	rt.state.retryTokenCounter++
	token := rt.state.retryTokenCounter

	entry := &RetryEntry{
		IssueID:       req.issueID,
		Identifier:    req.identifier,
		Attempt:       req.attempt,
		DueAtMs:       nowMillis() + delay,
		Error:         req.error,
		WorkerHost:    req.workerHost,
		WorkspacePath: req.workspacePath,
		Token:         token,
	}
	rt.state.RetryAttempts[req.issueID] = entry
	rt.state.Claimed[req.issueID] = struct{}{}

	slog.Info("scheduled retry",
		"issue_id", req.issueID,
		"identifier", req.identifier,
		"attempt", req.attempt,
		"delay_ms", delay,
	)
}

func calculateRetryDelay(kind retryKind, attempt int, maxBackoffMs int64) int64 {
	if kind == retryContinuation {
		return continuationRetryDelayMs
	}
	// Exponential backoff: min(10000 * 2^(attempt-1), max_retry_backoff_ms)
	exponent := attempt - 1
	if exponent < 0 {
		exponent = 0
	}
	if exponent > 10 {
		exponent = 10
	}
	delay := int64(failureRetryBaseMs) * int64(math.Pow(2, float64(exponent)))
	if delay > maxBackoffMs {
		delay = maxBackoffMs
	}
	return delay
}

func (rt *OrchestratorRuntime) buildSnapshot() Snapshot {
	now := nowMillis()
	var running []RunningSnapshot
	for issueID, entry := range rt.state.Running {
		rs := RunningSnapshot{
			IssueID:           issueID,
			Identifier:        entry.Identifier,
			State:             entry.Issue.State,
			WorkerHost:        entry.WorkerHost,
			WorkspacePath:     entry.WorkspacePath,
			SessionID:         entry.SessionID,
			CodexAppServerPID: entry.CodexAppServerPID,
			CodexInputTokens:  entry.CodexInputTokens,
			CodexOutputTokens: entry.CodexOutputTokens,
			CodexTotalTokens:  entry.CodexTotalTokens,
			TurnCount:         entry.TurnCount,
			StartedAt:         entry.StartedAt,
			LastCodexTimestamp: entry.LastCodexTimestamp,
			LastCodexMessage:   entry.LastCodexMessage,
			LastCodexEvent:    entry.LastCodexEvent,
			RuntimeSeconds:    int64(time.Since(entry.StartedAt).Seconds()),
		}
		running = append(running, rs)
	}
	sort.Slice(running, func(i, j int) bool {
		return running[i].Identifier < running[j].Identifier
	})

	var retrying []RetrySnapshot
	for _, entry := range rt.state.RetryAttempts {
		dueIn := entry.DueAtMs - now
		if dueIn < 0 {
			dueIn = 0
		}
		retrying = append(retrying, RetrySnapshot{
			IssueID:       entry.IssueID,
			Attempt:       entry.Attempt,
			DueInMs:       dueIn,
			Identifier:    entry.Identifier,
			Error:         entry.Error,
			WorkerHost:    entry.WorkerHost,
			WorkspacePath: entry.WorkspacePath,
		})
	}
	sort.Slice(retrying, func(i, j int) bool {
		return retrying[i].DueInMs < retrying[j].DueInMs
	})

	var nextPoll *int64
	if rt.nextPollDueAt > 0 {
		remaining := rt.nextPollDueAt - now
		if remaining < 0 {
			remaining = 0
		}
		nextPoll = &remaining
	}

	return Snapshot{
		Running:     running,
		Retrying:    retrying,
		CodexTotals: rt.state.CodexTotals,
		RateLimits:  rt.state.CodexRateLimits,
		Polling: PollingSnapshot{
			Checking:       rt.pollInProgress,
			NextPollInMs:   nextPoll,
			PollIntervalMs: rt.state.PollIntervalMs,
		},
	}
}

// --- helpers ---

func sortIssuesForDispatch(issues []tracker.Issue) {
	sort.Slice(issues, func(i, j int) bool {
		pi := issues[i].Priority
		pj := issues[j].Priority
		// Priority: asc, nulls last
		if pi == nil && pj != nil {
			return false
		}
		if pi != nil && pj == nil {
			return true
		}
		if pi != nil && pj != nil && *pi != *pj {
			return *pi < *pj
		}
		// Created at: asc
		if issues[i].CreatedAt != nil && issues[j].CreatedAt != nil {
			if !issues[i].CreatedAt.Equal(*issues[j].CreatedAt) {
				return issues[i].CreatedAt.Before(*issues[j].CreatedAt)
			}
		}
		// Identifier: lexicographic
		return issues[i].Identifier < issues[j].Identifier
	})
}

func nextRetryAttempt(current *int) int {
	if current == nil {
		return 1
	}
	return *current + 1
}

func isTerminalStateStr(state string, settings *config.Settings) bool {
	normalized := config.NormalizeIssueState(state)
	for _, s := range settings.Tracker.TerminalStates {
		if config.NormalizeIssueState(s) == normalized {
			return true
		}
	}
	return false
}

func nowMillis() int64 {
	return time.Now().UnixMilli()
}

func jsonInt64(m map[string]any, key string) int64 {
	if v, ok := m[key].(float64); ok {
		return int64(v)
	}
	if v, ok := m[key].(int64); ok {
		return v
	}
	if v, ok := m[key].(int); ok {
		return int64(v)
	}
	return 0
}
