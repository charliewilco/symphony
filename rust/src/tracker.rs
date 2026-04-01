use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use tokio::sync::RwLock;

use crate::config::{Settings, normalize_issue_state};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockerRef {
    pub id: Option<String>,
    pub identifier: Option<String>,
    pub state: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<i64>,
    pub state: String,
    pub branch_name: Option<String>,
    pub url: Option<String>,
    pub labels: Vec<String>,
    pub blocked_by: Vec<BlockerRef>,
    pub assigned_to_worker: bool,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub assignee_id: Option<String>,
    pub assignee_email: Option<String>,
}

impl Issue {
    pub fn to_liquid_object(&self) -> liquid::Object {
        let mut object = liquid::Object::new();
        object.insert("id".into(), liquid::model::Value::scalar(self.id.clone()));
        object.insert(
            "identifier".into(),
            liquid::model::Value::scalar(self.identifier.clone()),
        );
        object.insert(
            "title".into(),
            liquid::model::Value::scalar(self.title.clone()),
        );
        object.insert(
            "description".into(),
            self.description
                .as_ref()
                .map(|text| liquid::model::Value::scalar(text.clone()))
                .unwrap_or(liquid::model::Value::Nil),
        );
        object.insert(
            "priority".into(),
            self.priority
                .map(liquid::model::Value::scalar)
                .unwrap_or(liquid::model::Value::Nil),
        );
        object.insert(
            "state".into(),
            liquid::model::Value::scalar(self.state.clone()),
        );
        object.insert(
            "branch_name".into(),
            self.branch_name
                .as_ref()
                .map(|text| liquid::model::Value::scalar(text.clone()))
                .unwrap_or(liquid::model::Value::Nil),
        );
        object.insert(
            "url".into(),
            self.url
                .as_ref()
                .map(|text| liquid::model::Value::scalar(text.clone()))
                .unwrap_or(liquid::model::Value::Nil),
        );
        object.insert(
            "labels".into(),
            liquid::model::Value::Array(
                self.labels
                    .iter()
                    .cloned()
                    .map(liquid::model::Value::scalar)
                    .collect(),
            ),
        );
        object.insert(
            "blocked_by".into(),
            liquid::model::Value::Array(
                self.blocked_by
                    .iter()
                    .map(|blocker| {
                        let mut blocker_object = liquid::Object::new();
                        blocker_object.insert(
                            "id".into(),
                            blocker
                                .id
                                .as_ref()
                                .map(|value| liquid::model::Value::scalar(value.clone()))
                                .unwrap_or(liquid::model::Value::Nil),
                        );
                        blocker_object.insert(
                            "identifier".into(),
                            blocker
                                .identifier
                                .as_ref()
                                .map(|value| liquid::model::Value::scalar(value.clone()))
                                .unwrap_or(liquid::model::Value::Nil),
                        );
                        blocker_object.insert(
                            "state".into(),
                            blocker
                                .state
                                .as_ref()
                                .map(|value| liquid::model::Value::scalar(value.clone()))
                                .unwrap_or(liquid::model::Value::Nil),
                        );
                        liquid::model::Value::Object(blocker_object)
                    })
                    .collect(),
            ),
        );
        object.insert(
            "created_at".into(),
            self.created_at
                .map(|value| liquid::model::Value::scalar(value.to_rfc3339()))
                .unwrap_or(liquid::model::Value::Nil),
        );
        object.insert(
            "updated_at".into(),
            self.updated_at
                .map(|value| liquid::model::Value::scalar(value.to_rfc3339()))
                .unwrap_or(liquid::model::Value::Nil),
        );
        object.insert(
            "assigned_to_worker".into(),
            liquid::model::Value::scalar(self.assigned_to_worker),
        );
        object
    }
}

#[async_trait]
pub trait Tracker: Send + Sync {
    async fn fetch_candidate_issues(&self, settings: &Settings) -> Result<Vec<Issue>>;
    async fn fetch_issue_states_by_ids(
        &self,
        ids: &[String],
        settings: &Settings,
    ) -> Result<Vec<Issue>>;
    async fn fetch_issues_by_states(
        &self,
        states: &[String],
        settings: &Settings,
    ) -> Result<Vec<Issue>>;
    async fn graphql(
        &self,
        query: &str,
        variables: JsonValue,
        settings: &Settings,
    ) -> Result<JsonValue>;
    async fn create_comment(&self, issue_id: &str, body: &str, settings: &Settings) -> Result<()>;
    async fn update_issue_state(
        &self,
        issue_id: &str,
        state_name: &str,
        settings: &Settings,
    ) -> Result<()>;
}

pub fn tracker_for_settings(settings: &Settings) -> Arc<dyn Tracker> {
    match settings.tracker.kind.as_deref() {
        Some("memory") => Arc::new(MemoryTracker::default()),
        _ => Arc::new(LinearTracker::default()),
    }
}

#[derive(Default)]
pub struct LinearTracker {
    client: reqwest::Client,
}

#[derive(Clone, Debug)]
struct AssigneeFilter {
    match_values: HashSet<String>,
}

#[async_trait]
impl Tracker for LinearTracker {
    async fn fetch_candidate_issues(&self, settings: &Settings) -> Result<Vec<Issue>> {
        let project_slug = settings
            .tracker
            .project_slug
            .clone()
            .ok_or_else(|| anyhow!("missing_linear_project_slug"))?;
        let assignee_filter = self.routing_assignee_filter(settings).await?;

        let query = r#"
          query CandidateIssues($project: String!, $states: [String!]) {
            issues(
              filter: {
                project: { slugId: { eq: $project } }
                state: { name: { in: $states } }
              }
            ) {
              nodes {
                id
                identifier
                title
                description
                priority
                url
                branchName
                createdAt
                updatedAt
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
        "#;

        let variables = json!({
            "project": project_slug,
            "states": settings.tracker.active_states,
        });

        let payload = self.graphql(query, variables, settings).await?;
        let issues = parse_linear_issue_nodes(
            payload
                .pointer("/data/issues/nodes")
                .cloned()
                .unwrap_or(JsonValue::Array(vec![])),
        )?;
        Ok(apply_routing_to_issues(issues, assignee_filter.as_ref()))
    }

    async fn fetch_issue_states_by_ids(
        &self,
        ids: &[String],
        settings: &Settings,
    ) -> Result<Vec<Issue>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let assignee_filter = self.routing_assignee_filter(settings).await?;

        let query = r#"
          query IssueStates($ids: [ID!]) {
            issues(filter: { id: { in: $ids } }) {
              nodes {
                id
                identifier
                title
                description
                priority
                url
                branchName
                createdAt
                updatedAt
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
        "#;

        let payload = self.graphql(query, json!({ "ids": ids }), settings).await?;
        let issues = parse_linear_issue_nodes(
            payload
                .pointer("/data/issues/nodes")
                .cloned()
                .unwrap_or(JsonValue::Array(vec![])),
        )?;
        Ok(apply_routing_to_issues(issues, assignee_filter.as_ref()))
    }

    async fn fetch_issues_by_states(
        &self,
        states: &[String],
        settings: &Settings,
    ) -> Result<Vec<Issue>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }

        let project_slug = settings
            .tracker
            .project_slug
            .clone()
            .ok_or_else(|| anyhow!("missing_linear_project_slug"))?;

        let query = r#"
          query IssuesByStates($project: String!, $states: [String!]) {
            issues(
              filter: {
                project: { slugId: { eq: $project } }
                state: { name: { in: $states } }
              }
            ) {
              nodes {
                id
                identifier
                title
                description
                priority
                url
                branchName
                createdAt
                updatedAt
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
        "#;

        let payload = self
            .graphql(
                query,
                json!({ "project": project_slug, "states": states }),
                settings,
            )
            .await?;
        let issues = parse_linear_issue_nodes(
            payload
                .pointer("/data/issues/nodes")
                .cloned()
                .unwrap_or(JsonValue::Array(vec![])),
        )?;
        Ok(issues)
    }

    async fn graphql(
        &self,
        query: &str,
        variables: JsonValue,
        settings: &Settings,
    ) -> Result<JsonValue> {
        let api_key = settings
            .tracker
            .api_key
            .clone()
            .ok_or_else(|| anyhow!("missing_linear_api_token"))?;

        let response = self
            .client
            .post(&settings.tracker.endpoint)
            .header(AUTHORIZATION, api_key)
            .header(CONTENT_TYPE, "application/json")
            .json(&json!({
                "query": query,
                "variables": variables
            }))
            .send()
            .await
            .map_err(|error| anyhow!("linear_api_request: {error}"))?;

        let status = response.status();
        let body: JsonValue = response
            .json()
            .await
            .map_err(|error| anyhow!("linear_api_request: {error}"))?;
        if !status.is_success() {
            bail!("linear_api_status: {} body={body}", status.as_u16());
        }
        Ok(body)
    }

    async fn create_comment(&self, issue_id: &str, body: &str, settings: &Settings) -> Result<()> {
        let payload = self
            .graphql(
                r#"
                  mutation SymphonyCreateComment($issueId: String!, $body: String!) {
                    commentCreate(input: {issueId: $issueId, body: $body}) {
                      success
                    }
                  }
                "#,
                json!({ "issueId": issue_id, "body": body }),
                settings,
            )
            .await?;

        if payload
            .pointer("/data/commentCreate/success")
            .and_then(JsonValue::as_bool)
            == Some(true)
        {
            Ok(())
        } else {
            bail!("comment_create_failed")
        }
    }

    async fn update_issue_state(
        &self,
        issue_id: &str,
        state_name: &str,
        settings: &Settings,
    ) -> Result<()> {
        let lookup = self
            .graphql(
                r#"
                  query SymphonyResolveStateId($issueId: String!, $stateName: String!) {
                    issue(id: $issueId) {
                      team {
                        states(filter: {name: {eq: $stateName}}, first: 1) {
                          nodes {
                            id
                          }
                        }
                      }
                    }
                  }
                "#,
                json!({ "issueId": issue_id, "stateName": state_name }),
                settings,
            )
            .await?;
        let state_id = lookup
            .pointer("/data/issue/team/states/nodes/0/id")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| anyhow!("state_not_found"))?;

        let payload = self
            .graphql(
                r#"
                  mutation SymphonyUpdateIssueState($issueId: String!, $stateId: String!) {
                    issueUpdate(id: $issueId, input: {stateId: $stateId}) {
                      success
                    }
                  }
                "#,
                json!({ "issueId": issue_id, "stateId": state_id }),
                settings,
            )
            .await?;

        if payload
            .pointer("/data/issueUpdate/success")
            .and_then(JsonValue::as_bool)
            == Some(true)
        {
            Ok(())
        } else {
            bail!("issue_update_failed")
        }
    }
}

impl LinearTracker {
    async fn routing_assignee_filter(&self, settings: &Settings) -> Result<Option<AssigneeFilter>> {
        match settings.tracker.assignee.as_deref() {
            None => Ok(None),
            Some(assignee) => self.build_assignee_filter(assignee, settings).await,
        }
    }

    async fn build_assignee_filter(
        &self,
        assignee: &str,
        settings: &Settings,
    ) -> Result<Option<AssigneeFilter>> {
        let Some(normalized) = normalize_assignee_match_value(assignee) else {
            return Ok(None);
        };
        if normalized.eq_ignore_ascii_case("me") {
            let payload = self
                .graphql(
                    r#"
                      query ViewerIdentity {
                        viewer {
                          id
                        }
                      }
                    "#,
                    json!({}),
                    settings,
                )
                .await?;
            let viewer_id = payload
                .pointer("/data/viewer/id")
                .and_then(JsonValue::as_str)
                .and_then(normalize_assignee_match_value)
                .ok_or_else(|| anyhow!("missing_linear_viewer_identity"))?;
            return Ok(Some(AssigneeFilter {
                match_values: HashSet::from([viewer_id]),
            }));
        }

        Ok(Some(AssigneeFilter {
            match_values: HashSet::from([normalized]),
        }))
    }
}

#[derive(Clone, Default)]
pub struct MemoryTracker {
    issues: Arc<RwLock<HashMap<String, Issue>>>,
}

impl MemoryTracker {
    pub async fn set_issues(&self, issues: Vec<Issue>) {
        let mut guard = self.issues.write().await;
        *guard = issues
            .into_iter()
            .map(|issue| (issue.id.clone(), issue))
            .collect();
    }
}

#[async_trait]
impl Tracker for MemoryTracker {
    async fn fetch_candidate_issues(&self, settings: &Settings) -> Result<Vec<Issue>> {
        let guard = self.issues.read().await;
        Ok(guard
            .values()
            .filter(|issue| {
                settings.tracker.active_states.iter().any(|state| {
                    normalize_issue_state(state) == normalize_issue_state(&issue.state)
                })
            })
            .cloned()
            .collect())
    }

    async fn fetch_issue_states_by_ids(
        &self,
        ids: &[String],
        _settings: &Settings,
    ) -> Result<Vec<Issue>> {
        let guard = self.issues.read().await;
        Ok(ids.iter().filter_map(|id| guard.get(id).cloned()).collect())
    }

    async fn fetch_issues_by_states(
        &self,
        states: &[String],
        _settings: &Settings,
    ) -> Result<Vec<Issue>> {
        let guard = self.issues.read().await;
        Ok(guard
            .values()
            .filter(|issue| {
                states.iter().any(|state| {
                    normalize_issue_state(state) == normalize_issue_state(&issue.state)
                })
            })
            .cloned()
            .collect())
    }

    async fn graphql(
        &self,
        _query: &str,
        _variables: JsonValue,
        _settings: &Settings,
    ) -> Result<JsonValue> {
        Ok(json!({ "data": {} }))
    }

    async fn create_comment(
        &self,
        _issue_id: &str,
        _body: &str,
        _settings: &Settings,
    ) -> Result<()> {
        Ok(())
    }

    async fn update_issue_state(
        &self,
        issue_id: &str,
        state_name: &str,
        _settings: &Settings,
    ) -> Result<()> {
        let mut guard = self.issues.write().await;
        let issue = guard
            .get_mut(issue_id)
            .ok_or_else(|| anyhow!("issue_not_found"))?;
        issue.state = state_name.to_string();
        Ok(())
    }
}

fn parse_linear_issue_nodes(value: JsonValue) -> Result<Vec<Issue>> {
    let nodes = value.as_array().cloned().unwrap_or_default();
    nodes.into_iter().map(parse_linear_issue).collect()
}

fn parse_linear_issue(value: JsonValue) -> Result<Issue> {
    let assignee_id = value
        .pointer("/assignee/id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let assignee_email = value
        .pointer("/assignee/email")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);

    let state = value
        .pointer("/state/name")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();

    let labels = value
        .pointer("/labels/nodes")
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|label| {
            label
                .get("name")
                .and_then(JsonValue::as_str)
                .map(|name| name.to_ascii_lowercase())
        })
        .collect();

    let blocked_by = value
        .pointer("/inverseRelations/nodes")
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|relation| {
            relation
                .get("type")
                .and_then(JsonValue::as_str)
                .is_some_and(|relation_type| relation_type.trim().eq_ignore_ascii_case("blocks"))
        })
        .map(|relation| BlockerRef {
            id: relation
                .pointer("/issue/id")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            identifier: relation
                .pointer("/issue/identifier")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            state: relation
                .pointer("/issue/state/name")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
        })
        .collect();

    Ok(Issue {
        id: value
            .get("id")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| anyhow!("missing issue id"))?
            .to_string(),
        identifier: value
            .get("identifier")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| anyhow!("missing issue identifier"))?
            .to_string(),
        title: value
            .get("title")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        description: value
            .get("description")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        priority: value.get("priority").and_then(JsonValue::as_i64),
        state,
        branch_name: value
            .get("branchName")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        url: value
            .get("url")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        labels,
        blocked_by,
        assigned_to_worker: true,
        created_at: value
            .get("createdAt")
            .and_then(JsonValue::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc)),
        updated_at: value
            .get("updatedAt")
            .and_then(JsonValue::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc)),
        assignee_id,
        assignee_email,
    })
}

fn apply_routing_to_issues(
    mut issues: Vec<Issue>,
    assignee_filter: Option<&AssigneeFilter>,
) -> Vec<Issue> {
    for issue in &mut issues {
        issue.assigned_to_worker = assigned_to_worker(issue, assignee_filter);
    }
    issues
}

fn assigned_to_worker(issue: &Issue, assignee_filter: Option<&AssigneeFilter>) -> bool {
    let Some(assignee_filter) = assignee_filter else {
        return true;
    };
    issue
        .assignee_id
        .as_deref()
        .and_then(normalize_assignee_match_value)
        .is_some_and(|assignee_id| assignee_filter.match_values.contains(&assignee_id))
        || issue
            .assignee_email
            .as_deref()
            .and_then(normalize_assignee_match_value)
            .is_some_and(|assignee_email| assignee_filter.match_values.contains(&assignee_email))
}

fn normalize_assignee_match_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Settings, settings_from_toml_str};

    fn settings() -> Settings {
        settings_from_toml_str("[tracker]\nkind = \"memory\"\n")
    }

    fn issue() -> Issue {
        Issue {
            id: "issue-1".to_string(),
            identifier: "MT-1".to_string(),
            title: "Test".to_string(),
            description: Some("Test".to_string()),
            priority: None,
            state: "In Progress".to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            assigned_to_worker: true,
            created_at: None,
            updated_at: None,
            assignee_id: None,
            assignee_email: None,
        }
    }

    #[tokio::test]
    async fn memory_tracker_updates_issue_state_and_accepts_comments() {
        let tracker = MemoryTracker::default();
        tracker.set_issues(vec![issue()]).await;
        let settings = settings();

        tracker
            .create_comment("issue-1", "hello", &settings)
            .await
            .unwrap();
        tracker
            .update_issue_state("issue-1", "Done", &settings)
            .await
            .unwrap();

        let updated = tracker
            .fetch_issue_states_by_ids(&["issue-1".to_string()], &settings)
            .await
            .unwrap();
        assert_eq!(updated[0].state, "Done");
    }

    #[test]
    fn assigned_to_worker_accepts_matching_assignee_id_or_email() {
        let mut issue = issue();
        issue.assignee_id = Some("worker-id".to_string());
        issue.assignee_email = Some("worker@example.com".to_string());

        let id_filter = AssigneeFilter {
            match_values: HashSet::from([String::from("worker-id")]),
        };
        assert!(super::assigned_to_worker(&issue, Some(&id_filter)));

        let email_filter = AssigneeFilter {
            match_values: HashSet::from([String::from("worker@example.com")]),
        };
        assert!(super::assigned_to_worker(&issue, Some(&email_filter)));
    }

    #[test]
    fn assigned_to_worker_rejects_non_matching_assignee_filter() {
        let mut issue = issue();
        issue.assignee_id = Some("worker-id".to_string());

        let filter = AssigneeFilter {
            match_values: HashSet::from([String::from("somebody-else")]),
        };
        assert!(!super::assigned_to_worker(&issue, Some(&filter)));
    }

    #[test]
    fn parse_linear_issue_extracts_blockers_from_inverse_relations() {
        let issue = parse_linear_issue(json!({
            "id": "issue-1",
            "identifier": "MT-1",
            "title": "Test",
            "description": "Body",
            "priority": 2,
            "state": { "name": "Todo" },
            "labels": { "nodes": [] },
            "inverseRelations": {
                "nodes": [
                    {
                        "type": "blocks",
                        "issue": {
                            "id": "issue-2",
                            "identifier": "MT-2",
                            "state": { "name": "In Progress" }
                        }
                    },
                    {
                        "type": "relates",
                        "issue": {
                            "id": "issue-3",
                            "identifier": "MT-3",
                            "state": { "name": "Todo" }
                        }
                    }
                ]
            }
        }))
        .unwrap();

        assert_eq!(issue.blocked_by.len(), 1);
        assert_eq!(issue.blocked_by[0].id.as_deref(), Some("issue-2"));
        assert_eq!(issue.blocked_by[0].identifier.as_deref(), Some("MT-2"));
        assert_eq!(issue.blocked_by[0].state.as_deref(), Some("In Progress"));
    }
}
