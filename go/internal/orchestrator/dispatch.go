package orchestrator

import (
	"sort"

	"github.com/openai/symphony/go/internal/config"
	"github.com/openai/symphony/go/internal/tracker"
)

func EligibleIssues(issues []tracker.Issue, cfg config.Config, running map[string]RunningIssue, claimed map[string]bool) []tracker.Issue {
	out := []tracker.Issue{}
	active := set(cfg.Tracker.ActiveStates)
	term := set(cfg.Tracker.TerminalStates)
	for _, i := range issues {
		if i.ID == "" || i.Identifier == "" || i.Title == "" || i.State == "" {
			continue
		}
		if !active[i.State] || term[i.State] {
			continue
		}
		if running[i.ID].IssueID != "" || claimed[i.ID] {
			continue
		}
		if i.State == "Todo" {
			blocked := false
			for _, b := range i.Blockers {
				if !term[b.State] {
					blocked = true
					break
				}
			}
			if blocked {
				continue
			}
		}
		out = append(out, i)
	}
	sort.Slice(out, func(a, b int) bool {
		pa, pb := out[a].Priority, out[b].Priority
		if pa == nil && pb != nil {
			return false
		}
		if pa != nil && pb == nil {
			return true
		}
		if pa != nil && pb != nil && *pa != *pb {
			return *pa < *pb
		}
		if out[a].CreatedAtUnix != out[b].CreatedAtUnix {
			return out[a].CreatedAtUnix < out[b].CreatedAtUnix
		}
		return out[a].Identifier < out[b].Identifier
	})
	return out
}

func set(items []string) map[string]bool {
	m := map[string]bool{}
	for _, i := range items {
		m[i] = true
	}
	return m
}
