package orchestrator

import (
	"context"
	"log/slog"
	"sync"
	"time"

	"github.com/openai/symphony/go/internal/agent"
	"github.com/openai/symphony/go/internal/config"
	"github.com/openai/symphony/go/internal/tracker"
)

type RunningIssue struct {
	IssueID    string
	Identifier string
	State      string
	StartedAt  time.Time
	Cancel     context.CancelFunc
	Done       chan struct{}
}

type Service struct {
	mu          sync.Mutex
	cfg         config.Config
	tracker     tracker.Tracker
	runner      agent.Runner
	log         *slog.Logger
	running     map[string]RunningIssue
	claimed     map[string]bool
	retry       *RetryManager
	retryCounts map[string]int
	branchLocks map[string]string
	totals      TokenTotals
}

func New(cfg config.Config, tr tracker.Tracker, r agent.Runner, log *slog.Logger) *Service {
	return &Service{cfg: cfg, tracker: tr, runner: r, log: log, running: map[string]RunningIssue{}, claimed: map[string]bool{}, retry: NewRetryManager(time.Duration(cfg.Agent.MaxRetryBackoffMS) * time.Millisecond), retryCounts: map[string]int{}, branchLocks: map[string]string{}}
}

func (s *Service) Tick(ctx context.Context) error {
	issues, err := s.tracker.ListIssues(ctx)
	if err != nil {
		return err
	}
	s.mu.Lock()
	eligible := EligibleIssues(issues, s.cfg, s.running, s.claimed)
	for _, i := range eligible {
		if len(s.running) >= s.cfg.Agent.MaxConcurrentAgents {
			break
		}
		if m, ok := s.cfg.Agent.MaxConcurrentAgentsByState[i.State]; ok && countState(s.running, i.State) >= m {
			continue
		}
		if i.RepositoryURL != "" && i.BranchName != "" {
			k := i.RepositoryURL + ":" + i.BranchName
			if holder, ok := s.branchLocks[k]; ok && holder != i.ID {
				s.log.Warn("branch lock conflict", "issue", i.ID, "locked_by", holder)
				continue
			}
			s.branchLocks[k] = i.ID
		}
		s.claimed[i.ID] = true
		actx, cancel := context.WithCancel(ctx)
		r := RunningIssue{IssueID: i.ID, Identifier: i.Identifier, State: i.State, StartedAt: time.Now(), Cancel: cancel, Done: make(chan struct{})}
		s.running[i.ID] = r
		attempt := s.retryCounts[i.ID]
		go s.runIssue(actx, i, attempt, r.Done)
	}
	s.mu.Unlock()
	return nil
}

func (s *Service) runIssue(ctx context.Context, issue tracker.Issue, attempt int, done chan struct{}) {
	defer close(done)
	res := s.runner.Run(ctx, issue, attempt)
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.running, issue.ID)
	for k, v := range s.branchLocks {
		if v == issue.ID {
			delete(s.branchLocks, k)
		}
	}
	s.claimed[issue.ID] = false
	s.retryCounts[issue.ID] = attempt + 1
	switch res.Outcome {
	case agent.OutcomeDone:
		delete(s.retryCounts, issue.ID)
	case agent.OutcomeContinue:
		s.retry.Schedule(issue.ID, s.retryCounts[issue.ID], RetryContinuation, "continue", time.Now())
	case agent.OutcomeBlocked:
		s.retry.Schedule(issue.ID, s.retryCounts[issue.ID], RetryFailure, res.Reason, time.Now())
	case agent.OutcomeFailed:
		s.retry.Schedule(issue.ID, s.retryCounts[issue.ID], RetryFailure, "failed", time.Now())
	}
}

func (s *Service) StopIssue(id string) {
	s.mu.Lock()
	r, ok := s.running[id]
	s.mu.Unlock()
	if !ok {
		return
	}
	r.Cancel()
	select {
	case <-r.Done:
		return
	case <-time.After(30 * time.Second):
	}
}

func (s *Service) Snapshot() Snapshot {
	s.mu.Lock()
	defer s.mu.Unlock()
	r := make([]RunningSnapshot, 0, len(s.running))
	for _, v := range s.running {
		r = append(r, RunningSnapshot{IssueID: v.IssueID, Identifier: v.Identifier, State: v.State, Since: v.StartedAt})
	}
	rm := map[string]RetryEntry{}
	for k, v := range s.retry.entries {
		rm[k] = v
	}
	bl := map[string]string{}
	for k, v := range s.branchLocks {
		bl[k] = v
	}
	return Snapshot{GeneratedAt: time.Now(), Running: r, Retry: rm, BranchLocks: bl, TokenTotals: s.totals}
}

func countState(r map[string]RunningIssue, state string) int {
	c := 0
	for _, v := range r {
		if v.State == state {
			c++
		}
	}
	return c
}
