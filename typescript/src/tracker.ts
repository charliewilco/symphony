// Linear issue tracker client — §11 of the spec
import type { Issue, BlockerRef } from "./types.ts";
import type { TrackerSettings } from "./config.ts";

export type TrackerError =
  | { code: "unsupported_tracker_kind"; message: string }
  | { code: "missing_tracker_api_key"; message: string }
  | { code: "missing_tracker_project_slug"; message: string }
  | { code: "linear_api_request"; message: string }
  | { code: "linear_api_status"; status: number; message: string }
  | { code: "linear_graphql_errors"; errors: unknown[] }
  | { code: "linear_unknown_payload"; message: string }
  | { code: "linear_missing_end_cursor"; message: string };

const PAGE_SIZE = 50;
const NETWORK_TIMEOUT_MS = 30_000;

// §11.2 GraphQL queries

const CANDIDATE_ISSUES_QUERY = `
query CandidateIssues($projectSlug: String!, $states: [String!]!, $after: String, $first: Int!) {
  issues(
    filter: {
      project: { slugId: { eq: $projectSlug } }
      state: { name: { in: $states } }
    }
    first: $first
    after: $after
    orderBy: createdAt
  ) {
    pageInfo {
      hasNextPage
      endCursor
    }
    nodes {
      id
      identifier
      title
      description
      priority
      branchName
      url
      createdAt
      updatedAt
      state {
        name
      }
      labels {
        nodes {
          name
        }
      }
      relations(filter: { type: { eq: "blocks" } }) {
        nodes {
          relatedIssue {
            id
            identifier
            state {
              name
            }
          }
        }
      }
    }
  }
}
`.trim();

const ISSUES_BY_STATES_QUERY = `
query IssuesByStates($states: [String!]!, $projectSlug: String!, $after: String, $first: Int!) {
  issues(
    filter: {
      project: { slugId: { eq: $projectSlug } }
      state: { name: { in: $states } }
    }
    first: $first
    after: $after
  ) {
    pageInfo {
      hasNextPage
      endCursor
    }
    nodes {
      id
      identifier
    }
  }
}
`.trim();

const ISSUE_STATES_BY_IDS_QUERY = `
query IssueStatesByIds($ids: [ID!]!) {
  issues(filter: { id: { in: $ids } }) {
    nodes {
      id
      identifier
      state {
        name
      }
    }
  }
}
`.trim();

async function graphql(
  endpoint: string,
  apiKey: string,
  query: string,
  variables: Record<string, unknown>
): Promise<Record<string, unknown>> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), NETWORK_TIMEOUT_MS);

  let response: Response;
  try {
    response = await fetch(endpoint, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: apiKey,
      },
      body: JSON.stringify({ query, variables }),
      signal: controller.signal,
    });
  } catch (err) {
    clearTimeout(timeout);
    throw {
      code: "linear_api_request",
      message: `Network error: ${err}`,
    } as TrackerError;
  }
  clearTimeout(timeout);

  if (!response.ok) {
    throw {
      code: "linear_api_status",
      status: response.status,
      message: `HTTP ${response.status}: ${response.statusText}`,
    } as TrackerError;
  }

  let body: Record<string, unknown>;
  try {
    body = (await response.json()) as Record<string, unknown>;
  } catch {
    throw {
      code: "linear_unknown_payload",
      message: "Response was not valid JSON",
    } as TrackerError;
  }

  const errors = body["errors"] as unknown[] | undefined;
  if (errors && errors.length > 0) {
    throw { code: "linear_graphql_errors", errors } as TrackerError;
  }

  return (body["data"] as Record<string, unknown>) ?? {};
}

// §11.3 Normalization
function normalizeIssueNode(node: Record<string, unknown>): Issue {
  const stateNode = node["state"] as Record<string, unknown> | null | undefined;
  const labelsConn = node["labels"] as Record<string, unknown> | null | undefined;
  const labelsNodes = (labelsConn?.["nodes"] as Record<string, unknown>[]) ?? [];
  const relationsConn = node["relations"] as Record<string, unknown> | null | undefined;
  const relationsNodes = (relationsConn?.["nodes"] as Record<string, unknown>[]) ?? [];

  const blocked_by: BlockerRef[] = relationsNodes.map((rel) => {
    const related = rel["relatedIssue"] as Record<string, unknown> | null | undefined;
    const relState = related?.["state"] as Record<string, unknown> | null | undefined;
    return {
      id: typeof related?.["id"] === "string" ? related["id"] : null,
      identifier: typeof related?.["identifier"] === "string" ? related["identifier"] : null,
      state: typeof relState?.["name"] === "string" ? relState["name"] : null,
    };
  });

  const priorityRaw = node["priority"];
  const priority =
    typeof priorityRaw === "number" ? priorityRaw :
    typeof priorityRaw === "string" ? parseInt(priorityRaw, 10) : null;

  return {
    id: String(node["id"]),
    identifier: String(node["identifier"]),
    title: String(node["title"] ?? ""),
    description: typeof node["description"] === "string" ? node["description"] : null,
    priority: priority !== null && Number.isFinite(priority) ? priority : null,
    state: typeof stateNode?.["name"] === "string" ? stateNode["name"] : "",
    branch_name: typeof node["branchName"] === "string" ? node["branchName"] : null,
    url: typeof node["url"] === "string" ? node["url"] : null,
    labels: labelsNodes
      .map((l) => (typeof l["name"] === "string" ? l["name"].toLowerCase() : ""))
      .filter(Boolean),
    blocked_by,
    created_at:
      typeof node["createdAt"] === "string" ? new Date(node["createdAt"]) : null,
    updated_at:
      typeof node["updatedAt"] === "string" ? new Date(node["updatedAt"]) : null,
  };
}

export class LinearTracker {
  private endpoint: string;
  private apiKey: string;
  private projectSlug: string;

  constructor(settings: TrackerSettings) {
    if (!settings.api_key) {
      throw { code: "missing_tracker_api_key", message: "Linear API key missing" } as TrackerError;
    }
    if (!settings.project_slug) {
      throw { code: "missing_tracker_project_slug", message: "Linear project_slug missing" } as TrackerError;
    }
    this.endpoint = settings.endpoint;
    this.apiKey = settings.api_key;
    this.projectSlug = settings.project_slug;
  }

  /** §11.1.1 Fetch all candidate issues in active states (paginated). */
  async fetchCandidateIssues(activeStates: string[]): Promise<Issue[]> {
    const issues: Issue[] = [];
    let after: string | null = null;

    while (true) {
      const data = await graphql(this.endpoint, this.apiKey, CANDIDATE_ISSUES_QUERY, {
        projectSlug: this.projectSlug,
        states: activeStates,
        after,
        first: PAGE_SIZE,
      });

      const conn = data["issues"] as Record<string, unknown> | undefined;
      if (!conn) throw { code: "linear_unknown_payload", message: "Missing issues field" } as TrackerError;

      const nodes = (conn["nodes"] as Record<string, unknown>[]) ?? [];
      for (const node of nodes) {
        issues.push(normalizeIssueNode(node));
      }

      const pageInfo = conn["pageInfo"] as Record<string, unknown> | undefined;
      if (!pageInfo?.["hasNextPage"]) break;

      const endCursor = pageInfo["endCursor"];
      if (typeof endCursor !== "string") {
        throw { code: "linear_missing_end_cursor", message: "Missing endCursor in pagination" } as TrackerError;
      }
      after = endCursor;
    }

    return issues;
  }

  /** §11.1.2 Fetch issues by state names (for startup terminal cleanup). */
  async fetchIssuesByStates(stateNames: string[]): Promise<Array<{ id: string; identifier: string }>> {
    const results: Array<{ id: string; identifier: string }> = [];
    let after: string | null = null;

    while (true) {
      const data = await graphql(this.endpoint, this.apiKey, ISSUES_BY_STATES_QUERY, {
        states: stateNames,
        projectSlug: this.projectSlug,
        after,
        first: PAGE_SIZE,
      });

      const conn = data["issues"] as Record<string, unknown> | undefined;
      if (!conn) break;

      const nodes = (conn["nodes"] as Record<string, unknown>[]) ?? [];
      for (const node of nodes) {
        results.push({
          id: String(node["id"]),
          identifier: String(node["identifier"]),
        });
      }

      const pageInfo = conn["pageInfo"] as Record<string, unknown> | undefined;
      if (!pageInfo?.["hasNextPage"]) break;

      const endCursor = pageInfo["endCursor"];
      if (typeof endCursor !== "string") break;
      after = endCursor;
    }

    return results;
  }

  /** §11.1.3 Fetch current state for specific issue IDs (reconciliation). */
  async fetchIssueStatesByIds(
    issueIds: string[]
  ): Promise<Map<string, { identifier: string; state: string }>> {
    if (issueIds.length === 0) return new Map();

    const data = await graphql(this.endpoint, this.apiKey, ISSUE_STATES_BY_IDS_QUERY, {
      ids: issueIds,
    });

    const conn = data["issues"] as Record<string, unknown> | undefined;
    const nodes = ((conn?.["nodes"] as Record<string, unknown>[]) ?? []);

    const result = new Map<string, { identifier: string; state: string }>();
    for (const node of nodes) {
      const stateNode = node["state"] as Record<string, unknown> | null | undefined;
      const id = String(node["id"]);
      const identifier = String(node["identifier"]);
      const state = typeof stateNode?.["name"] === "string" ? stateNode["name"] : "";
      result.set(id, { identifier, state });
    }
    return result;
  }

  /** Execute a raw GraphQL query (linear_graphql tool extension — §10.5). */
  async executeRawGraphQL(
    query: string,
    variables?: Record<string, unknown>
  ): Promise<{ success: boolean; data?: unknown; errors?: unknown[] }> {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), NETWORK_TIMEOUT_MS);

    let response: Response;
    try {
      response = await fetch(this.endpoint, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: this.apiKey,
        },
        body: JSON.stringify({ query, variables: variables ?? {} }),
        signal: controller.signal,
      });
    } catch (err) {
      clearTimeout(timeout);
      return { success: false, errors: [{ message: String(err) }] };
    }
    clearTimeout(timeout);

    let body: Record<string, unknown>;
    try {
      body = (await response.json()) as Record<string, unknown>;
    } catch {
      return { success: false, errors: [{ message: "Invalid JSON response" }] };
    }

    const errors = body["errors"] as unknown[] | undefined;
    if (errors && errors.length > 0) {
      return { success: false, data: body, errors };
    }
    return { success: true, data: body["data"] };
  }
}

export function createTracker(settings: TrackerSettings): LinearTracker {
  if (settings.kind !== "linear") {
    throw {
      code: "unsupported_tracker_kind",
      message: `Tracker kind "${settings.kind}" is not supported`,
    } as TrackerError;
  }
  return new LinearTracker(settings);
}
