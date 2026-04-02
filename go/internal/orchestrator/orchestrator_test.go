package orchestrator

import (
	"context"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/openai/symphony/go/internal/agent"
	"github.com/openai/symphony/go/internal/config"
	"github.com/openai/symphony/go/internal/tracker"
	"github.com/openai/symphony/go/internal/workflow"
)

func TestRetryBackoff(t *testing.T) {
	r := NewRetryManager(30 * time.Second)
	e := r.Schedule("1", 1, RetryFailure, "x", time.Now())
	if e.ReadyAt.Sub(time.Now()) < 9*time.Second {
		t.Fatal("expected backoff")
	}
}

func TestBranchLockConflictBlocksDispatch(t *testing.T) {
	cfg := config.DefaultConfig()
	cfg.Agent.MaxConcurrentAgents = 2
	tr := tracker.NewMemory([]tracker.Issue{{ID: "1", Identifier: "A-1", Title: "a", State: "Todo", RepositoryURL: "u", BranchName: "b"}, {ID: "2", Identifier: "A-2", Title: "b", State: "Todo", RepositoryURL: "u", BranchName: "b"}})
	d := t.TempDir()
	wp := filepath.Join(d, "WORKFLOW.md")
	_ = os.WriteFile(wp, []byte("{{issue.identifier}}"), 0o644)
	tpl, _ := workflow.Load(wp)
	runner := agent.Runner{Codex: agent.MockCodex{Output: "ok"}, Tracker: tr, Workflow: tpl, MaxTurns: 1}
	s := New(cfg, tr, runner, slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err := s.Tick(context.Background()); err != nil {
		t.Fatal(err)
	}
	time.Sleep(50 * time.Millisecond)
	snap := s.Snapshot()
	if len(snap.BranchLocks) > 1 {
		t.Fatal("lock conflict should limit dispatch")
	}
}

func TestStateRoundTrip(t *testing.T) {
	p := filepath.Join(t.TempDir(), "state.json")
	in := PersistedState{RetryCounts: map[string]int{"a": 2}}
	if err := SaveState(p, in); err != nil {
		t.Fatal(err)
	}
	out, err := LoadState(p)
	if err != nil || out.RetryCounts["a"] != 2 {
		t.Fatalf("roundtrip failed: %v", err)
	}
}
