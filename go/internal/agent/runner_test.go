package agent

import (
	"context"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"github.com/openai/symphony/go/internal/tracker"
	"github.com/openai/symphony/go/internal/workflow"
)

func TestCancelRunsAfterHook(t *testing.T) {
	d := t.TempDir()
	wp := filepath.Join(d, "WORKFLOW.md")
	_ = os.WriteFile(wp, []byte("{{issue.identifier}}"), 0o644)
	tpl, _ := workflow.Load(wp)
	tr := tracker.NewMemory([]tracker.Issue{{ID: "1", Identifier: "A-1", Title: "x", State: "Todo"}})
	var after atomic.Bool
	r := Runner{Codex: MockCodex{Delay: 50 * time.Millisecond, Output: "ok"}, Tracker: tr, Workflow: tpl, MaxTurns: 3, AfterRun: func(context.Context) error { after.Store(true); return nil }}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_ = r.Run(ctx, tracker.Issue{ID: "1", Identifier: "A-1", Title: "x", State: "Todo"}, 0)
	if !after.Load() {
		t.Fatal("expected after_run")
	}
}
