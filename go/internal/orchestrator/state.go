package orchestrator

import (
	"encoding/json"
	"os"
	"path/filepath"
	"time"
)

type TokenTotals struct {
	Input  int64 `json:"input"`
	Output int64 `json:"output"`
}

type PersistedState struct {
	Version       int            `json:"version"`
	SavedAt       time.Time      `json:"saved_at"`
	RetryCounts   map[string]int `json:"retry_counts"`
	ClaimedIssues []string       `json:"claimed_issues"`
	TokenTotals   TokenTotals    `json:"token_totals"`
}

func StatePath(workspaceRoot string) string {
	return filepath.Join(workspaceRoot, ".symphony-state.json")
}

func SaveState(path string, st PersistedState) error {
	st.Version = 1
	st.SavedAt = time.Now().UTC()
	b, err := json.MarshalIndent(st, "", "  ")
	if err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

func LoadState(path string) (PersistedState, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return PersistedState{}, err
	}
	var st PersistedState
	if err := json.Unmarshal(b, &st); err != nil {
		return PersistedState{}, err
	}
	if st.RetryCounts == nil {
		st.RetryCounts = map[string]int{}
	}
	return st, nil
}
