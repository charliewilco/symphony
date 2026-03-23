use anyhow::Result;
use serde_json::{Value as JsonValue, json};

use crate::config::Settings;
use crate::tracker::tracker_for_settings;

pub const LINEAR_GRAPHQL_TOOL: &str = "linear_graphql";

pub fn tool_specs() -> Vec<JsonValue> {
    vec![json!({
        "name": LINEAR_GRAPHQL_TOOL,
        "description": "Execute a raw GraphQL query or mutation against Linear using Symphony's configured auth.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "GraphQL query or mutation document to execute against Linear."
                },
                "variables": {
                    "type": ["object", "null"],
                    "description": "Optional GraphQL variables object.",
                    "additionalProperties": true
                }
            }
        }
    })]
}

pub async fn execute(tool: Option<&str>, arguments: JsonValue, settings: &Settings) -> JsonValue {
    match tool {
        Some(LINEAR_GRAPHQL_TOOL) => execute_linear_graphql(arguments, settings).await,
        Some(other) => failure_response(json!({
            "error": {
                "message": format!("Unsupported dynamic tool: {other:?}."),
                "supportedTools": [LINEAR_GRAPHQL_TOOL]
            }
        })),
        None => failure_response(json!({
            "error": {
                "message": "Unsupported dynamic tool: null.",
                "supportedTools": [LINEAR_GRAPHQL_TOOL]
            }
        })),
    }
}

async fn execute_linear_graphql(arguments: JsonValue, settings: &Settings) -> JsonValue {
    match normalize_arguments(arguments) {
        Ok((query, variables)) => {
            let tracker = tracker_for_settings(settings);
            match tracker.graphql(&query, variables, settings).await {
                Ok(response) => {
                    let success = response
                        .get("errors")
                        .and_then(JsonValue::as_array)
                        .is_none_or(|errors| errors.is_empty());
                    dynamic_tool_response(success, response)
                }
                Err(error) => {
                    let message = error.to_string();
                    if message.contains("missing_linear_api_token") {
                        failure_response(json!({
                            "error": {
                                "message": "Symphony is missing Linear auth. Set `linear.api_key` in `WORKFLOW.md` or export `LINEAR_API_KEY`."
                            }
                        }))
                    } else if let Some(status) = message.strip_prefix("linear_api_status: ") {
                        let digits = status.split_whitespace().next().unwrap_or("500");
                        failure_response(json!({
                            "error": {
                                "message": format!("Linear GraphQL request failed with HTTP {digits}."),
                                "status": digits.parse::<u16>().unwrap_or(500)
                            }
                        }))
                    } else if message.contains("linear_api_request:") {
                        failure_response(json!({
                            "error": {
                                "message": "Linear GraphQL request failed before receiving a successful response.",
                                "reason": message
                            }
                        }))
                    } else {
                        failure_response(json!({
                            "error": {
                                "message": "Linear GraphQL tool execution failed.",
                                "reason": message
                            }
                        }))
                    }
                }
            }
        }
        Err(payload) => failure_response(payload),
    }
}

fn normalize_arguments(arguments: JsonValue) -> Result<(String, JsonValue), JsonValue> {
    match arguments {
        JsonValue::String(query) => {
            let query = query.trim();
            if query.is_empty() {
                Err(
                    json!({ "error": { "message": "`linear_graphql` requires a non-empty `query` string." }}),
                )
            } else {
                Ok((query.to_string(), json!({})))
            }
        }
        JsonValue::Object(object) => {
            let query = object
                .get("query")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .ok_or_else(|| json!({ "error": { "message": "`linear_graphql` requires a non-empty `query` string." }}))?;
            let variables = object
                .get("variables")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !variables.is_object() && !variables.is_null() {
                return Err(
                    json!({ "error": { "message": "`linear_graphql.variables` must be a JSON object when provided." }}),
                );
            }
            Ok((
                query.to_string(),
                if variables.is_null() {
                    json!({})
                } else {
                    variables
                },
            ))
        }
        _ => Err(json!({
            "error": {
                "message": "`linear_graphql` expects either a GraphQL query string or an object with `query` and optional `variables`."
            }
        })),
    }
}

fn dynamic_tool_response(success: bool, payload: JsonValue) -> JsonValue {
    let output = if payload.is_object() || payload.is_array() {
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
    } else {
        payload.to_string()
    };
    json!({
        "success": success,
        "output": output,
        "contentItems": [{ "type": "inputText", "text": output }]
    })
}

fn failure_response(payload: JsonValue) -> JsonValue {
    dynamic_tool_response(false, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CliOverrides, Settings};
    use crate::workflow::LoadedWorkflow;

    #[tokio::test]
    async fn tool_specs_advertises_contract() {
        let specs = tool_specs();
        assert_eq!(specs[0]["name"], "linear_graphql");
    }

    #[tokio::test]
    async fn invalid_arguments_return_failure() {
        let settings = Settings::from_workflow(
            &LoadedWorkflow {
                config: serde_yaml::from_str("tracker:\n  kind: memory\n").unwrap(),
                prompt_template: String::new(),
                prompt: String::new(),
            },
            &CliOverrides::default(),
        )
        .unwrap();

        let payload = execute(Some(LINEAR_GRAPHQL_TOOL), json!(["bad"]), &settings).await;
        assert_eq!(payload["success"], false);
    }
}
