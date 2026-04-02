package orchestrator

import "time"

type RetryType string

const (
	RetryContinuation RetryType = "continuation"
	RetryFailure      RetryType = "failure"
)

type RetryEntry struct {
	IssueID string
	Attempt int
	Type    RetryType
	ReadyAt time.Time
	Token   int64
	Reason  string
}

type RetryManager struct {
	entries    map[string]RetryEntry
	seq        int64
	maxBackoff time.Duration
}

func NewRetryManager(maxBackoff time.Duration) *RetryManager {
	return &RetryManager{entries: map[string]RetryEntry{}, maxBackoff: maxBackoff}
}

func (r *RetryManager) Schedule(issueID string, attempt int, rt RetryType, reason string, now time.Time) RetryEntry {
	r.seq++
	d := time.Second
	if rt == RetryFailure {
		d = 10 * time.Second * time.Duration(1<<(max(attempt, 1)-1))
		if r.maxBackoff > 0 && d > r.maxBackoff {
			d = r.maxBackoff
		}
	}
	e := RetryEntry{IssueID: issueID, Attempt: attempt, Type: rt, ReadyAt: now.Add(d), Token: r.seq, Reason: reason}
	r.entries[issueID] = e
	return e
}

func (r *RetryManager) Cancel(issueID string) { delete(r.entries, issueID) }
func (r *RetryManager) DispatchReady(now time.Time) []RetryEntry {
	out := []RetryEntry{}
	for k, e := range r.entries {
		if !now.Before(e.ReadyAt) {
			out = append(out, e)
			delete(r.entries, k)
		}
	}
	return out
}

func max(a, b int) int {
	if a > b {
		return a
	}
	return b
}
