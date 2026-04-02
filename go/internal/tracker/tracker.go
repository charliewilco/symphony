package tracker

import "context"

type Issue struct {
	ID            string   `json:"id"`
	Identifier    string   `json:"identifier"`
	Title         string   `json:"title"`
	State         string   `json:"state"`
	Description   string   `json:"description"`
	Labels        []string `json:"labels"`
	URL           string   `json:"url"`
	BranchName    string   `json:"branch_name"`
	RepositoryURL string   `json:"repository_url"`
	Priority      *int     `json:"priority"`
	CreatedAtUnix int64    `json:"created_at_unix"`
	Blockers      []Issue  `json:"blockers"`
}

type Tracker interface {
	ListIssues(ctx context.Context) ([]Issue, error)
	GetIssue(ctx context.Context, id string) (Issue, error)
}
