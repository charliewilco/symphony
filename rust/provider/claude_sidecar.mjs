#!/usr/bin/env node

import readline from "node:readline";

async function loadSdk() {
  try {
    return await import("@anthropic-ai/claude-agent-sdk");
  } catch (error) {
    throw new Error(
      `failed_to_load_claude_agent_sdk: ${error instanceof Error ? error.message : String(error)}`
    );
  }
}

function emit(payload) {
  process.stdout.write(`${JSON.stringify(payload)}\n`);
}

async function handleTurn(message) {
  const sdk = await loadSdk();
  const params = message.params ?? {};
  const query = sdk.query ?? sdk.default?.query;
  if (typeof query !== "function") {
    throw new Error("claude_agent_sdk_missing_query");
  }

  let sessionId = params.session_id ?? null;
  let resultSubtype = "success";
  let resultText = "";
  let usage = null;

  const options = {
    cwd: params.cwd,
    allowedTools: params.allowed_tools,
    permissionMode: params.permission_mode,
    settingSources: params.setting_sources,
    resume: sessionId ?? undefined,
  };

  for await (const item of query({
    prompt: params.prompt,
    options,
  })) {
    const subtype = item?.subtype ?? null;
    if (item?.session_id) {
      sessionId = item.session_id;
    }
    if (subtype === "result" || item?.result || item?.final) {
      resultSubtype = item?.subtype ?? "success";
      resultText = item?.result ?? item?.final ?? "";
      usage = item?.usage ?? null;
    } else {
      emit({
        method: "agent/update",
        params: {
          event: subtype ?? "notification",
          payload: item,
        },
      });
    }
  }

  emit({
    id: message.id,
    result: {
      session: sessionId ? { id: sessionId } : null,
      turn: { id: `turn-${Date.now()}` },
      subtype: resultSubtype,
      result: resultText,
      usage,
    },
  });
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
    const message = JSON.parse(line);
    if (message.method !== "turn/start") {
      emit({
        id: message.id,
        result: {
          ignored: true,
        },
      });
      continue;
    }
    try {
      await handleTurn(message);
    } catch (error) {
      emit({
        id: message.id,
        error: {
          message: error instanceof Error ? error.message : String(error),
        },
      });
    }
  }
}

main().catch((error) => {
  process.stderr.write(
    `${error instanceof Error ? error.stack ?? error.message : String(error)}\n`
  );
  process.exit(1);
});
