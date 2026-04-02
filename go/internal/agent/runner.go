package agent

import (
	"context"
	"strings"

	"github.com/openai/symphony/go/internal/tracker"
	"github.com/openai/symphony/go/internal/workflow"
)

type Outcome int

const (
	OutcomeDone Outcome = iota
	OutcomeContinue
	OutcomeBlocked
	OutcomeFailed
)

type Result struct {
	Outcome Outcome
	Error   error
	Reason  string
}

type Runner struct {
	Codex     Client
	Tracker   tracker.Tracker
	Workflow  *workflow.Template
	MaxTurns  int
	BeforeRun func(context.Context) error
	AfterRun  func(context.Context) error
}

func (r Runner) Run(ctx context.Context, issue tracker.Issue, attempt int) Result {
	if r.BeforeRun != nil {
		if err := r.BeforeRun(ctx); err != nil {
			return Result{Outcome: OutcomeFailed, Error: err}
		}
	}
	defer func() {
		if r.AfterRun != nil {
			_ = r.AfterRun(context.Background())
		}
	}()
	var last string
	for turn := 0; turn < r.MaxTurns; turn++ {
		select {
		case <-ctx.Done():
			return Result{Outcome: OutcomeDone}
		default:
		}
		ap := &attempt
		if attempt == 0 {
			ap = nil
		}
		prompt, err := r.Workflow.Render(workflow.IssueData{Identifier: issue.Identifier, Title: issue.Title, State: issue.State, Description: issue.Description, Labels: issue.Labels, URL: issue.URL, BranchName: issue.BranchName}, ap)
		if err != nil {
			return Result{Outcome: OutcomeFailed, Error: err}
		}
		if turn > 0 {
			prompt = "Continue the previous work session for this issue."
		}
		res, err := r.Codex.RunTurn(ctx, prompt)
		if err != nil {
			return Result{Outcome: OutcomeFailed, Error: err}
		}
		last = res.LastOutput
		fresh, err := r.Tracker.GetIssue(ctx, issue.ID)
		if err != nil {
			return Result{Outcome: OutcomeFailed, Error: err}
		}
		if fresh.State != "Todo" && fresh.State != "In Progress" && fresh.State != "Merging" && fresh.State != "Rework" {
			return Result{Outcome: OutcomeDone}
		}
	}
	l := strings.ToLower(last)
	if strings.Contains(l, "blocked") || strings.Contains(l, "unable to proceed") {
		return Result{Outcome: OutcomeBlocked, Reason: last}
	}
	return Result{Outcome: OutcomeContinue}
}
