// Workspace manager — §9 of the spec
import * as fs from "node:fs/promises";
import * as path from "node:path";
import { spawn } from "node:child_process";
import type { Workspace } from "./types.ts";
import type { HookSettings, WorkspaceSettings } from "./config.ts";
import { logger } from "./logger.ts";

// §4.2 Workspace key sanitization
export function sanitizeWorkspaceKey(identifier: string): string {
  return identifier.replace(/[^A-Za-z0-9._-]/g, "_");
}

// §9.5 Invariant 2: workspace path must be inside workspace root
export function assertInsideRoot(workspaceRoot: string, workspacePath: string): void {
  const absRoot = path.resolve(workspaceRoot);
  const absPath = path.resolve(workspacePath);
  // Must be a strict subdirectory
  if (!absPath.startsWith(absRoot + path.sep) && absPath !== absRoot) {
    throw new Error(
      `Safety violation: workspace path "${absPath}" is outside workspace root "${absRoot}"`
    );
  }
}

/** Run a shell hook script in the workspace directory. */
async function runHook(
  name: string,
  script: string,
  cwd: string,
  timeoutMs: number
): Promise<void> {
  logger.info(`hook start hook=${name} cwd=${cwd}`);

  await new Promise<void>((resolve, reject) => {
    const child = spawn("bash", ["-lc", script], {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";
    child.stdout?.on("data", (d: Buffer) => { stdout += d.toString(); });
    child.stderr?.on("data", (d: Buffer) => { stderr += d.toString(); });

    const timer = setTimeout(() => {
      child.kill("SIGTERM");
      reject(new Error(`Hook "${name}" timed out after ${timeoutMs}ms`));
    }, timeoutMs);

    child.on("close", (code: number | null) => {
      clearTimeout(timer);
      if (code === 0) {
        resolve();
      } else {
        reject(
          new Error(`Hook "${name}" exited with code ${code}. stderr: ${stderr.trim()}`)
        );
      }
    });

    child.on("error", (err: Error) => {
      clearTimeout(timer);
      reject(new Error(`Hook "${name}" spawn error: ${err.message}`));
    });
  });
}

/** §9.2 Create or reuse a workspace for an issue. */
export async function ensureWorkspace(
  identifier: string,
  wsSettings: WorkspaceSettings,
  hookSettings: HookSettings
): Promise<Workspace> {
  const workspace_key = sanitizeWorkspaceKey(identifier);
  const workspacePath = path.join(wsSettings.root, workspace_key);

  // §9.5 Invariant 2
  assertInsideRoot(wsSettings.root, workspacePath);

  // Ensure workspace root exists
  await fs.mkdir(wsSettings.root, { recursive: true });

  let created_now = false;
  try {
    await fs.access(workspacePath);
    // Directory already exists
  } catch {
    // Create the directory
    await fs.mkdir(workspacePath, { recursive: true });
    created_now = true;
  }

  if (created_now && hookSettings.after_create) {
    try {
      await runHook("after_create", hookSettings.after_create, workspacePath, hookSettings.timeout_ms);
    } catch (err) {
      // §9.4: after_create failure is fatal — clean up
      logger.error(`hook failed hook=after_create workspace=${workspacePath} error=${err}`);
      try {
        await fs.rm(workspacePath, { recursive: true, force: true });
      } catch {}
      throw err;
    }
  }

  return { path: workspacePath, workspace_key, created_now };
}

/** Run the before_run hook. §9.4 — failure is fatal to the attempt. */
export async function runBeforeRun(
  workspacePath: string,
  hookSettings: HookSettings
): Promise<void> {
  if (!hookSettings.before_run) return;
  await runHook("before_run", hookSettings.before_run, workspacePath, hookSettings.timeout_ms);
}

/** Run the after_run hook. §9.4 — failure is logged and ignored. */
export async function runAfterRun(
  workspacePath: string,
  hookSettings: HookSettings
): Promise<void> {
  if (!hookSettings.after_run) return;
  try {
    await runHook("after_run", hookSettings.after_run, workspacePath, hookSettings.timeout_ms);
  } catch (err) {
    logger.warn(`hook failed hook=after_run workspace=${workspacePath} error=${err}`);
  }
}

/** Remove a workspace directory. §9.4 before_remove is logged+ignored on failure. */
export async function removeWorkspace(
  identifier: string,
  wsSettings: WorkspaceSettings,
  hookSettings: HookSettings
): Promise<void> {
  const workspace_key = sanitizeWorkspaceKey(identifier);
  const workspacePath = path.join(wsSettings.root, workspace_key);

  let exists = false;
  try {
    await fs.access(workspacePath);
    exists = true;
  } catch {}

  if (!exists) return;

  if (hookSettings.before_remove) {
    try {
      await runHook("before_remove", hookSettings.before_remove, workspacePath, hookSettings.timeout_ms);
    } catch (err) {
      logger.warn(`hook failed hook=before_remove workspace=${workspacePath} error=${err}`);
      // continue cleanup
    }
  }

  try {
    await fs.rm(workspacePath, { recursive: true, force: true });
    logger.info(`workspace removed workspace=${workspacePath}`);
  } catch (err) {
    logger.warn(`workspace removal failed workspace=${workspacePath} error=${err}`);
  }
}
