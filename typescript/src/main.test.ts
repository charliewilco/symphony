// Tests for Symphony TypeScript implementation
import { describe, test, expect } from "bun:test";
import { parseWorkflow } from "./workflow.ts";
import { settingsFromWorkflow, validateForDispatch } from "./config.ts";
import { sanitizeWorkspaceKey } from "./workspace.ts";
import { buildPrompt } from "./prompt.ts";
import type { Issue } from "./types.ts";

// §5.2 Workflow parsing
describe("parseWorkflow", () => {
  test("parses front matter and body", () => {
    const raw = `---
tracker:
  kind: linear
  project_slug: test
---
Work on {{issue.identifier}}: {{issue.title}}
`.trim();
    const def = parseWorkflow(raw);
    expect(def.config).toMatchObject({ tracker: { kind: "linear", project_slug: "test" } });
    expect(def.prompt_template).toBe("Work on {{issue.identifier}}: {{issue.title}}");
  });

  test("treats entire content as prompt when no front matter", () => {
    const raw = "You are working on an issue.";
    const def = parseWorkflow(raw);
    expect(def.config).toEqual({});
    expect(def.prompt_template).toBe("You are working on an issue.");
  });

  test("throws workflow_parse_error on invalid YAML", () => {
    const raw = `---
tracker: [invalid: yaml
---
prompt`;
    expect(() => parseWorkflow(raw)).toThrow();
  });

  test("throws workflow_front_matter_not_a_map when front matter is not an object", () => {
    const raw = `---
- item1
- item2
---
prompt`;
    expect(() => parseWorkflow(raw)).toThrow();
  });

  test("handles empty front matter", () => {
    const raw = `---
---
prompt body`;
    const def = parseWorkflow(raw);
    expect(def.config).toEqual({});
    expect(def.prompt_template).toBe("prompt body");
  });
});

// §6 Config layer
describe("settingsFromWorkflow", () => {
  test("applies defaults when config is empty", () => {
    const def = { config: {}, prompt_template: "" };
    const settings = settingsFromWorkflow(def);
    expect(settings.polling.interval_ms).toBe(30_000);
    expect(settings.agent.max_concurrent_agents).toBe(10);
    expect(settings.agent.max_turns).toBe(20);
    expect(settings.agent.max_retry_backoff_ms).toBe(300_000);
    expect(settings.codex.command).toBe("codex app-server");
    expect(settings.codex.turn_timeout_ms).toBe(3_600_000);
    expect(settings.codex.stall_timeout_ms).toBe(300_000);
    expect(settings.hooks.timeout_ms).toBe(60_000);
    expect(settings.tracker.active_states).toEqual(["Todo", "In Progress"]);
    expect(settings.tracker.terminal_states).toContain("Done");
  });

  test("reads tracker config correctly", () => {
    const def = {
      config: {
        tracker: {
          kind: "linear",
          project_slug: "my-project",
          api_key: "lin_api_test",
          active_states: ["In Progress"],
          terminal_states: ["Done", "Cancelled"],
        },
      },
      prompt_template: "",
    };
    const settings = settingsFromWorkflow(def);
    expect(settings.tracker.kind).toBe("linear");
    expect(settings.tracker.project_slug).toBe("my-project");
    expect(settings.tracker.api_key).toBe("lin_api_test");
    expect(settings.tracker.active_states).toEqual(["In Progress"]);
  });

  test("resolves $VAR_NAME in api_key", () => {
    process.env["TEST_LINEAR_KEY"] = "lin_test_123";
    const def = {
      config: { tracker: { kind: "linear", api_key: "$TEST_LINEAR_KEY" } },
      prompt_template: "",
    };
    const settings = settingsFromWorkflow(def);
    expect(settings.tracker.api_key).toBe("lin_test_123");
    delete process.env["TEST_LINEAR_KEY"];
  });

  test("falls back to LINEAR_API_KEY env for linear tracker", () => {
    process.env["LINEAR_API_KEY"] = "lin_env_key";
    const def = { config: { tracker: { kind: "linear" } }, prompt_template: "" };
    const settings = settingsFromWorkflow(def);
    expect(settings.tracker.api_key).toBe("lin_env_key");
    delete process.env["LINEAR_API_KEY"];
  });

  test("applies CLI port override over workflow server.port", () => {
    const def = { config: { server: { port: 4000 } }, prompt_template: "" };
    const settings = settingsFromWorkflow(def, { port: 9000 });
    expect(settings.server.port).toBe(9000);
  });

  test("max_concurrent_agents_by_state normalizes state keys", () => {
    const def = {
      config: { agent: { max_concurrent_agents_by_state: { "In Progress": 3, "todo": 1 } } },
      prompt_template: "",
    };
    const settings = settingsFromWorkflow(def);
    expect(settings.agent.max_concurrent_agents_by_state.get("in progress")).toBe(3);
    expect(settings.agent.max_concurrent_agents_by_state.get("todo")).toBe(1);
  });

  test("ignores non-positive max_concurrent_agents_by_state values", () => {
    const def = {
      config: { agent: { max_concurrent_agents_by_state: { bad: -1, zero: 0 } } },
      prompt_template: "",
    };
    const settings = settingsFromWorkflow(def);
    expect(settings.agent.max_concurrent_agents_by_state.size).toBe(0);
  });
});

// §6.3 Validation
describe("validateForDispatch", () => {
  test("returns null for valid linear config", () => {
    const def = {
      config: {
        tracker: { kind: "linear", api_key: "test_key", project_slug: "proj" },
      },
      prompt_template: "",
    };
    const settings = settingsFromWorkflow(def);
    expect(validateForDispatch(settings)).toBeNull();
  });

  test("rejects missing tracker.kind", () => {
    const def = { config: {}, prompt_template: "" };
    const settings = settingsFromWorkflow(def);
    expect(validateForDispatch(settings)).toContain("tracker.kind");
  });

  test("rejects unsupported tracker.kind", () => {
    const def = { config: { tracker: { kind: "github" } }, prompt_template: "" };
    const settings = settingsFromWorkflow(def);
    expect(validateForDispatch(settings)).toContain("not supported");
  });

  test("rejects missing api_key", () => {
    const def = {
      config: { tracker: { kind: "linear", project_slug: "proj" } },
      prompt_template: "",
    };
    const settings = settingsFromWorkflow(def);
    expect(validateForDispatch(settings)).toContain("api_key");
  });
});

// §4.2 Workspace key sanitization
describe("sanitizeWorkspaceKey", () => {
  test("allows valid chars", () => {
    expect(sanitizeWorkspaceKey("ABC-123")).toBe("ABC-123");
    expect(sanitizeWorkspaceKey("feat.my-branch_1")).toBe("feat.my-branch_1");
  });

  test("replaces invalid chars with underscore", () => {
    expect(sanitizeWorkspaceKey("ABC/123")).toBe("ABC_123");
    expect(sanitizeWorkspaceKey("issue #42")).toBe("issue__42");
    expect(sanitizeWorkspaceKey("a@b!c")).toBe("a_b_c");
  });
});

// §12 Prompt template rendering
describe("buildPrompt", () => {
  const mockIssue: Issue = {
    id: "issue-1",
    identifier: "MT-123",
    title: "Fix the bug",
    description: "The bug is bad",
    priority: 1,
    state: "In Progress",
    branch_name: null,
    url: "https://linear.app/mt/issue/MT-123",
    labels: ["bug", "urgent"],
    blocked_by: [],
    created_at: new Date("2024-01-01"),
    updated_at: new Date("2024-01-02"),
  };

  test("renders basic template variables", async () => {
    const result = await buildPrompt(
      "Work on {{issue.identifier}}: {{issue.title}}",
      mockIssue,
      null
    );
    expect(result).toBe("Work on MT-123: Fix the bug");
  });

  test("renders attempt variable", async () => {
    const result = await buildPrompt(
      "{% if attempt %}Retry attempt {{attempt}}{% else %}First run{% endif %}",
      mockIssue,
      null
    );
    expect(result).toContain("First run");
  });

  test("renders attempt = 2 for retry", async () => {
    const result = await buildPrompt(
      "{% if attempt %}Retry attempt {{attempt}}{% else %}First run{% endif %}",
      mockIssue,
      2
    );
    expect(result).toContain("Retry attempt 2");
  });

  test("falls back to default prompt on empty template", async () => {
    const result = await buildPrompt("", mockIssue, null);
    expect(result).toBe("You are working on an issue from Linear.");
  });

  test("throws template_render_error on unknown variable", async () => {
    await expect(buildPrompt("{{issue.nonexistent}}", mockIssue, null)).rejects.toMatchObject({
      code: "template_render_error",
    });
  });

  test("can iterate labels", async () => {
    const result = await buildPrompt(
      "Labels: {% for label in issue.labels %}{{label}} {% endfor %}",
      mockIssue,
      null
    );
    expect(result).toContain("bug");
    expect(result).toContain("urgent");
  });
});
