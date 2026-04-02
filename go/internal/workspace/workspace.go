package workspace

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"time"
)

var invalidKey = regexp.MustCompile(`[^A-Za-z0-9._-]`)

func Key(identifier string) string { return invalidKey.ReplaceAllString(identifier, "_") }

func RunHook(ctx context.Context, root, script string, timeout time.Duration) error {
	if script == "" {
		return nil
	}
	hctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	cmd := exec.CommandContext(hctx, "bash", "-lc", script)
	cmd.Dir = root
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("hook failed: %w: %s", err, string(out))
	}
	return nil
}

func Ensure(path string) error { return os.MkdirAll(path, 0o755) }
func WorkspacePath(root, issueIdentifier string) string {
	return filepath.Join(root, Key(issueIdentifier))
}
