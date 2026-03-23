// Workflow file loader — §5 of the spec
import * as fs from "node:fs/promises";
import * as path from "node:path";
import yaml from "js-yaml";

export interface WorkflowDefinition {
  config: Record<string, unknown>;
  prompt_template: string;
}

export type WorkflowError =
  | { code: "missing_workflow_file"; message: string }
  | { code: "workflow_parse_error"; message: string }
  | { code: "workflow_front_matter_not_a_map"; message: string };

export function workflowFilePath(override?: string): string {
  if (override) return path.resolve(override);
  return path.resolve(process.cwd(), "WORKFLOW.md");
}

export async function loadWorkflow(
  filePath: string
): Promise<WorkflowDefinition> {
  let raw: string;
  try {
    raw = await fs.readFile(filePath, "utf-8");
  } catch (err) {
    throw { code: "missing_workflow_file", message: `Cannot read workflow file: ${filePath}` } as WorkflowError;
  }

  return parseWorkflow(raw);
}

export function parseWorkflow(raw: string): WorkflowDefinition {
  // §5.2: parse YAML front matter if file starts with "---"
  if (raw.startsWith("---")) {
    const end = raw.indexOf("\n---", 3);
    if (end === -1) {
      // No closing ---; treat as prompt-only
      return { config: {}, prompt_template: raw.trim() };
    }

    const frontMatterStr = raw.slice(3, end).trim();
    const bodyStr = raw.slice(end + 4).trim();

    let parsed: unknown;
    try {
      parsed = yaml.load(frontMatterStr);
    } catch (err) {
      throw {
        code: "workflow_parse_error",
        message: `YAML parse error in front matter: ${err}`,
      } as WorkflowError;
    }

    // null/undefined means empty front matter — treat as empty config
    if (parsed === null || parsed === undefined) {
      return { config: {}, prompt_template: bodyStr };
    }

    if (typeof parsed !== "object" || Array.isArray(parsed)) {
      throw {
        code: "workflow_front_matter_not_a_map",
        message: "YAML front matter must be an object/map",
      } as WorkflowError;
    }

    const config = parsed as Record<string, unknown>;
    return { config, prompt_template: bodyStr };
  }

  // No front matter — entire content is prompt
  return { config: {}, prompt_template: raw.trim() };
}

export function isWorkflowError(err: unknown): err is WorkflowError {
  return (
    typeof err === "object" &&
    err !== null &&
    "code" in err &&
    typeof (err as WorkflowError).code === "string"
  );
}
