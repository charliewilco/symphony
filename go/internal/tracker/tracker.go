// Package tracker defines the issue model, tracker interface, and factory.
package tracker

import (
	"context"
	"strings"
	"time"

	"symphony/internal/config"
)

// BlockerRef represents a blocking issue reference.
type BlockerRef struct {
	ID         *string `json:"id,omitempty"`
	Identifier *string `json:"identifier,omitempty"`
	State      *string `json:"state,omitempty"`
}

// Issue is the normalized issue model used throughout the system.
type Issue struct {
	ID               string      `json:"id"`
	Identifier       string      `json:"identifier"`
	Title            string      `json:"title"`
	Description      *string     `json:"description,omitempty"`
	Priority         *int        `json:"priority,omitempty"`
	State            string      `json:"state"`
	BranchName       *string     `json:"branch_name,omitempty"`
	URL              *string     `json:"url,omitempty"`
	Labels           []string    `json:"labels"`
	BlockedBy        []BlockerRef `json:"blocked_by"`
	AssignedToWorker bool        `json:"assigned_to_worker"`
	CreatedAt        *time.Time  `json:"created_at,omitempty"`
	UpdatedAt        *time.Time  `json:"updated_at,omitempty"`
	AssigneeID       *string     `json:"assignee_id,omitempty"`
	AssigneeEmail    *string     `json:"assignee_email,omitempty"`
}

// ToLiquidObject converts an issue to a map for liquid template rendering.
func (i *Issue) ToLiquidObject() map[string]any {
	obj := map[string]any{
		"id":                i.ID,
		"identifier":        i.Identifier,
		"title":             i.Title,
		"state":             i.State,
		"assigned_to_worker": i.AssignedToWorker,
	}
	if i.Description != nil {
		obj["description"] = *i.Description
	}
	if i.Priority != nil {
		obj["priority"] = *i.Priority
	}
	if i.BranchName != nil {
		obj["branch_name"] = *i.BranchName
	}
	if i.URL != nil {
		obj["url"] = *i.URL
	}
	if i.Labels != nil {
		obj["labels"] = i.Labels
	} else {
		obj["labels"] = []string{}
	}
	if i.CreatedAt != nil {
		obj["created_at"] = i.CreatedAt.Format(time.RFC3339)
	}
	if i.UpdatedAt != nil {
		obj["updated_at"] = i.UpdatedAt.Format(time.RFC3339)
	}

	blockers := make([]map[string]any, 0, len(i.BlockedBy))
	for _, b := range i.BlockedBy {
		bm := map[string]any{}
		if b.ID != nil {
			bm["id"] = *b.ID
		}
		if b.Identifier != nil {
			bm["identifier"] = *b.Identifier
		}
		if b.State != nil {
			bm["state"] = *b.State
		}
		blockers = append(blockers, bm)
	}
	obj["blocked_by"] = blockers
	return obj
}

// Tracker defines the interface for fetching issues from a tracker backend.
type Tracker interface {
	FetchCandidateIssues(ctx context.Context, settings *config.Settings) ([]Issue, error)
	FetchIssuesByStates(ctx context.Context, states []string, settings *config.Settings) ([]Issue, error)
	FetchIssueStatesByIDs(ctx context.Context, ids []string, settings *config.Settings) ([]Issue, error)
	GraphQL(ctx context.Context, query string, variables map[string]any, settings *config.Settings) (map[string]any, error)
}

// TrackerForSettings returns the appropriate tracker for the settings.
func TrackerForSettings(settings *config.Settings) Tracker {
	switch settings.Tracker.Kind {
	case "memory":
		return NewMemoryTracker()
	default:
		return NewLinearTracker()
	}
}

// IsActiveState checks if a state is in the active states list (normalized).
func IsActiveState(issue *Issue, settings *config.Settings) bool {
	normalized := config.NormalizeIssueState(issue.State)
	for _, s := range settings.Tracker.ActiveStates {
		if config.NormalizeIssueState(s) == normalized {
			return true
		}
	}
	return false
}

// IsTerminalState checks if a state is in the terminal states list (normalized).
func IsTerminalState(issue *Issue, settings *config.Settings) bool {
	normalized := config.NormalizeIssueState(issue.State)
	for _, s := range settings.Tracker.TerminalStates {
		if config.NormalizeIssueState(s) == normalized {
			return true
		}
	}
	return false
}

// NormalizeAssigneeMatchValue normalizes an assignee value for matching.
func NormalizeAssigneeMatchValue(value string) string {
	return strings.TrimSpace(value)
}
