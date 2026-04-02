package tracker

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
)

type Linear struct {
	Token         string
	WorkspaceSlug string
	ProjectSlug   string
	Client        *http.Client
}

func (l *Linear) ListIssues(ctx context.Context) ([]Issue, error) {
	q := `{"query":"query Issues($workspace: String!, $project: String!){issues(filter:{team:{key:{eq:$workspace}},project:{slugId:{eq:$project}}}){nodes{id identifier title}}}","variables":{"workspace":"` + l.WorkspaceSlug + `","project":"` + l.ProjectSlug + `"}}`
	req, _ := http.NewRequestWithContext(ctx, http.MethodPost, "https://api.linear.app/graphql", bytes.NewBufferString(q))
	req.Header.Set("Authorization", l.Token)
	req.Header.Set("Content-Type", "application/json")
	resp, err := l.Client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 300 {
		return nil, fmt.Errorf("linear status %d", resp.StatusCode)
	}
	var raw map[string]any
	if err := json.NewDecoder(resp.Body).Decode(&raw); err != nil {
		return nil, err
	}
	return []Issue{}, nil
}

func (l *Linear) GetIssue(ctx context.Context, id string) (Issue, error) {
	issues, err := l.ListIssues(ctx)
	if err != nil {
		return Issue{}, err
	}
	for _, i := range issues {
		if i.ID == id {
			return i, nil
		}
	}
	return Issue{}, fmt.Errorf("not found")
}
