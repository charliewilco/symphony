// Package ssh provides SSH execution helpers for remote workspace operations.
package ssh

import (
	"fmt"
	"os"
	"os/exec"
	"strings"
)

// Target holds a parsed SSH destination.
type Target struct {
	Destination string
	Port        string
}

// Run executes a command on a remote host via SSH and returns stdout and exit code.
func Run(host, command string) (string, int, error) {
	sshPath, err := findSSH()
	if err != nil {
		return "", -1, err
	}
	target := ParseTarget(host)

	args := sshArgs(target, RemoteShellCommand(command))
	cmd := exec.Command(sshPath, args...)
	out, err := cmd.CombinedOutput()
	exitCode := 0
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			exitCode = exitErr.ExitCode()
		} else {
			return string(out), -1, err
		}
	}
	return string(out), exitCode, nil
}

// StartChild starts a long-running SSH command and returns the exec.Cmd.
func StartChild(host, command string) (*exec.Cmd, error) {
	sshPath, err := findSSH()
	if err != nil {
		return nil, err
	}
	target := ParseTarget(host)
	args := sshArgs(target, RemoteShellCommand(command))

	cmd := exec.Command(sshPath, args...)
	cmd.Stdin = nil
	return cmd, nil
}

// RemoteShellCommand wraps a command for remote execution.
func RemoteShellCommand(command string) string {
	return fmt.Sprintf("bash -lc %s", ShellEscape(command))
}

// ShellEscape escapes a string for safe use in a shell command.
func ShellEscape(value string) string {
	return "'" + strings.ReplaceAll(value, "'", "'\"'\"'") + "'"
}

// ParseTarget parses a host:port string into a Target.
func ParseTarget(target string) Target {
	trimmed := strings.TrimSpace(target)
	if idx := strings.LastIndex(trimmed, ":"); idx >= 0 {
		dest := trimmed[:idx]
		port := trimmed[idx+1:]
		// If destination contains ':' and is not bracketed IPv6, treat whole thing as destination
		if strings.Contains(dest, ":") && !(strings.Contains(dest, "[") && strings.Contains(dest, "]")) {
			return Target{Destination: trimmed}
		}
		if dest != "" && isAllDigits(port) {
			return Target{Destination: dest, Port: port}
		}
	}
	return Target{Destination: trimmed}
}

func sshArgs(target Target, remoteCmd string) []string {
	var args []string
	if configFile := os.Getenv("SYMPHONY_SSH_CONFIG"); configFile != "" {
		args = append(args, "-F", configFile)
	}
	args = append(args, "-T")
	if target.Port != "" {
		args = append(args, "-p", target.Port)
	}
	args = append(args, target.Destination, remoteCmd)
	return args
}

func findSSH() (string, error) {
	path, err := exec.LookPath("ssh")
	if err != nil {
		return "", fmt.Errorf("ssh_not_found")
	}
	return path, nil
}

func isAllDigits(s string) bool {
	if s == "" {
		return false
	}
	for _, c := range s {
		if c < '0' || c > '9' {
			return false
		}
	}
	return true
}
