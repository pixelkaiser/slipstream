import { randomUUID } from "node:crypto";
import http from "node:http";
import { setTimeout as delay } from "node:timers/promises";
import {
  encodeAddAgentOutput,
  encodeCreateTask,
  decodeWarpRequest,
  encodeStreamFinishedDone,
  encodeStreamFinishedInternalError,
  encodeStreamInit,
} from "./protobuf.js";
import { resolveProviderModel } from "./model.js";
import { collectAssistantOutput } from "./response.js";
import { log } from "./logger.js";
import { formatSseDataEvent } from "./sse.js";

const port = Number.parseInt(process.env.PORT ?? "8787", 10);
const defaultBaseUrl = "https://api.openai.com/v1";
const maxRequestBytes = 25 * 1024 * 1024;
const openAiBaseUrlHeader = "x-warp-openai-base-url";

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
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
  response.write(formatSseDataEvent(bytes));
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
  const model = resolveProviderModel({
    openAiModel: process.env.OPENAI_MODEL,
    warpModel: params.model,
    localModelAliases: process.env.LOCAL_MODEL_ALIASES,
  });
  log("info", "provider_request", {
    baseUrl,
    model,
    warpModel: params.model ?? null,
    promptChars: params.prompt.length,
    baseUrlSource: nonEmpty(params.baseUrl) ? "request_header" : nonEmpty(process.env.OPENAI_BASE_URL) ? "env" : "default",
  });

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
    log("warn", "provider_error", {
      status: completionResponse.status,
      bodyChars: body.length,
      model,
      baseUrl,
    });
    throw new Error(`OpenAI-compatible endpoint returned ${completionResponse.status}: ${body}`);
  }

  if (!completionResponse.body) {
    throw new Error("OpenAI-compatible endpoint returned no response stream.");
  }
  log("debug", "provider_stream_opened", {
    status: completionResponse.status,
    model,
    baseUrl,
  });

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
  const startedAt = Date.now();
  let eventsSent = 0;
  const body = await readBody(request);
  const warpRequest = decodeWarpRequest(body);
  const openAiBaseUrl = request.headers[openAiBaseUrlHeader];
  const requestOpenAiBaseUrl = Array.isArray(openAiBaseUrl) ? openAiBaseUrl[0] : openAiBaseUrl;
  log("info", passiveSuggestions ? "passive_suggestions_request" : "multi_agent_request", {
    conversationId: warpRequest.conversationId,
    requestId: warpRequest.requestId,
    taskId: warpRequest.rootTaskId,
    shouldCreateRootTask: warpRequest.shouldCreateRootTask,
    promptChars: warpRequest.prompt.length,
    warpModel: warpRequest.model ?? null,
    hasRequestApiKey: warpRequest.openAiApiKey != null,
    hasOpenAiBaseUrlHeader: requestOpenAiBaseUrl != null,
  });

  function sendEvent(event: string, bytes: Uint8Array): void {
    writeSse(response, bytes);
    eventsSent += 1;
    log("debug", "sse_event", {
      requestId: warpRequest.requestId,
      event,
      bytes: bytes.length,
    });
  }

  response.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-cache, no-transform",
    connection: "keep-alive",
    "x-accel-buffering": "no",
  });

  sendEvent("stream_init", encodeStreamInit(warpRequest.conversationId, warpRequest.requestId));

  try {
    if (!passiveSuggestions) {
      if (warpRequest.shouldCreateRootTask) {
        sendEvent(
          "create_task",
          encodeCreateTask({
            taskId: warpRequest.rootTaskId,
            description: warpRequest.prompt,
          }),
        );
      }

      const messageId = randomUUID();
      const output = await collectAssistantOutput(
        streamChatCompletion({
          prompt: warpRequest.prompt,
          apiKey: warpRequest.openAiApiKey,
          baseUrl: requestOpenAiBaseUrl,
          model: warpRequest.model,
        }),
      );

      sendEvent(
        "add_agent_output",
        encodeAddAgentOutput({
          messageId,
          taskId: warpRequest.rootTaskId,
          requestId: warpRequest.requestId,
          text: output,
        }),
      );
    }

    sendEvent("stream_finished_done", encodeStreamFinishedDone());
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    log("error", "multi_agent_error", {
      requestId: warpRequest.requestId,
      message,
    });
    sendEvent("stream_finished_internal_error", encodeStreamFinishedInternalError(message));
  } finally {
    await delay(0);
    response.end();
    log("info", "multi_agent_completed", {
      requestId: warpRequest.requestId,
      durationMs: Date.now() - startedAt,
      eventsSent,
    });
  }
}

const server = http.createServer((request, response) => {
  void (async () => {
    const startedAt = Date.now();
    const method = request.method ?? "GET";
    const url = new URL(request.url ?? "/", `http://${request.headers.host ?? "127.0.0.1"}`);
    log("info", "http_request", {
      method,
      path: url.pathname,
      remoteAddress: request.socket.remoteAddress,
    });

    if (method === "GET" && url.pathname === "/health") {
      sendJson(response, 200, { ok: true });
      log("info", "http_response", {
        method,
        path: url.pathname,
        statusCode: response.statusCode,
        durationMs: Date.now() - startedAt,
      });
      return;
    }

    if (method === "POST" && url.pathname === "/ai/multi-agent") {
      await handleMultiAgent(request, response, false);
      log("info", "http_response", {
        method,
        path: url.pathname,
        statusCode: response.statusCode,
        durationMs: Date.now() - startedAt,
      });
      return;
    }

    if (method === "POST" && url.pathname === "/ai/passive-suggestions") {
      await handleMultiAgent(request, response, true);
      log("info", "http_response", {
        method,
        path: url.pathname,
        statusCode: response.statusCode,
        durationMs: Date.now() - startedAt,
      });
      return;
    }

    sendJson(response, 404, { error: "not_found" });
    log("warn", "http_response", {
      method,
      path: url.pathname,
      statusCode: response.statusCode,
      durationMs: Date.now() - startedAt,
    });
  })().catch((error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    log("error", "http_request_error", { message });
    if (!response.headersSent) {
      sendJson(response, 500, { error: message });
    } else {
      response.end();
    }
  });
});

server.listen(port, "127.0.0.1", () => {
  log("info", "server_started", {
    url: `http://127.0.0.1:${port}`,
    logLevel: process.env.LOG_LEVEL?.trim() || "info",
    hasOpenAiBaseUrlEnv: nonEmpty(process.env.OPENAI_BASE_URL) != null,
    hasOpenAiModelEnv: nonEmpty(process.env.OPENAI_MODEL) != null,
    hasModelAliasesEnv: nonEmpty(process.env.LOCAL_MODEL_ALIASES) != null,
  });
});
