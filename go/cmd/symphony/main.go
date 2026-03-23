// Package main is the CLI entrypoint for the Symphony orchestrator service.
package main

import (
	"context"
	"flag"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"syscall"
	"time"

	"symphony/internal/config"
	"symphony/internal/dashboard"
	"symphony/internal/httpserver"
	"symphony/internal/logfile"
	"symphony/internal/orchestrator"
	"symphony/internal/workflow"
)

func main() {
	var (
		guardrailsFlag bool
		yoloFlag       bool
		logsRoot       string
		port           int
	)

	flag.BoolVar(&guardrailsFlag, "i-understand-that-this-will-be-running-without-the-usual-guardrails", false, "Required acknowledgement flag")
	flag.BoolVar(&yoloFlag, "yolo", false, "Alias for guardrails acknowledgement")
	flag.StringVar(&logsRoot, "logs-root", "", "Path for log files")
	flag.IntVar(&port, "port", 0, "HTTP server port (0 = disabled)")
	flag.Parse()

	acknowledged := guardrailsFlag || yoloFlag
	if !acknowledged {
		fmt.Fprintln(os.Stderr, "Error: You must pass --i-understand-that-this-will-be-running-without-the-usual-guardrails or --yolo")
		os.Exit(1)
	}

	// Positional arg: workflow path
	workflowPath := ""
	if flag.NArg() > 0 {
		workflowPath = flag.Arg(0)
	}

	// Resolve workflow path
	resolvedPath, err := workflow.WorkflowFilePath(workflowPath)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error resolving workflow path: %v\n", err)
		os.Exit(1)
	}

	// CLI overrides
	overrides := &config.CliOverrides{
		LogsRoot: logsRoot,
	}
	if port > 0 {
		overrides.ServerPortOverride = &port
	}

	// Load workflow
	workflowStore, err := workflow.NewWorkflowStore(resolvedPath)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error loading workflow: %v\n", err)
		os.Exit(1)
	}

	// Validate initial config
	settings, err := config.FromWorkflow(workflowStore.Current(), overrides)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Configuration error: %v\n", err)
		os.Exit(1)
	}

	// Setup logging
	effectiveLogsRoot := settings.EffectiveLogsRoot(overrides)
	logFile, err := logfile.Setup(effectiveLogsRoot)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Warning: Could not setup log file: %v\n", err)
	}
	if logFile != nil {
		defer logFile.Close()
	}

	slog.Info("Symphony starting",
		"workflow", resolvedPath,
		"tracker_kind", settings.Tracker.Kind,
		"max_agents", settings.Agent.MaxConcurrentAgents,
		"poll_interval_ms", settings.Polling.IntervalMs,
	)

	// Setup context with signal handling
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		sig := <-sigCh
		slog.Info("received signal, shutting down", "signal", sig)
		cancel()
	}()

	// Start orchestrator
	handle, err := orchestrator.Start(ctx, workflowStore, overrides)
	if err != nil {
		slog.Error("failed to start orchestrator", "error", err)
		os.Exit(1)
	}

	// Start HTTP server in background if configured
	if settings.Server.Port != nil {
		go func() {
			if err := httpserver.Serve(handle, workflowStore, overrides); err != nil {
				slog.Error("HTTP server error", "error", err)
			}
		}()
	}

	// Dashboard render loop (if dashboard enabled)
	if settings.Observability.DashboardEnabled {
		go func() {
			ticker := time.NewTicker(time.Duration(settings.Observability.RefreshMs) * time.Millisecond)
			defer ticker.Stop()
			for {
				select {
				case <-ctx.Done():
					return
				case <-ticker.C:
					snap, err := handle.Snapshot()
					if err != nil {
						continue
					}
					// Re-read settings for dashboard
					currentSettings, err := config.FromWorkflow(workflowStore.Current(), overrides)
					if err != nil {
						currentSettings = settings
					}
					output := dashboard.FormatSnapshot(&snap, currentSettings, 0, 115)
					// Clear screen and print
					fmt.Print("\033[H\033[2J")
					fmt.Println(output)
				}
			}
		}()
	}

	// Wait for context cancellation
	<-ctx.Done()
	slog.Info("Symphony stopped")
}
