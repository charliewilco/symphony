package workflow

import (
	"fmt"
	"os"
	"strings"

	"github.com/osteele/liquid"
)

type IssueData struct {
	Identifier  string
	Title       string
	State       string
	Description string
	Labels      []string
	URL         string
	BranchName  string
}

type Template struct {
	Source string
	engine *liquid.Engine
}

func Load(path string) (*Template, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	e := liquid.NewEngine()
	return &Template{Source: string(b), engine: e}, nil
}

func (t *Template) Render(issue IssueData, attempt *int) (string, error) {
	ctx := map[string]any{"issue": map[string]any{"identifier": issue.Identifier, "title": issue.Title, "state": issue.State, "description": issue.Description, "labels": issue.Labels, "url": issue.URL, "branch_name": issue.BranchName}}
	if attempt != nil {
		ctx["attempt"] = *attempt
	}
	out, err := t.engine.ParseAndRenderString(t.Source, ctx)
	if err != nil {
		return "", fmt.Errorf("render workflow: %w", err)
	}
	if strings.TrimSpace(out) == "" {
		return "", fmt.Errorf("rendered prompt is empty")
	}
	return out, nil
}
