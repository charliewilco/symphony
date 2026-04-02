package config

import "testing"

func TestValidateMissingFields(t *testing.T) {
	cfg := DefaultConfig()
	cfg.Tracker.ProjectSlug = ""
	cfg.Tracker.WorkspaceSlug = ""
	cfg.Codex.Command = ""
	d := Validate(cfg, "x")
	if len(d) < 3 {
		t.Fatalf("expected diagnostics, got %d", len(d))
	}
}
