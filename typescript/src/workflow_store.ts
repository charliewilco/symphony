// WorkflowStore — hot-reload management for WORKFLOW.md (§6.2)
import * as fs from "node:fs";
import { loadWorkflow, parseWorkflow, type WorkflowDefinition } from "./workflow.ts";
import { logger } from "./logger.ts";

export class WorkflowStore {
  private current: WorkflowDefinition;
  private filePath: string;
  private watcher: fs.FSWatcher | null = null;
  private reloadCallbacks: Array<(def: WorkflowDefinition) => void> = [];
  private reloadDebounce: ReturnType<typeof setTimeout> | null = null;

  constructor(filePath: string, initial: WorkflowDefinition) {
    this.filePath = filePath;
    this.current = initial;
  }

  static async create(filePath: string): Promise<WorkflowStore> {
    const initial = await loadWorkflow(filePath);
    return new WorkflowStore(filePath, initial);
  }

  getCurrent(): WorkflowDefinition {
    return this.current;
  }

  onReload(cb: (def: WorkflowDefinition) => void): void {
    this.reloadCallbacks.push(cb);
  }

  /** Start watching the workflow file for changes. */
  startWatching(): void {
    try {
      this.watcher = fs.watch(this.filePath, () => {
        // Debounce rapid change events
        if (this.reloadDebounce) clearTimeout(this.reloadDebounce);
        this.reloadDebounce = setTimeout(() => this.reload(), 200);
      });
    } catch (err) {
      logger.warn(`Failed to watch workflow file: ${err} path=${this.filePath}`);
    }
  }

  stopWatching(): void {
    if (this.reloadDebounce) clearTimeout(this.reloadDebounce);
    this.watcher?.close();
    this.watcher = null;
  }

  private async reload(): Promise<void> {
    try {
      const raw = await Bun.file(this.filePath).text();
      const newDef = parseWorkflow(raw);
      this.current = newDef;
      logger.info("workflow reloaded path=" + this.filePath);
      for (const cb of this.reloadCallbacks) {
        try { cb(newDef); } catch {}
      }
    } catch (err) {
      // §6.2: invalid reloads must not crash; keep operating with last known good config
      logger.error(`workflow reload failed path=${this.filePath} error=${err}`);
    }
  }
}
