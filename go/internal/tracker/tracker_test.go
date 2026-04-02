package tracker

import (
	"context"
	"testing"
)

func TestMemoryTracker(t *testing.T) {
	m := NewMemory([]Issue{{ID: "1", Identifier: "A-1", Title: "t", State: "Todo"}})
	if _, err := m.GetIssue(context.Background(), "1"); err != nil {
		t.Fatal(err)
	}
}
