package agent

import (
	"context"
	"errors"
	"time"
)

type TurnResult struct{ LastOutput string }

type Client interface {
	RunTurn(ctx context.Context, prompt string) (TurnResult, error)
}

type MockCodex struct {
	Delay  time.Duration
	Output string
	Err    error
}

func (m MockCodex) RunTurn(ctx context.Context, _ string) (TurnResult, error) {
	if m.Delay > 0 {
		select {
		case <-ctx.Done():
			return TurnResult{}, ctx.Err()
		case <-time.After(m.Delay):
		}
	}
	if m.Err != nil {
		return TurnResult{}, m.Err
	}
	if m.Output == "" {
		return TurnResult{}, errors.New("empty output")
	}
	return TurnResult{LastOutput: m.Output}, nil
}
