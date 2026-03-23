// Package workspace manages per-issue workspace directories and lifecycle hooks.
package workspace

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
	"unicode"

	"symphony/internal/config"
	"symphony/internal/ssh"
)

// WorkspaceContext holds the result of workspace creation.
type WorkspaceContext struct {
	Path       string
	CreatedNow bool
	WorkerHost string // empty for local
}

// CreateForIssue creates or reuses a workspace directory for an issue.
func CreateForIssue(ctx context.Context, issueIdentifier string, settings *config.Settings, workerHost string) (*WorkspaceContext, error) {
	workspaceKey := SanitizeIdentifier(issueIdentifier)

	if workerHost != "" {
		path := filepath.Join(settings.Workspace.Root, workspaceKey)
		if err := ensureRemoteWorkspace(path, workerHost, settings); err != nil {
			return nil, err
		}
		wsCtx := &WorkspaceContext{
			Path:       path,
			CreatedNow: true,
			WorkerHost: workerHost,
		}
		if err := runAfterCreateIfNeeded(ctx, wsCtx, issueIdentifier, settings); err != nil {
			return nil, err
		}
		return wsCtx, nil
	}

	// Local workspace
	if err := os.MkdirAll(settings.Workspace.Root, 0o755); err != nil {
		return nil, fmt.Errorf("cannot create workspace root: %w", err)
	}

	canonicalRoot, err := filepath.Abs(settings.Workspace.Root)
	if err != nil {
		canonicalRoot = settings.Workspace.Root
	}

	target := filepath.Join(canonicalRoot, workspaceKey)

	createdNow := false
	fi, err := os.Stat(target)
	if err != nil {
		// Does not exist
		if err := os.MkdirAll(target, 0o755); err != nil {
			return nil, err
		}
		createdNow = true
	} else if !fi.IsDir() {
		// Exists but is not a dir
		os.Remove(target)
		if err := os.MkdirAll(target, 0o755); err != nil {
			return nil, err
		}
		createdNow = true
	}

	if err := validateLocalWorkspacePath(canonicalRoot, target); err != nil {
		return nil, err
	}

	wsCtx := &WorkspaceContext{
		Path:       target,
		CreatedNow: createdNow,
	}
	if err := runAfterCreateIfNeeded(ctx, wsCtx, issueIdentifier, settings); err != nil {
		return nil, err
	}
	return wsCtx, nil
}

// RunBeforeRunHook runs the before_run hook if configured.
func RunBeforeRunHook(ctx context.Context, ws *WorkspaceContext, issueIdentifier string, settings *config.Settings) error {
	if settings.Hooks.BeforeRun == "" {
		return nil
	}
	return runHook(ctx, settings.Hooks.BeforeRun, ws, issueIdentifier, "before_run", false, settings)
}

// RunAfterRunHook runs the after_run hook if configured. Failures are logged but ignored.
func RunAfterRunHook(ctx context.Context, ws *WorkspaceContext, issueIdentifier string, settings *config.Settings) {
	if settings.Hooks.AfterRun == "" {
		return
	}
	if err := runHook(ctx, settings.Hooks.AfterRun, ws, issueIdentifier, "after_run", true, settings); err != nil {
		slog.Warn("after_run hook failed", "issue", issueIdentifier, "error", err)
	}
}

// RemoveIssueWorkspace removes the workspace directory for an issue.
func RemoveIssueWorkspace(ctx context.Context, issueIdentifier string, settings *config.Settings, workerHost string) error {
	workspaceKey := SanitizeIdentifier(issueIdentifier)
	path := filepath.Join(settings.Workspace.Root, workspaceKey)
	ws := &WorkspaceContext{
		Path:       path,
		WorkerHost: workerHost,
	}
	return RemoveWorkspace(ctx, ws, issueIdentifier, settings)
}

// RemoveWorkspace removes a workspace directory, running before_remove hook first.
func RemoveWorkspace(ctx context.Context, ws *WorkspaceContext, issueIdentifier string, settings *config.Settings) error {
	if settings.Hooks.BeforeRemove != "" {
		if err := runHook(ctx, settings.Hooks.BeforeRemove, ws, issueIdentifier, "before_remove", true, settings); err != nil {
			slog.Warn("before_remove hook failed", "issue", issueIdentifier, "error", err)
		}
	}

	if ws.WorkerHost != "" {
		cmd := fmt.Sprintf("rm -rf %s", ssh.ShellEscape(ws.Path))
		_, exitCode, err := ssh.Run(ws.WorkerHost, cmd)
		if err != nil {
			return err
		}
		if exitCode != 0 {
			return fmt.Errorf("workspace_remove_failed")
		}
		return nil
	}

	root, err := filepath.Abs(settings.Workspace.Root)
	if err != nil {
		root = settings.Workspace.Root
	}
	target, err := filepath.Abs(ws.Path)
	if err != nil {
		target = ws.Path
	}
	if target == root {
		return fmt.Errorf("cannot_remove_workspace_root")
	}
	if _, err := os.Stat(target); err == nil {
		return os.RemoveAll(target)
	}
	return nil
}

// SanitizeIdentifier replaces characters not in [A-Za-z0-9._-] with _.
func SanitizeIdentifier(identifier string) string {
	var b strings.Builder
	for _, c := range identifier {
		if (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '.' || c == '_' || c == '-' {
			b.WriteRune(c)
		} else if unicode.IsLetter(c) || unicode.IsDigit(c) {
			b.WriteRune('_')
		} else {
			b.WriteRune('_')
		}
	}
	return b.String()
}

func runAfterCreateIfNeeded(ctx context.Context, ws *WorkspaceContext, issueIdentifier string, settings *config.Settings) error {
	if ws.CreatedNow && settings.Hooks.AfterCreate != "" {
		return runHook(ctx, settings.Hooks.AfterCreate, ws, issueIdentifier, "after_create", false, settings)
	}
	return nil
}

func runHook(ctx context.Context, command string, ws *WorkspaceContext, issueIdentifier, hookName string, bestEffort bool, settings *config.Settings) error {
	timeoutDuration := time.Duration(settings.Hooks.TimeoutMs) * time.Millisecond
	hookCtx, cancel := context.WithTimeout(ctx, timeoutDuration)
	defer cancel()

	var output string
	var exitCode int
	var err error

	if ws.WorkerHost != "" {
		remoteCmd := fmt.Sprintf("cd %s && %s", ssh.ShellEscape(ws.Path), command)
		output, exitCode, err = ssh.Run(ws.WorkerHost, remoteCmd)
	} else {
		cmd := exec.CommandContext(hookCtx, "sh", "-lc", command)
		cmd.Dir = ws.Path
		out, cmdErr := cmd.CombinedOutput()
		output = string(out)
		if cmdErr != nil {
			if exitErr, ok := cmdErr.(*exec.ExitError); ok {
				exitCode = exitErr.ExitCode()
			} else {
				err = cmdErr
			}
		}
	}

	if hookCtx.Err() == context.DeadlineExceeded {
		if bestEffort {
			slog.Warn("Workspace hook timed out", "hook", hookName, "issue", issueIdentifier)
			return nil
		}
		return fmt.Errorf("workspace_hook_timeout: %s", hookName)
	}

	if err != nil {
		if bestEffort {
			slog.Warn("Workspace hook failed", "hook", hookName, "issue", issueIdentifier, "error", err)
			return nil
		}
		return err
	}

	if exitCode != 0 {
		if bestEffort {
			slog.Warn("Workspace hook failed", "hook", hookName, "issue", issueIdentifier, "status", exitCode, "output", output)
			return nil
		}
		return fmt.Errorf("workspace_hook_failed: %s: status=%d output=%s", hookName, exitCode, output)
	}

	return nil
}

func validateLocalWorkspacePath(root, workspace string) error {
	absRoot, err := filepath.Abs(root)
	if err != nil {
		return fmt.Errorf("path_canonicalize_failed: %w", err)
	}
	absWorkspace, err := filepath.Abs(workspace)
	if err != nil {
		return fmt.Errorf("path_canonicalize_failed: %w", err)
	}
	if absWorkspace == absRoot {
		return fmt.Errorf("invalid_workspace_cwd")
	}
	if !strings.HasPrefix(absWorkspace, absRoot+string(filepath.Separator)) && absWorkspace != absRoot {
		return fmt.Errorf("workspace_outside_root")
	}
	return nil
}

func ensureRemoteWorkspace(path, workerHost string, settings *config.Settings) error {
	command := fmt.Sprintf("set -eu\nworkspace=%s\nif [ -d \"$workspace\" ]; then exit 0; fi\nif [ -e \"$workspace\" ]; then rm -rf \"$workspace\"; fi\nmkdir -p \"$workspace\"",
		ssh.ShellEscape(path))

	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(settings.Hooks.TimeoutMs)*time.Millisecond)
	defer cancel()

	_ = ctx // We rely on the SSH command timeout in practice
	output, exitCode, err := ssh.Run(workerHost, command)
	if err != nil {
		return fmt.Errorf("workspace_prepare_failed: %w", err)
	}
	if exitCode != 0 {
		return fmt.Errorf("workspace_prepare_failed: %s", output)
	}
	return nil
}
