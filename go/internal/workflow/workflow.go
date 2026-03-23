// Package workflow handles parsing WORKFLOW.md files with YAML front matter and prompt body.
package workflow

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"gopkg.in/yaml.v3"
)

// LoadedWorkflow holds the parsed workflow definition.
type LoadedWorkflow struct {
	Config         map[string]any // YAML front matter root object
	PromptTemplate string         // Markdown body after front matter, trimmed
}

// WorkflowFilePath resolves the workflow file path from an explicit path or cwd default.
func WorkflowFilePath(explicit string) (string, error) {
	if explicit != "" {
		abs, err := filepath.Abs(explicit)
		if err != nil {
			return explicit, nil
		}
		return abs, nil
	}
	cwd, err := os.Getwd()
	if err != nil {
		return "", fmt.Errorf("cannot determine cwd: %w", err)
	}
	return filepath.Join(cwd, "WORKFLOW.md"), nil
}

// Load reads and parses a workflow file from disk.
func Load(path string) (*LoadedWorkflow, error) {
	content, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("missing_workflow_file: %s: %w", path, err)
	}
	return Parse(string(content))
}

// Parse parses workflow content (front matter + prompt body).
func Parse(content string) (*LoadedWorkflow, error) {
	frontMatterLines, promptLines := splitFrontMatter(content)
	config, err := frontMatterToMap(frontMatterLines)
	if err != nil {
		return nil, err
	}
	prompt := strings.TrimSpace(strings.Join(promptLines, "\n"))
	return &LoadedWorkflow{
		Config:         config,
		PromptTemplate: prompt,
	}, nil
}

func splitFrontMatter(content string) ([]string, []string) {
	lines := strings.Split(content, "\n")
	if len(lines) == 0 || lines[0] != "---" {
		return nil, lines
	}

	var front []string
	var restStart int
	found := false
	for i := 1; i < len(lines); i++ {
		if lines[i] == "---" {
			restStart = i + 1
			found = true
			break
		}
		front = append(front, lines[i])
	}

	if !found {
		// Unterminated front matter: treat all as front matter, empty prompt
		return front, nil
	}

	var promptLines []string
	if restStart < len(lines) {
		promptLines = lines[restStart:]
	}
	return front, promptLines
}

func frontMatterToMap(lines []string) (map[string]any, error) {
	yamlStr := strings.Join(lines, "\n")
	if strings.TrimSpace(yamlStr) == "" {
		return make(map[string]any), nil
	}

	var raw any
	if err := yaml.Unmarshal([]byte(yamlStr), &raw); err != nil {
		return nil, fmt.Errorf("workflow_parse_error: %w", err)
	}

	m, ok := raw.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("workflow_front_matter_not_a_map")
	}
	return m, nil
}

// WorkflowStore manages workflow state and hot-reload.
type WorkflowStore struct {
	path     string
	mu       sync.RWMutex
	current  *LoadedWorkflow
	lastMod  time.Time
	lastSize int64
}

// NewWorkflowStore creates and loads a workflow store.
func NewWorkflowStore(path string) (*WorkflowStore, error) {
	ws := &WorkflowStore{path: path}
	if err := ws.reload(); err != nil {
		return nil, err
	}
	return ws, nil
}

// Current returns the current loaded workflow.
func (ws *WorkflowStore) Current() *LoadedWorkflow {
	ws.mu.RLock()
	defer ws.mu.RUnlock()
	return ws.current
}

// MaybeReload checks file mtime and reloads if changed.
func (ws *WorkflowStore) MaybeReload() error {
	info, err := os.Stat(ws.path)
	if err != nil {
		return fmt.Errorf("missing_workflow_file: %s: %w", ws.path, err)
	}

	ws.mu.RLock()
	changed := info.ModTime() != ws.lastMod || info.Size() != ws.lastSize
	ws.mu.RUnlock()

	if !changed {
		return nil
	}
	return ws.reload()
}

func (ws *WorkflowStore) reload() error {
	w, err := Load(ws.path)
	if err != nil {
		return err
	}
	info, _ := os.Stat(ws.path)

	ws.mu.Lock()
	defer ws.mu.Unlock()
	ws.current = w
	if info != nil {
		ws.lastMod = info.ModTime()
		ws.lastSize = info.Size()
	}
	return nil
}
