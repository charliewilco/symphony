package tool

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
)

type LinearGraphQLTool struct {
	Token  string
	Client *http.Client
}

func (t LinearGraphQLTool) Execute(ctx context.Context, payload any) (map[string]any, error) {
	body := map[string]any{}
	switch v := payload.(type) {
	case string:
		body["query"] = v
	case map[string]any:
		body = v
	default:
		return nil, fmt.Errorf("invalid payload type")
	}
	b, _ := json.Marshal(body)
	req, _ := http.NewRequestWithContext(ctx, http.MethodPost, "https://api.linear.app/graphql", bytes.NewBuffer(b))
	req.Header.Set("Authorization", t.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := t.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("network error: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode == 401 {
		return nil, fmt.Errorf("auth error: invalid linear token")
	}
	if resp.StatusCode >= 300 {
		return nil, fmt.Errorf("http error: %s", resp.Status)
	}
	var out map[string]any
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		return nil, err
	}
	return out, nil
}
