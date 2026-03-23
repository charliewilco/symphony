// Package logfile sets up structured JSON logging to file and stderr.
package logfile

import (
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"time"
)

// Setup configures slog with a JSON handler writing to both a log file and stderr.
func Setup(logsRoot string) (*os.File, error) {
	if err := os.MkdirAll(logsRoot, 0o755); err != nil {
		return nil, err
	}

	timestamp := time.Now().UTC().Format("20060102T150405")
	logPath := filepath.Join(logsRoot, "symphony-"+timestamp+".log")

	f, err := os.OpenFile(logPath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, err
	}

	// Write to both file and stderr
	w := io.MultiWriter(f, os.Stderr)
	handler := slog.NewJSONHandler(w, &slog.HandlerOptions{
		Level: slog.LevelDebug,
	})
	slog.SetDefault(slog.New(handler))

	slog.Info("logging initialized", "path", logPath)
	return f, nil
}
