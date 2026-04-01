import readline from "node:readline";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

type SidecarMessage = {
  id?: number;
  method?: string;
  params?: Record<string, Json>;
};

async function loadSdk() {
  return import("@anthropic-ai/claude-agent-sdk");
}

function emit(payload: Record<string, Json>) {
  process.stdout.write(`${JSON.stringify(payload)}\n`);
}

async function handleTurn(message: SidecarMessage) {
  const sdk = await loadSdk();
  const query = (sdk as any).query ?? (sdk as any).default?.query;
  if (typeof query !== "function") {
    throw new Error("claude_agent_sdk_missing_query");
  }

  const params = message.params ?? {};
  let sessionId = (params.session_id as string | null | undefined) ?? null;
  let usage: Json = null;

  for await (const item of query({
    prompt: params.prompt,
    options: {
      cwd: params.cwd,
      allowedTools: params.allowed_tools,
      permissionMode: params.permission_mode,
      settingSources: params.setting_sources,
      resume: sessionId ?? undefined,
    },
  })) {
    if (item?.session_id) {
      sessionId = item.session_id;
    }
    if (item?.subtype === "result" || item?.result || item?.final) {
      usage = item?.usage ?? null;
      emit({
        id: message.id ?? 0,
        result: {
          session: sessionId ? { id: sessionId } : null,
          turn: { id: `turn-${Date.now()}` },
          usage,
        },
      });
      return;
    }

    emit({
      method: "agent/update",
      params: {
        event: item?.subtype ?? "notification",
        payload: item,
      },
    });
  }
}

async function main() {
  const rl = readline.createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
  });

  for await (const line of rl) {
    if (!line.trim()) {
      continue;
    }
    const message = JSON.parse(line) as SidecarMessage;
    if (message.method !== "turn/start") {
      emit({
        id: message.id ?? 0,
        result: { ignored: true },
      });
      continue;
    }
    await handleTurn(message);
  }
}

main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
  process.exit(1);
});
