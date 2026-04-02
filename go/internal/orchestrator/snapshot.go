package orchestrator

import "time"

type RunningSnapshot struct {
	IssueID    string    `json:"issue_id"`
	Identifier string    `json:"identifier"`
	State      string    `json:"state"`
	Since      time.Time `json:"since"`
}
type Snapshot struct {
	GeneratedAt time.Time             `json:"generated_at"`
	Running     []RunningSnapshot     `json:"running"`
	Retry       map[string]RetryEntry `json:"retry"`
	BranchLocks map[string]string     `json:"branch_locks"`
	TokenTotals TokenTotals           `json:"token_totals"`
}
