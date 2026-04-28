import { randomUUID } from "node:crypto";
import http from "node:http";
import { setTimeout as delay } from "node:timers/promises";
import {
  encodeAddAgentOutput,
  encodeAppendAgentOutput,
  decodeWarpRequest,
  encodeBase64Url,
  encodeStreamFinishedDone,
  encodeStreamFinishedInternalError,
  encodeStreamInit,
} from "./protobuf.js";

const port = Number.parseInt(process.env.PORT ?? "8787", 10);
const defaultBaseUrl = "https://api.openai.com/v1";
const defaultModel = "Qwen/Qwen3.6-27B-FP8";
const defaultModelAliases: Record<string, string> = {
  auto: defaultModel,
  "auto-efficient": defaultModel,
  "auto-coding": defaultModel,
  "auto-reasoning": defaultModel,
};
const maxRequestBytes = 25 * 1024 * 1024;
const openAiBaseUrlHeader = "x-warp-openai-base-url";

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

function configuredModelAliases(): Record<string, string> {
  const rawAliases = nonEmpty(process.env.LOCAL_MODEL_ALIASES);
  if (!rawAliases) {
    return {};
  }

  const aliases = JSON.parse(rawAliases) as unknown;
  if (!aliases || typeof aliases !== "object" || Array.isArray(aliases)) {
    throw new Error("LOCAL_MODEL_ALIASES must be a JSON object mapping Warp model IDs to provider model IDs.");
  }

  return Object.fromEntries(
    Object.entries(aliases)
      .map(([key, value]) => [key, typeof value === "string" ? nonEmpty(value) : undefined] as const)
      .filter((entry): entry is [string, string] => entry[1] != null),
  );
}

function resolveProviderModel(warpModel: string | undefined): string {
  const explicitModel = nonEmpty(process.env.OPENAI_MODEL);
  if (explicitModel) {
    return explicitModel;
  }

  const requestedModel = nonEmpty(warpModel);
  if (!requestedModel) {
    return defaultModel;
  }

  const modelAliases = {
    ...defaultModelAliases,
    ...configuredModelAliases(),
  };

  if (modelAliases[requestedModel]) {
    return modelAliases[requestedModel];
  }

  return requestedModel.startsWith("auto") ? defaultModel : requestedModel;
}

function sendJson(response: http.ServerResponse, status: number, payload: unknown): void {
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
  });
  response.end(JSON.stringify(payload));
}

async function readBody(request: http.IncomingMessage): Promise<Uint8Array> {
  const chunks: Buffer[] = [];
  let bytesRead = 0;

  for await (const chunk of request) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    bytesRead += buffer.length;
    if (bytesRead > maxRequestBytes) {
      throw new Error("Request body exceeds the 25 MiB local service limit.");
    }
    chunks.push(buffer);
  }

  return Buffer.concat(chunks);
}

function writeSse(response: http.ServerResponse, bytes: Uint8Array): void {
  response.write(`data: ${encodeBase64Url(bytes)}\n\n`);
}

function extractStreamingContent(payload: unknown): string {
  if (!payload || typeof payload !== "object") {
    return "";
  }

  const choices = (payload as { choices?: unknown }).choices;
  if (!Array.isArray(choices)) {
    return "";
  }

  return choices
    .map((choice) => {
      if (!choice || typeof choice !== "object") {
        return "";
      }

      const delta = (choice as { delta?: { content?: unknown } }).delta;
      return typeof delta?.content === "string" ? delta.content : "";
    })
    .join("");
}

async function* streamChatCompletion(params: {
  prompt: string;
  apiKey?: string;
  baseUrl?: string;
  model?: string;
}): AsyncGenerator<string> {
  const apiKey = params.apiKey ?? process.env.OPENAI_API_KEY;
  if (!apiKey) {
    throw new Error("OPENAI_API_KEY is not set and the Warp request did not include an OpenAI key.");
  }

  const baseUrl = trimTrailingSlash(nonEmpty(params.baseUrl) ?? nonEmpty(process.env.OPENAI_BASE_URL) ?? defaultBaseUrl);
  const model = resolveProviderModel(params.model);

  const messages = [
    process.env.LOCAL_MULTI_AGENT_SYSTEM_PROMPT
      ? { role: "system", content: process.env.LOCAL_MULTI_AGENT_SYSTEM_PROMPT }
      : undefined,
    { role: "user", content: params.prompt || "Continue." },
  ].filter((message) => message != null);

  const completionResponse = await fetch(`${baseUrl}/chat/completions`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({
      model,
      messages,
      temperature: 0.2,
      stream: true,
    }),
  });

  if (!completionResponse.ok) {
    const body = await completionResponse.text();
    throw new Error(`OpenAI-compatible endpoint returned ${completionResponse.status}: ${body}`);
  }

  if (!completionResponse.body) {
    throw new Error("OpenAI-compatible endpoint returned no response stream.");
  }

  const reader = completionResponse.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  while (true) {
    const { done, value } = await reader.read();
    if (done) {
      break;
    }

    buffer += decoder.decode(value, { stream: true });
    const lines = buffer.split(/\r?\n/);
    buffer = lines.pop() ?? "";

    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed.startsWith("data:")) {
        continue;
      }

      const data = trimmed.slice("data:".length).trim();
      if (data === "[DONE]") {
        return;
      }

      const content = extractStreamingContent(JSON.parse(data));
      if (content) {
        yield content;
      }
    }
  }
}

async function handleMultiAgent(
  request: http.IncomingMessage,
  response: http.ServerResponse,
  passiveSuggestions: boolean,
): Promise<void> {
  const body = await readBody(request);
  const warpRequest = decodeWarpRequest(body);
  const openAiBaseUrl = request.headers[openAiBaseUrlHeader];
  const requestOpenAiBaseUrl = Array.isArray(openAiBaseUrl) ? openAiBaseUrl[0] : openAiBaseUrl;

  response.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-cache, no-transform",
    connection: "keep-alive",
    "x-accel-buffering": "no",
  });

  writeSse(response, encodeStreamInit(warpRequest.conversationId, warpRequest.requestId));

  try {
    if (!passiveSuggestions) {
      const messageId = randomUUID();

      writeSse(
        response,
        encodeAddAgentOutput({
          messageId,
          taskId: warpRequest.rootTaskId,
          requestId: warpRequest.requestId,
          text: "",
        }),
      );

      for await (const chunk of streamChatCompletion({
        prompt: warpRequest.prompt,
        apiKey: warpRequest.openAiApiKey,
        baseUrl: requestOpenAiBaseUrl,
        model: warpRequest.model,
      })) {
        writeSse(
          response,
          encodeAppendAgentOutput({
            messageId,
            taskId: warpRequest.rootTaskId,
            requestId: warpRequest.requestId,
            text: chunk,
          }),
        );
      }
    }

    writeSse(response, encodeStreamFinishedDone());
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    writeSse(response, encodeStreamFinishedInternalError(message));
  } finally {
    await delay(0);
    response.end();
  }
}

const server = http.createServer((request, response) => {
  void (async () => {
    const method = request.method ?? "GET";
    const url = new URL(request.url ?? "/", `http://${request.headers.host ?? "127.0.0.1"}`);

    if (method === "GET" && url.pathname === "/health") {
      sendJson(response, 200, { ok: true });
      return;
    }

    if (method === "POST" && url.pathname === "/ai/multi-agent") {
      await handleMultiAgent(request, response, false);
      return;
    }

    if (method === "POST" && url.pathname === "/ai/passive-suggestions") {
      await handleMultiAgent(request, response, true);
      return;
    }

    sendJson(response, 404, { error: "not_found" });
  })().catch((error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    if (!response.headersSent) {
      sendJson(response, 500, { error: message });
    } else {
      response.end();
    }
  });
});

server.listen(port, "127.0.0.1", () => {
  console.log(`Local multi-agent API listening on http://127.0.0.1:${port}`);
});
