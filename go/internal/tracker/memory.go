package tracker

import (
	"context"
	"sync"

	"symphony/internal/config"
)

// MemoryTracker is an in-memory tracker implementation for testing.
type MemoryTracker struct {
	mu     sync.RWMutex
	issues map[string]Issue
}

// NewMemoryTracker creates a new empty memory tracker.
func NewMemoryTracker() *MemoryTracker {
	return &MemoryTracker{
		issues: make(map[string]Issue),
	}
}

// SetIssues replaces all issues in the memory tracker.
func (mt *MemoryTracker) SetIssues(issues []Issue) {
	mt.mu.Lock()
	defer mt.mu.Unlock()
	mt.issues = make(map[string]Issue, len(issues))
	for _, issue := range issues {
		mt.issues[issue.ID] = issue
	}
}

func (mt *MemoryTracker) FetchCandidateIssues(ctx context.Context, settings *config.Settings) ([]Issue, error) {
	mt.mu.RLock()
	defer mt.mu.RUnlock()
	var result []Issue
	for _, issue := range mt.issues {
		for _, activeState := range settings.Tracker.ActiveStates {
			if config.NormalizeIssueState(activeState) == config.NormalizeIssueState(issue.State) {
				result = append(result, issue)
				break
			}
		}
	}
	return result, nil
}

func (mt *MemoryTracker) FetchIssuesByStates(ctx context.Context, states []string, settings *config.Settings) ([]Issue, error) {
	mt.mu.RLock()
	defer mt.mu.RUnlock()
	var result []Issue
	for _, issue := range mt.issues {
		for _, state := range states {
			if config.NormalizeIssueState(state) == config.NormalizeIssueState(issue.State) {
				result = append(result, issue)
				break
			}
		}
	}
	return result, nil
}

func (mt *MemoryTracker) FetchIssueStatesByIDs(ctx context.Context, ids []string, settings *config.Settings) ([]Issue, error) {
	mt.mu.RLock()
	defer mt.mu.RUnlock()
	var result []Issue
	for _, id := range ids {
		if issue, ok := mt.issues[id]; ok {
			result = append(result, issue)
		}
	}
	return result, nil
}

func (mt *MemoryTracker) GraphQL(ctx context.Context, query string, variables map[string]any, settings *config.Settings) (map[string]any, error) {
	return map[string]any{"data": map[string]any{}}, nil
}

// UpdateIssueState sets the state for a given issue (test helper).
func (mt *MemoryTracker) UpdateIssueState(issueID, stateName string) error {
	mt.mu.Lock()
	defer mt.mu.Unlock()
	if issue, ok := mt.issues[issueID]; ok {
		issue.State = stateName
		mt.issues[issueID] = issue
		return nil
	}
	return nil
}
