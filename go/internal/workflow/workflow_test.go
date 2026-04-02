package workflow

import (
	"os"
	"path/filepath"
	"testing"
)

func TestRender(t *testing.T) {
	d := t.TempDir()
	p := filepath.Join(d, "WORKFLOW.md")
	_ = os.WriteFile(p, []byte("Issue {{ issue.identifier }}"), 0o644)
	tpl, err := Load(p)
	if err != nil {
		t.Fatal(err)
	}
	out, err := tpl.Render(IssueData{Identifier: "ABC-1"}, nil)
	if err != nil || out == "" {
		t.Fatalf("render failed: %v %q", err, out)
	}
}
