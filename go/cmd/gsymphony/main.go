package main

import (
	"context"
	"flag"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"github.com/openai/symphony/go/internal/agent"
	"github.com/openai/symphony/go/internal/config"
	"github.com/openai/symphony/go/internal/orchestrator"
	"github.com/openai/symphony/go/internal/server"
	"github.com/openai/symphony/go/internal/tracker"
	"github.com/openai/symphony/go/internal/workflow"
)

func main() {
	if len(os.Args) > 1 && os.Args[1] == "validate" {
		os.Exit(runValidate(os.Args[2:]))
	}
	os.Exit(run(os.Args[1:]))
}

func runValidate(args []string) int {
	fs := flag.NewFlagSet("validate", flag.ContinueOnError)
	jsonOut := fs.Bool("json", false, "json output")
	configPath := fs.String("config", "./.symphony.toml", "config path")
	_ = fs.Parse(args)
	cfg, err := config.Load(*configPath)
	if err != nil {
		fmt.Println(err)
		return 1
	}
	ds := config.Validate(cfg, *configPath)
	if *jsonOut {
		out, _ := config.DiagnosticsJSON(ds)
		fmt.Println(out)
	} else {
		for _, d := range ds {
			fmt.Printf("%s: %s (%s)\n", d.Code, d.Message, d.FieldPath)
		}
	}
	if len(ds) > 0 {
		return 1
	}
	return 0
}

func run(args []string) int {
	fs := flag.NewFlagSet("gsymphony", flag.ContinueOnError)
	guard := fs.Bool("i-understand-that-this-will-be-running-without-the-usual-guardrails", false, "ack")
	yolo := fs.Bool("yolo", false, "ack alias")
	configPath := fs.String("config", "./.symphony.toml", "config")
	port := fs.Int("port", 0, "port")
	_ = fs.String("logs-root", "./logs", "logs root")
	_ = fs.Parse(args)
	if !*guard && !*yolo {
		fmt.Println("missing guardrails acknowledgment flag")
		return 2
	}
	workflowPath := "./WORKFLOW.md"
	if fs.NArg() > 0 {
		workflowPath = fs.Arg(0)
	}
	logger := slog.New(slog.NewJSONHandler(os.Stdout, nil))
	cfg, err := config.Load(*configPath)
	if err != nil {
		logger.Error("load config", "err", err)
		return 1
	}
	if ds := config.Validate(cfg, *configPath); len(ds) > 0 {
		logger.Error("invalid config", "count", len(ds))
		return 1
	}
	tpl, err := workflow.Load(workflowPath)
	if err != nil {
		logger.Error("load workflow", "err", err)
		return 1
	}
	tr := tracker.NewMemory(nil)
	runner := agent.Runner{Codex: agent.MockCodex{Output: "done"}, Tracker: tr, Workflow: tpl, MaxTurns: cfg.Agent.MaxTurns}
	orch := orchestrator.New(cfg, tr, runner, logger)
	ctx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()
	if *port > 0 {
		api := server.API{Orch: orch, Refresh: func() { _ = orch.Tick(ctx) }}
		go func() { _ = http.ListenAndServe(fmt.Sprintf(":%d", *port), api.Handler()) }()
	}
	t := time.NewTicker(time.Duration(cfg.Polling.IntervalMS) * time.Millisecond)
	defer t.Stop()
	stateFile := orchestrator.StatePath(filepath.Clean(cfg.Workspace.Root))
	for {
		select {
		case <-ctx.Done():
			_ = os.Remove(stateFile)
			return 0
		case <-t.C:
			_ = orch.Tick(ctx)
			s := orch.Snapshot()
			claimed := make([]string, 0, len(s.Running))
			for _, r := range s.Running {
				claimed = append(claimed, r.IssueID)
			}
			_ = orchestrator.SaveState(stateFile, orchestrator.PersistedState{RetryCounts: map[string]int{}, ClaimedIssues: claimed, TokenTotals: s.TokenTotals})
		}
	}
}
