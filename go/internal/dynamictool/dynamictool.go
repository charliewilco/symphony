// Package dynamictool implements dynamic tool specs and execution for the codex app-server protocol.
package dynamictool

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"symphony/internal/config"
	"symphony/internal/tracker"
)

// LinearGraphQLTool is the name of the linear_graphql dynamic tool.
const LinearGraphQLTool = "linear_graphql"

// ToolSpecs returns the dynamic tool specifications to register with codex.
func ToolSpecs() []map[string]any {
	return []map[string]any{
		{
			"name":        LinearGraphQLTool,
			"description": "Execute a raw GraphQL query or mutation against Linear using Symphony's configured auth.",
			"inputSchema": map[string]any{
				"type":                 "object",
				"additionalProperties": false,
				"required":             []string{"query"},
				"properties": map[string]any{
					"query": map[string]any{
						"type":        "string",
						"description": "GraphQL query or mutation document to execute against Linear.",
					},
					"variables": map[string]any{
						"type":                 []string{"object", "null"},
						"description":          "Optional GraphQL variables object.",
						"additionalProperties": true,
					},
				},
			},
		},
	}
}

// Execute executes a dynamic tool call and returns the result.
func Execute(ctx context.Context, toolName string, arguments any, settings *config.Settings) map[string]any {
	switch toolName {
	case LinearGraphQLTool:
		return executeLinearGraphQL(ctx, arguments, settings)
	case "":
		return failureResponse(map[string]any{
			"error": map[string]any{
				"message":        "Unsupported dynamic tool: null.",
				"supportedTools": []string{LinearGraphQLTool},
			},
		})
	default:
		return failureResponse(map[string]any{
			"error": map[string]any{
				"message":        fmt.Sprintf("Unsupported dynamic tool: %q.", toolName),
				"supportedTools": []string{LinearGraphQLTool},
			},
		})
	}
}

func executeLinearGraphQL(ctx context.Context, arguments any, settings *config.Settings) map[string]any {
	query, variables, errPayload := normalizeArguments(arguments)
	if errPayload != nil {
		return failureResponse(errPayload)
	}

	t := tracker.TrackerForSettings(settings)
	response, apiErr := t.GraphQL(ctx, query, variables, settings)
	if apiErr != nil {
		msg := apiErr.Error()
		if strings.Contains(msg, "missing_linear_api_token") {
			return failureResponse(map[string]any{
				"error": map[string]any{
					"message": "Symphony is missing Linear auth. Set `linear.api_key` in `WORKFLOW.md` or export `LINEAR_API_KEY`.",
				},
			})
		}
		if strings.HasPrefix(msg, "linear_api_status:") {
			parts := strings.Fields(msg)
			status := "500"
			if len(parts) >= 2 {
				status = parts[1]
			}
			return failureResponse(map[string]any{
				"error": map[string]any{
					"message": fmt.Sprintf("Linear GraphQL request failed with HTTP %s.", status),
					"status":  status,
				},
			})
		}
		return failureResponse(map[string]any{
			"error": map[string]any{
				"message": "Linear GraphQL tool execution failed.",
				"reason":  msg,
			},
		})
	}

	// Check for GraphQL errors in response
	success := true
	if errors, ok := response["errors"].([]any); ok && len(errors) > 0 {
		success = false
	}
	return dynamicToolResponse(success, response)
}

func normalizeArguments(arguments any) (string, map[string]any, map[string]any) {
	switch args := arguments.(type) {
	case string:
		trimmed := strings.TrimSpace(args)
		if trimmed == "" {
			return "", nil, map[string]any{
				"error": map[string]any{
					"message": "`linear_graphql` requires a non-empty `query` string.",
				},
			}
		}
		return trimmed, map[string]any{}, nil
	case map[string]any:
		queryRaw, ok := args["query"]
		if !ok {
			return "", nil, map[string]any{
				"error": map[string]any{
					"message": "`linear_graphql` requires a non-empty `query` string.",
				},
			}
		}
		query, ok := queryRaw.(string)
		if !ok || strings.TrimSpace(query) == "" {
			return "", nil, map[string]any{
				"error": map[string]any{
					"message": "`linear_graphql` requires a non-empty `query` string.",
				},
			}
		}
		variables := map[string]any{}
		if vars, ok := args["variables"]; ok && vars != nil {
			varsMap, ok := vars.(map[string]any)
			if !ok {
				return "", nil, map[string]any{
					"error": map[string]any{
						"message": "`linear_graphql.variables` must be a JSON object when provided.",
					},
				}
			}
			variables = varsMap
		}
		return strings.TrimSpace(query), variables, nil
	default:
		return "", nil, map[string]any{
			"error": map[string]any{
				"message": "`linear_graphql` expects either a GraphQL query string or an object with `query` and optional `variables`.",
			},
		}
	}
}

func dynamicToolResponse(success bool, payload any) map[string]any {
	output, _ := json.MarshalIndent(payload, "", "  ")
	outputStr := string(output)
	return map[string]any{
		"success":      success,
		"output":       outputStr,
		"contentItems": []map[string]any{{"type": "inputText", "text": outputStr}},
	}
}

func failureResponse(payload any) map[string]any {
	return dynamicToolResponse(false, payload)
}
