package tracker

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"symphony/internal/config"
)

// LinearTracker implements the Tracker interface for the Linear API.
type LinearTracker struct {
	client *http.Client
}

// NewLinearTracker creates a new Linear tracker client.
func NewLinearTracker() *LinearTracker {
	return &LinearTracker{
		client: &http.Client{Timeout: 30 * time.Second},
	}
}

func (lt *LinearTracker) FetchCandidateIssues(ctx context.Context, settings *config.Settings) ([]Issue, error) {
	if settings.Tracker.ProjectSlug == "" {
		return nil, fmt.Errorf("missing_linear_project_slug")
	}

	query := `
		query CandidateIssues($project: String!, $states: [String!]) {
			issues(
				filter: {
					project: { slugId: { eq: $project } }
					state: { name: { in: $states } }
				}
			) {
				nodes {
					id identifier title description priority url branchName createdAt updatedAt
					assignee { id email }
					state { name }
					labels { nodes { name } }
					inverseRelations {
						nodes {
							type
							issue { id identifier state { name } }
						}
					}
				}
			}
		}
	`

	variables := map[string]any{
		"project": settings.Tracker.ProjectSlug,
		"states":  settings.Tracker.ActiveStates,
	}

	payload, err := lt.GraphQL(ctx, query, variables, settings)
	if err != nil {
		return nil, err
	}

	issues, err := parseLinearIssueNodes(payload)
	if err != nil {
		return nil, err
	}
	return applyRouting(issues, settings), nil
}

func (lt *LinearTracker) FetchIssuesByStates(ctx context.Context, states []string, settings *config.Settings) ([]Issue, error) {
	if len(states) == 0 {
		return nil, nil
	}
	if settings.Tracker.ProjectSlug == "" {
		return nil, fmt.Errorf("missing_linear_project_slug")
	}

	query := `
		query IssuesByStates($project: String!, $states: [String!]) {
			issues(
				filter: {
					project: { slugId: { eq: $project } }
					state: { name: { in: $states } }
				}
			) {
				nodes {
					id identifier title description priority url branchName createdAt updatedAt
					assignee { id email }
					state { name }
					labels { nodes { name } }
					inverseRelations {
						nodes {
							type
							issue { id identifier state { name } }
						}
					}
				}
			}
		}
	`

	variables := map[string]any{
		"project": settings.Tracker.ProjectSlug,
		"states":  states,
	}

	payload, err := lt.GraphQL(ctx, query, variables, settings)
	if err != nil {
		return nil, err
	}
	return parseLinearIssueNodes(payload)
}

func (lt *LinearTracker) FetchIssueStatesByIDs(ctx context.Context, ids []string, settings *config.Settings) ([]Issue, error) {
	if len(ids) == 0 {
		return nil, nil
	}

	query := `
		query IssueStates($ids: [ID!]) {
			issues(filter: { id: { in: $ids } }) {
				nodes {
					id identifier title description priority url branchName createdAt updatedAt
					assignee { id email }
					state { name }
					labels { nodes { name } }
					inverseRelations {
						nodes {
							type
							issue { id identifier state { name } }
						}
					}
				}
			}
		}
	`

	payload, err := lt.GraphQL(ctx, query, map[string]any{"ids": ids}, settings)
	if err != nil {
		return nil, err
	}

	issues, err := parseLinearIssueNodes(payload)
	if err != nil {
		return nil, err
	}
	return applyRouting(issues, settings), nil
}

func (lt *LinearTracker) GraphQL(ctx context.Context, query string, variables map[string]any, settings *config.Settings) (map[string]any, error) {
	if settings.Tracker.APIKey == "" {
		return nil, fmt.Errorf("missing_linear_api_token")
	}

	body := map[string]any{
		"query":     query,
		"variables": variables,
	}
	bodyBytes, err := json.Marshal(body)
	if err != nil {
		return nil, fmt.Errorf("linear_api_request: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, "POST", settings.Tracker.Endpoint, bytes.NewReader(bodyBytes))
	if err != nil {
		return nil, fmt.Errorf("linear_api_request: %w", err)
	}
	req.Header.Set("Authorization", settings.Tracker.APIKey)
	req.Header.Set("Content-Type", "application/json")

	resp, err := lt.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("linear_api_request: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("linear_api_request: %w", err)
	}

	var result map[string]any
	if err := json.Unmarshal(respBody, &result); err != nil {
		return nil, fmt.Errorf("linear_api_request: invalid JSON response: %w", err)
	}

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("linear_api_status: %d body=%s", resp.StatusCode, string(respBody))
	}

	return result, nil
}

// --- parsing helpers ---

func parseLinearIssueNodes(payload map[string]any) ([]Issue, error) {
	data, _ := payload["data"].(map[string]any)
	if data == nil {
		return nil, nil
	}
	issues, _ := data["issues"].(map[string]any)
	if issues == nil {
		return nil, nil
	}
	nodes, _ := issues["nodes"].([]any)
	if nodes == nil {
		return nil, nil
	}

	var result []Issue
	for _, node := range nodes {
		m, ok := node.(map[string]any)
		if !ok {
			continue
		}
		issue, err := parseLinearIssue(m)
		if err != nil {
			continue
		}
		result = append(result, issue)
	}
	return result, nil
}

func parseLinearIssue(m map[string]any) (Issue, error) {
	id, _ := m["id"].(string)
	if id == "" {
		return Issue{}, fmt.Errorf("missing issue id")
	}
	identifier, _ := m["identifier"].(string)
	if identifier == "" {
		return Issue{}, fmt.Errorf("missing issue identifier")
	}
	title, _ := m["title"].(string)

	var description *string
	if d, ok := m["description"].(string); ok {
		description = &d
	}

	var priority *int
	if p, ok := m["priority"].(float64); ok {
		pi := int(p)
		priority = &pi
	}

	state := ""
	if stateMap, ok := m["state"].(map[string]any); ok {
		state, _ = stateMap["name"].(string)
	}

	var branchName *string
	if bn, ok := m["branchName"].(string); ok {
		branchName = &bn
	}

	var url *string
	if u, ok := m["url"].(string); ok {
		url = &u
	}

	var labels []string
	if labelsMap, ok := m["labels"].(map[string]any); ok {
		if nodes, ok := labelsMap["nodes"].([]any); ok {
			for _, node := range nodes {
				if lm, ok := node.(map[string]any); ok {
					if name, ok := lm["name"].(string); ok {
						labels = append(labels, strings.ToLower(name))
					}
				}
			}
		}
	}
	if labels == nil {
		labels = []string{}
	}

	var blockedBy []BlockerRef
	if irMap, ok := m["inverseRelations"].(map[string]any); ok {
		if nodes, ok := irMap["nodes"].([]any); ok {
			for _, node := range nodes {
				rm, ok := node.(map[string]any)
				if !ok {
					continue
				}
				relType, _ := rm["type"].(string)
				if !strings.EqualFold(strings.TrimSpace(relType), "blocks") {
					continue
				}
				issueMap, _ := rm["issue"].(map[string]any)
				if issueMap == nil {
					continue
				}
				ref := BlockerRef{}
				if bid, ok := issueMap["id"].(string); ok {
					ref.ID = &bid
				}
				if bident, ok := issueMap["identifier"].(string); ok {
					ref.Identifier = &bident
				}
				if bstate, ok := issueMap["state"].(map[string]any); ok {
					if sn, ok := bstate["name"].(string); ok {
						ref.State = &sn
					}
				}
				blockedBy = append(blockedBy, ref)
			}
		}
	}
	if blockedBy == nil {
		blockedBy = []BlockerRef{}
	}

	var assigneeID, assigneeEmail *string
	if assignee, ok := m["assignee"].(map[string]any); ok {
		if aid, ok := assignee["id"].(string); ok {
			assigneeID = &aid
		}
		if ae, ok := assignee["email"].(string); ok {
			assigneeEmail = &ae
		}
	}

	var createdAt, updatedAt *time.Time
	if ca, ok := m["createdAt"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ca); err == nil {
			createdAt = &t
		}
	}
	if ua, ok := m["updatedAt"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ua); err == nil {
			updatedAt = &t
		}
	}

	return Issue{
		ID:               id,
		Identifier:       identifier,
		Title:            title,
		Description:      description,
		Priority:         priority,
		State:            state,
		BranchName:       branchName,
		URL:              url,
		Labels:           labels,
		BlockedBy:        blockedBy,
		AssignedToWorker: true,
		CreatedAt:        createdAt,
		UpdatedAt:        updatedAt,
		AssigneeID:       assigneeID,
		AssigneeEmail:    assigneeEmail,
	}, nil
}

func applyRouting(issues []Issue, settings *config.Settings) []Issue {
	if settings.Tracker.Assignee == "" {
		return issues
	}
	assignee := NormalizeAssigneeMatchValue(settings.Tracker.Assignee)
	if assignee == "" {
		return issues
	}
	for i := range issues {
		issues[i].AssignedToWorker = isAssignedToWorker(&issues[i], assignee)
	}
	return issues
}

func isAssignedToWorker(issue *Issue, assignee string) bool {
	if issue.AssigneeID != nil {
		if NormalizeAssigneeMatchValue(*issue.AssigneeID) == assignee {
			return true
		}
	}
	if issue.AssigneeEmail != nil {
		if NormalizeAssigneeMatchValue(*issue.AssigneeEmail) == assignee {
			return true
		}
	}
	return false
}
