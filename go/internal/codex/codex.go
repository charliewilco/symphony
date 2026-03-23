// Package codex manages the Codex app-server JSON-RPC protocol over stdio.
package codex

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"os/exec"
	"strings"
	"time"

	"symphony/internal/config"
	"symphony/internal/dynamictool"
	"symphony/internal/ssh"
	"symphony/internal/tracker"
)

const nonInteractiveToolInputAnswer = "This is a non-interactive session. Operator input is unavailable."

// CodexUpdate represents a status update from the codex process.
type CodexUpdate struct {
	Event             string          `json:"event"`
	Timestamp         time.Time       `json:"timestamp"`
	Payload           any             `json:"payload"`
	SessionID         string          `json:"session_id,omitempty"`
	CodexAppServerPID string          `json:"codex_app_server_pid,omitempty"`
	RateLimits        map[string]any  `json:"rate_limits,omitempty"`
}

// TurnResult holds the result of completing a turn.
type TurnResult struct {
	SessionID string
	ThreadID  string
	TurnID    string
}

// AppServerSession manages a running codex app-server process.
type AppServerSession struct {
	cmd               *exec.Cmd
	stdin             io.WriteCloser
	stdout            *bufio.Reader
	threadID          string
	workspace         string
	autoApprove       bool
	approvalPolicy    any
	turnSandboxPolicy any
	pid               string
}

// StartSession launches the codex app-server and completes the initialization handshake.
func StartSession(ctx context.Context, workspace string, workerHost string, settings *config.Settings) (*AppServerSession, error) {
	workspace = validateWorkspaceCwd(workspace, workerHost, settings)

	var cmd *exec.Cmd
	if workerHost != "" {
		remoteCmd := fmt.Sprintf("cd %s && exec %s", ssh.ShellEscape(workspace), settings.Codex.Command)
		var err error
		cmd, err = ssh.StartChild(workerHost, remoteCmd)
		if err != nil {
			return nil, err
		}
	} else {
		cmd = exec.CommandContext(ctx, "bash", "-lc", settings.Codex.Command)
		cmd.Dir = workspace
	}

	stdinPipe, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("cannot create stdin pipe: %w", err)
	}
	stdoutPipe, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("cannot create stdout pipe: %w", err)
	}
	stderrPipe, err := cmd.StderrPipe()
	if err != nil {
		return nil, fmt.Errorf("cannot create stderr pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("cannot start codex: %w", err)
	}

	// Log stderr in background
	go func() {
		scanner := bufio.NewScanner(stderrPipe)
		for scanner.Scan() {
			slog.Debug("codex stderr", "line", scanner.Text())
		}
	}()

	pid := ""
	if cmd.Process != nil {
		pid = fmt.Sprintf("%d", cmd.Process.Pid)
	}

	// Determine auto-approve
	autoApprove := false
	if s, ok := settings.Codex.ApprovalPolicy.(string); ok && s == "never" {
		autoApprove = true
	}

	session := &AppServerSession{
		cmd:               cmd,
		stdin:             stdinPipe,
		stdout:            bufio.NewReader(stdoutPipe),
		workspace:         workspace,
		autoApprove:       autoApprove,
		approvalPolicy:    settings.Codex.ApprovalPolicy,
		turnSandboxPolicy: settings.DefaultTurnSandboxPolicy(workspace),
		pid:               pid,
	}

	// Step 1: initialize
	if err := sendMessage(session.stdin, map[string]any{
		"method": "initialize",
		"id":     1,
		"params": map[string]any{
			"capabilities": map[string]any{"experimentalApi": true},
			"clientInfo": map[string]any{
				"name":    "symphony-orchestrator",
				"title":   "Symphony Orchestrator",
				"version": "1.0",
			},
		},
	}); err != nil {
		return nil, err
	}

	// Step 2: wait for response
	if _, err := awaitResponse(session.stdout, 1, settings); err != nil {
		return nil, fmt.Errorf("initialize response: %w", err)
	}

	// Step 3: initialized notification
	if err := sendMessage(session.stdin, map[string]any{
		"method": "initialized",
		"params": map[string]any{},
	}); err != nil {
		return nil, err
	}

	// Step 4: thread/start
	if err := sendMessage(session.stdin, map[string]any{
		"method": "thread/start",
		"id":     2,
		"params": map[string]any{
			"approvalPolicy": session.approvalPolicy,
			"sandbox":        settings.Codex.ThreadSandbox,
			"cwd":            workspace,
			"dynamicTools":   dynamictool.ToolSpecs(),
		},
	}); err != nil {
		return nil, err
	}

	// Step 5: get thread_id
	threadResp, err := awaitResponse(session.stdout, 2, settings)
	if err != nil {
		return nil, fmt.Errorf("thread/start response: %w", err)
	}
	threadID := jsonPointerStr(threadResp, "thread", "id")
	if threadID == "" {
		return nil, fmt.Errorf("invalid_thread_payload")
	}
	session.threadID = threadID

	return session, nil
}

// RunTurn starts a turn, streams events, handles tool calls and approvals.
func RunTurn(ctx context.Context, session *AppServerSession, prompt string, issue *tracker.Issue, settings *config.Settings, updatesCh chan<- CodexUpdate) (*TurnResult, error) {
	// Step 6: turn/start
	if err := sendMessage(session.stdin, map[string]any{
		"method": "turn/start",
		"id":     3,
		"params": map[string]any{
			"threadId":      session.threadID,
			"input":         []map[string]any{{"type": "text", "text": prompt}},
			"cwd":           session.workspace,
			"title":         fmt.Sprintf("%s: %s", issue.Identifier, issue.Title),
			"approvalPolicy": session.approvalPolicy,
			"sandboxPolicy": session.turnSandboxPolicy,
		},
	}); err != nil {
		return nil, err
	}

	// Step 7: get turn_id
	turnResp, err := awaitResponse(session.stdout, 3, settings)
	if err != nil {
		return nil, fmt.Errorf("turn/start response: %w", err)
	}
	turnID := jsonPointerStr(turnResp, "turn", "id")
	if turnID == "" {
		return nil, fmt.Errorf("invalid_turn_payload")
	}
	sessionID := fmt.Sprintf("%s-%s", session.threadID, turnID)

	// Send session_started update
	sendUpdate(updatesCh, CodexUpdate{
		Event:     "session_started",
		Timestamp: time.Now().UTC(),
		Payload: map[string]any{
			"thread_id":  session.threadID,
			"turn_id":    turnID,
			"session_id": sessionID,
		},
		SessionID:         sessionID,
		CodexAppServerPID: session.pid,
	})

	// Stream loop
	turnTimeout := time.Duration(settings.Codex.TurnTimeoutMs) * time.Millisecond
	for {
		line, err := readLineWithTimeout(session.stdout, turnTimeout)
		if err != nil {
			return nil, fmt.Errorf("port_exit: %w", err)
		}

		trimmed := strings.TrimSpace(line)
		if trimmed == "" {
			continue
		}

		var payload map[string]any
		if err := json.Unmarshal([]byte(trimmed), &payload); err != nil {
			slog.Debug("non-JSON line from codex", "line", trimmed)
			continue
		}

		method, _ := payload["method"].(string)
		switch method {
		case "turn/completed":
			sendUpdate(updatesCh, CodexUpdate{
				Event:             "turn_completed",
				Timestamp:         time.Now().UTC(),
				Payload:           payload,
				SessionID:         sessionID,
				CodexAppServerPID: session.pid,
			})
			return &TurnResult{
				SessionID: sessionID,
				ThreadID:  session.threadID,
				TurnID:    turnID,
			}, nil

		case "turn/failed":
			params, _ := payload["params"]
			return nil, fmt.Errorf("turn_failed: %v", params)

		case "turn/cancelled":
			params, _ := payload["params"]
			return nil, fmt.Errorf("turn_cancelled: %v", params)

		case "item/tool/call":
			id := payload["id"]
			if id == nil {
				continue
			}
			params, _ := payload["params"].(map[string]any)
			toolName := toolCallName(params)
			arguments := toolCallArguments(params)

			result := dynamictool.Execute(ctx, toolName, arguments, settings)
			if err := sendMessage(session.stdin, map[string]any{
				"id":     id,
				"result": result,
			}); err != nil {
				return nil, err
			}

			event := "unsupported_tool_call"
			if toolName == dynamictool.LinearGraphQLTool {
				if success, ok := result["success"].(bool); ok && success {
					event = "tool_call_completed"
				} else {
					event = "tool_call_failed"
				}
			}
			sendUpdate(updatesCh, CodexUpdate{
				Event:             event,
				Timestamp:         time.Now().UTC(),
				Payload:           map[string]any{"payload": payload, "raw": trimmed},
				SessionID:         sessionID,
				CodexAppServerPID: session.pid,
			})

		case "item/tool/requestUserInput":
			id := payload["id"]
			if id == nil {
				return nil, fmt.Errorf("turn_input_required: %v", payload)
			}
			params, _ := payload["params"].(map[string]any)

			if session.autoApprove {
				if answers, decision := toolRequestUserInputApprovalAnswers(params); answers != nil {
					sendMessage(session.stdin, map[string]any{
						"id":     id,
						"result": map[string]any{"answers": answers},
					})
					sendUpdate(updatesCh, CodexUpdate{
						Event:             "approval_auto_approved",
						Timestamp:         time.Now().UTC(),
						Payload:           map[string]any{"payload": payload, "raw": trimmed, "decision": decision},
						SessionID:         sessionID,
						CodexAppServerPID: session.pid,
					})
					continue
				}
			}

			answers := toolRequestUserInputUnavailableAnswers(params)
			if answers == nil {
				return nil, fmt.Errorf("turn_input_required: %v", payload)
			}
			sendMessage(session.stdin, map[string]any{
				"id":     id,
				"result": map[string]any{"answers": answers},
			})
			sendUpdate(updatesCh, CodexUpdate{
				Event:             "tool_input_auto_answered",
				Timestamp:         time.Now().UTC(),
				Payload:           map[string]any{"payload": payload, "raw": trimmed, "answer": nonInteractiveToolInputAnswer},
				SessionID:         sessionID,
				CodexAppServerPID: session.pid,
			})

		case "item/commandExecution/requestApproval",
			"execCommandApproval",
			"applyPatchApproval",
			"item/fileChange/requestApproval":
			if session.autoApprove {
				if id := payload["id"]; id != nil {
					decision := approvalDecision(method)
					sendMessage(session.stdin, map[string]any{
						"id":     id,
						"result": map[string]any{"decision": decision},
					})
					sendUpdate(updatesCh, CodexUpdate{
						Event:             "approval_auto_approved",
						Timestamp:         time.Now().UTC(),
						Payload:           map[string]any{"payload": payload, "raw": trimmed, "decision": decision},
						SessionID:         sessionID,
						CodexAppServerPID: session.pid,
					})
				}
			} else {
				return nil, fmt.Errorf("approval_required: %v", payload)
			}

		default:
			// Generic notification
			sendUpdate(updatesCh, CodexUpdate{
				Event:             "notification",
				Timestamp:         time.Now().UTC(),
				Payload:           payload,
				SessionID:         sessionID,
				CodexAppServerPID: session.pid,
			})
		}
	}
}

// StopSession kills the codex process.
func StopSession(session *AppServerSession) error {
	if session.cmd != nil && session.cmd.Process != nil {
		session.cmd.Process.Kill()
		session.cmd.Wait()
	}
	return nil
}

// --- helpers ---

func sendMessage(w io.Writer, msg any) error {
	data, err := json.Marshal(msg)
	if err != nil {
		return fmt.Errorf("marshal error: %w", err)
	}
	data = append(data, '\n')
	_, err = w.Write(data)
	return err
}

func awaitResponse(r *bufio.Reader, expectedID int, settings *config.Settings) (map[string]any, error) {
	timeout := time.Duration(settings.Codex.ReadTimeoutMs) * time.Millisecond
	deadline := time.After(timeout)

	type lineResult struct {
		line string
		err  error
	}
	ch := make(chan lineResult, 1)

	go func() {
		line, err := r.ReadString('\n')
		ch <- lineResult{line, err}
	}()

	select {
	case result := <-ch:
		if result.err != nil {
			return nil, result.err
		}
		trimmed := strings.TrimSpace(result.line)
		var payload map[string]any
		if err := json.Unmarshal([]byte(trimmed), &payload); err != nil {
			return nil, fmt.Errorf("invalid JSON response: %w", err)
		}
		// Check for result field
		if result, ok := payload["result"]; ok {
			if resultMap, ok := result.(map[string]any); ok {
				return resultMap, nil
			}
		}
		return payload, nil
	case <-deadline:
		return nil, fmt.Errorf("read_timeout")
	}
}

func readLineWithTimeout(r *bufio.Reader, timeout time.Duration) (string, error) {
	type lineResult struct {
		line string
		err  error
	}
	ch := make(chan lineResult, 1)

	go func() {
		line, err := r.ReadString('\n')
		ch <- lineResult{line, err}
	}()

	select {
	case result := <-ch:
		return result.line, result.err
	case <-time.After(timeout):
		return "", fmt.Errorf("turn_timeout")
	}
}

func validateWorkspaceCwd(workspace, workerHost string, settings *config.Settings) string {
	if workerHost != "" {
		return workspace
	}
	// For local, ensure path is valid
	if workspace == "" {
		return settings.Workspace.Root
	}
	return workspace
}

func toolCallName(params map[string]any) string {
	if params == nil {
		return ""
	}
	if name, ok := params["name"].(string); ok {
		return name
	}
	if name, ok := params["toolName"].(string); ok {
		return name
	}
	return ""
}

func toolCallArguments(params map[string]any) any {
	if params == nil {
		return nil
	}
	if args, ok := params["arguments"]; ok {
		// If arguments is a string, try to parse as JSON
		if s, ok := args.(string); ok {
			var parsed any
			if err := json.Unmarshal([]byte(s), &parsed); err == nil {
				return parsed
			}
			return s
		}
		return args
	}
	if args, ok := params["input"]; ok {
		return args
	}
	return nil
}

func approvalDecision(method string) string {
	switch method {
	case "item/fileChange/requestApproval", "applyPatchApproval":
		return "approve"
	default:
		return "allow"
	}
}

func needsInput(method string, payload map[string]any) bool {
	return method == "item/requestUserInput" || method == "requestUserInput"
}

func toolRequestUserInputApprovalAnswers(params map[string]any) ([]map[string]any, string) {
	inputItems, ok := params["inputItems"].([]any)
	if !ok || len(inputItems) == 0 {
		return nil, ""
	}
	var answers []map[string]any
	decision := "approve"
	for _, item := range inputItems {
		itemMap, ok := item.(map[string]any)
		if !ok {
			return nil, ""
		}
		itemType, _ := itemMap["type"].(string)
		if itemType != "confirmation" {
			return nil, ""
		}
		id, _ := itemMap["id"].(string)
		if id == "" {
			return nil, ""
		}
		answers = append(answers, map[string]any{
			"id":     id,
			"result": true,
		})
	}
	return answers, decision
}

func toolRequestUserInputUnavailableAnswers(params map[string]any) []map[string]any {
	inputItems, ok := params["inputItems"].([]any)
	if !ok || len(inputItems) == 0 {
		return nil
	}
	var answers []map[string]any
	for _, item := range inputItems {
		itemMap, ok := item.(map[string]any)
		if !ok {
			return nil
		}
		id, _ := itemMap["id"].(string)
		if id == "" {
			return nil
		}
		itemType, _ := itemMap["type"].(string)
		switch itemType {
		case "text":
			answers = append(answers, map[string]any{
				"id":     id,
				"result": nonInteractiveToolInputAnswer,
			})
		case "confirmation":
			answers = append(answers, map[string]any{
				"id":     id,
				"result": false,
			})
		default:
			answers = append(answers, map[string]any{
				"id":     id,
				"result": nonInteractiveToolInputAnswer,
			})
		}
	}
	return answers
}

func sendUpdate(ch chan<- CodexUpdate, update CodexUpdate) {
	if update.RateLimits == nil {
		update.RateLimits = ExtractRateLimits(update.Payload)
	}
	select {
	case ch <- update:
	default:
		// Drop if channel is full
	}
}

// ExtractRateLimits searches a payload for a rate-limits map, matching the
// Rust extract_rate_limits heuristic: look for an object with limit_id/limit_name
// and at least one of primary/secondary/credits.
func ExtractRateLimits(payload any) map[string]any {
	return rateLimitsFromPayload(payload)
}

func rateLimitsFromPayload(v any) map[string]any {
	m, ok := v.(map[string]any)
	if !ok {
		if arr, ok := v.([]any); ok {
			for _, item := range arr {
				if rl := rateLimitsFromPayload(item); rl != nil {
					return rl
				}
			}
		}
		return nil
	}
	// Direct rate_limits key
	if direct, ok := m["rate_limits"]; ok {
		if rl := rateLimitsFromPayload(direct); rl != nil {
			return rl
		}
	}
	// This object itself looks like a rate limits map
	if isRateLimitsMap(m) {
		return m
	}
	// Recurse into values
	for _, val := range m {
		if rl := rateLimitsFromPayload(val); rl != nil {
			return rl
		}
	}
	return nil
}

func isRateLimitsMap(m map[string]any) bool {
	_, hasLimitID := m["limit_id"]
	_, hasLimitName := m["limit_name"]
	if !hasLimitID && !hasLimitName {
		return false
	}
	for _, bucket := range []string{"primary", "secondary", "credits"} {
		if _, ok := m[bucket]; ok {
			return true
		}
	}
	return false
}

func jsonPointerStr(m map[string]any, keys ...string) string {
	current := any(m)
	for _, key := range keys {
		cm, ok := current.(map[string]any)
		if !ok {
			return ""
		}
		current = cm[key]
	}
	s, _ := current.(string)
	return s
}
