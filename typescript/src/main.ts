// Entry point — CLI arg parsing, startup, signal handling
import * as path from "node:path";
import { workflowFilePath } from "./workflow.ts";
import { WorkflowStore } from "./workflow_store.ts";
import { settingsFromWorkflow, validateForDispatch } from "./config.ts";
import { Orchestrator } from "./orchestrator.ts";
import { startHttpServer } from "./http.ts";
import { startDashboard } from "./dashboard.ts";
import { logger } from "./logger.ts";
import type { Snapshot } from "./types.ts";

const ACKNOWLEDGEMENT_FLAG = "--i-understand-that-this-will-be-running-without-the-usual-guardrails";
const YOLO_FLAG = "--yolo";

interface Args {
  acknowledgeGuardrails: boolean;
  workflowPath?: string;
  port?: number;
  logsRoot?: string;
}

function parseArgs(argv: string[]): Args {
  const args: Args = { acknowledgeGuardrails: false };
  const positionals: string[] = [];

  let i = 0;
  while (i < argv.length) {
    const arg = argv[i];
    if (arg === ACKNOWLEDGEMENT_FLAG || arg === YOLO_FLAG) {
      args.acknowledgeGuardrails = true;
    } else if (arg === "--port" || arg === "-p") {
      args.port = parseInt(argv[++i], 10);
    } else if (arg.startsWith("--port=")) {
      args.port = parseInt(arg.slice("--port=".length), 10);
    } else if (arg === "--logs-root") {
      args.logsRoot = argv[++i];
    } else if (arg.startsWith("--logs-root=")) {
      args.logsRoot = arg.slice("--logs-root=".length);
    } else if (!arg.startsWith("--")) {
      positionals.push(arg);
    }
    i++;
  }

  if (positionals.length > 0) {
    args.workflowPath = positionals[positionals.length - 1];
  }

  return args;
}

function acknowledgementBanner(): string {
  return [
    "This Symphony implementation is an engineering preview.",
    "Codex will run without any guardrails.",
    "Symphony TypeScript is not a supported product and is presented as-is.",
    `To proceed, start with \`${ACKNOWLEDGEMENT_FLAG}\` (or \`${YOLO_FLAG}\`) CLI argument`,
  ].join("\n");
}

async function main(): Promise<void> {
  // Skip the "bun" and script path from argv
  const argv = process.argv.slice(2);
  const args = parseArgs(argv);

  if (!args.acknowledgeGuardrails) {
    process.stderr.write(acknowledgementBanner() + "\n");
    process.exit(1);
  }

  const workflowPath = workflowFilePath(args.workflowPath);

  // Initialize logging first
  const defaultLogsRoot = args.logsRoot ?? path.join(process.cwd(), "logs");
  logger.initFileLog(defaultLogsRoot);

  logger.info(`symphony starting workflow_path=${workflowPath}`);

  // Load workflow
  let workflowStore: WorkflowStore;
  try {
    workflowStore = await WorkflowStore.create(workflowPath);
  } catch (err) {
    process.stderr.write(`Failed to load workflow: ${err}\n`);
    process.exit(1);
  }

  const overrides = {
    port: args.port,
    logs_root: args.logsRoot,
  };

  // Validate config at startup
  const settings = settingsFromWorkflow(workflowStore.getCurrent(), overrides);
  const validationError = validateForDispatch(settings);
  if (validationError) {
    process.stderr.write(`Startup validation failed: ${validationError}\n`);
    process.exit(1);
  }

  logger.info(
    `config loaded tracker=${settings.tracker.kind} project=${settings.tracker.project_slug} poll_interval_ms=${settings.polling.interval_ms}`
  );

  // Start workflow file watcher
  workflowStore.startWatching();

  // Create orchestrator
  const orchestrator = new Orchestrator(workflowStore, overrides);

  // Start terminal dashboard (if TTY)
  let latestSnapshot: Snapshot | null = null;
  orchestrator.onSnapshot(() => {
    latestSnapshot = orchestrator.snapshot();
  });

  const dashboard = startDashboard(
    () => latestSnapshot,
    () => settingsFromWorkflow(workflowStore.getCurrent(), overrides)
  );

  // Start HTTP server if configured
  const effectiveSettings = settingsFromWorkflow(workflowStore.getCurrent(), overrides);
  if (effectiveSettings.server.port !== null) {
    try {
      const { port } = startHttpServer(orchestrator, effectiveSettings);
      logger.info(`HTTP server started port=${port}`);
    } catch (err) {
      logger.warn(`HTTP server failed to start: ${err}`);
    }
  }

  // Start orchestrator (this begins the poll loop)
  try {
    await orchestrator.start();
  } catch (err) {
    process.stderr.write(`Orchestrator startup failed: ${err}\n`);
    dashboard.stop();
    process.exit(1);
  }

  // Graceful shutdown on SIGINT/SIGTERM
  const shutdown = () => {
    logger.info("symphony shutting down");
    dashboard.stop();
    orchestrator.stop();
    workflowStore.stopWatching();
    process.exit(0);
  };

  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);

  logger.info("symphony running");
}

main().catch((err) => {
  process.stderr.write(`Fatal: ${err}\n`);
  process.exit(1);
});
