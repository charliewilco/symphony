package tracker

import (
	"context"
	"fmt"
	"sync"
)

type Memory struct {
	mu     sync.RWMutex
	issues map[string]Issue
}

func NewMemory(issues []Issue) *Memory {
	m := &Memory{issues: map[string]Issue{}}
	for _, i := range issues {
		m.issues[i.ID] = i
	}
	return m
}

func (m *Memory) ListIssues(context.Context) ([]Issue, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	out := make([]Issue, 0, len(m.issues))
	for _, i := range m.issues {
		out = append(out, i)
	}
	return out, nil
}

func (m *Memory) GetIssue(_ context.Context, id string) (Issue, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	i, ok := m.issues[id]
	if !ok {
		return Issue{}, fmt.Errorf("issue not found: %s", id)
	}
	return i, nil
}

func (m *Memory) Upsert(i Issue) { m.mu.Lock(); defer m.mu.Unlock(); m.issues[i.ID] = i }
