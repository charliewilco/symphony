// Prompt template rendering — §12 of the spec
import { Liquid } from "liquidjs";
import type { Issue } from "./types.ts";

const engine = new Liquid({
  strictVariables: true,
  strictFilters: true,
});

export type PromptError =
  | { code: "template_parse_error"; message: string }
  | { code: "template_render_error"; message: string };

const FALLBACK_PROMPT = "You are working on an issue from Linear.";

/** Build the prompt for an issue run attempt. */
export async function buildPrompt(
  templateStr: string,
  issue: Issue,
  attempt: number | null
): Promise<string> {
  const body = templateStr.trim();
  if (!body) {
    return FALLBACK_PROMPT;
  }

  // Convert issue to template-friendly object (all keys as strings)
  const issueCtx: Record<string, unknown> = {
    id: issue.id,
    identifier: issue.identifier,
    title: issue.title,
    description: issue.description ?? "",
    priority: issue.priority,
    state: issue.state,
    branch_name: issue.branch_name ?? "",
    url: issue.url ?? "",
    labels: issue.labels,
    blocked_by: issue.blocked_by.map((b) => ({
      id: b.id ?? "",
      identifier: b.identifier ?? "",
      state: b.state ?? "",
    })),
    created_at: issue.created_at?.toISOString() ?? "",
    updated_at: issue.updated_at?.toISOString() ?? "",
  };

  const context: Record<string, unknown> = {
    issue: issueCtx,
    attempt,
  };

  let template;
  try {
    template = engine.parse(body);
  } catch (err) {
    throw { code: "template_parse_error", message: String(err) } as PromptError;
  }

  try {
    const rendered = await engine.render(template, context);
    return rendered.trim() || FALLBACK_PROMPT;
  } catch (err) {
    throw { code: "template_render_error", message: String(err) } as PromptError;
  }
}

/** Build the continuation guidance message for subsequent turns. */
export function buildContinuationPrompt(issue: Issue, turnCount: number): string {
  return [
    `Continue working on issue ${issue.identifier}: ${issue.title}.`,
    `This is continuation turn ${turnCount}.`,
    "Review your prior progress and continue from where you left off.",
  ].join(" ");
}

export function isPromptError(err: unknown): err is PromptError {
  return (
    typeof err === "object" &&
    err !== null &&
    "code" in err &&
    (err as PromptError).code === "template_parse_error" ||
    (err as PromptError).code === "template_render_error"
  );
}
