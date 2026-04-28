import { randomUUID } from "node:crypto";
import http from "node:http";
import { setTimeout as delay } from "node:timers/promises";
import {
  encodeAddAgentOutput,
  encodeAddReadFilesToolCall,
  encodeCreateTask,
  decodeWarpRequest,
  encodeStreamFinishedDone,
  encodeStreamFinishedInternalError,
  encodeStreamInit,
  type ReadFilesToolCallFile,
} from "./protobuf.js";
import { resolveProviderModel } from "./model.js";
import { log } from "./logger.js";
import { formatSseDataEvent } from "./sse.js";

const port = Number.parseInt(process.env.PORT ?? "8787", 10);
const defaultBaseUrl = "https://api.openai.com/v1";
const maxRequestBytes = 25 * 1024 * 1024;
const openAiBaseUrlHeader = "x-warp-openai-base-url";
const conversationState = new Map<string, { lastUserPrompt?: string }>();

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

type ProviderToolCall = {
  id: string;
  name: string;
  argumentsText: string;
};

type ProviderResponse = {
  content: string;
  toolCalls: ProviderToolCall[];
};

function readFilesToolSchema(): object {
  return {
    type: "function",
    function: {
      name: "read_files",
      description: "Read one or more text files from the user's current workspace or shell context.",
      parameters: {
        type: "object",
        additionalProperties: false,
        properties: {
          files: {
            type: "array",
            minItems: 1,
            items: {
              type: "object",
              additionalProperties: false,
              properties: {
                name: {
                  type: "string",
                  description: "A relative or absolute file path to read.",
                },
                line_ranges: {
                  type: "array",
                  items: {
                    type: "object",
                    additionalProperties: false,
                    properties: {
                      start: { type: "integer", minimum: 1 },
                      end: { type: "integer", minimum: 1 },
                    },
                    required: ["start", "end"],
                  },
                },
              },
              required: ["name"],
            },
          },
        },
        required: ["files"],
      },
    },
  };
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

function extractStreamingToolCalls(payload: unknown, accumulated: Map<number, ProviderToolCall>): void {
  if (!payload || typeof payload !== "object") {
    return;
  }

  const choices = (payload as { choices?: unknown }).choices;
  if (!Array.isArray(choices)) {
    return;
  }

  for (const choice of choices) {
    if (!choice || typeof choice !== "object") {
      continue;
    }

    const delta = (choice as { delta?: { tool_calls?: unknown } }).delta;
    if (!Array.isArray(delta?.tool_calls)) {
      continue;
    }

    for (const toolCallDelta of delta.tool_calls) {
      if (!toolCallDelta || typeof toolCallDelta !== "object") {
        continue;
      }

      const index = (toolCallDelta as { index?: unknown }).index;
      if (typeof index !== "number") {
        continue;
      }

      const existing = accumulated.get(index) ?? { id: "", name: "", argumentsText: "" };
      const id = (toolCallDelta as { id?: unknown }).id;
      const fn = (toolCallDelta as { function?: { name?: unknown; arguments?: unknown } }).function;
      accumulated.set(index, {
        id: typeof id === "string" ? id : existing.id,
        name: typeof fn?.name === "string" ? existing.name + fn.name : existing.name,
        argumentsText: typeof fn?.arguments === "string" ? existing.argumentsText + fn.arguments : existing.argumentsText,
      });
    }
  }
}

function shouldEnableTools(): boolean {
  return process.env.LOCAL_ENABLE_TOOLS?.trim().toLowerCase() !== "false";
}

function parseReadFilesToolCall(toolCall: ProviderToolCall): ReadFilesToolCallFile[] | undefined {
  if (toolCall.name !== "read_files") {
    return undefined;
  }

  const args = JSON.parse(toolCall.argumentsText || "{}") as { files?: unknown; paths?: unknown };
  const files = Array.isArray(args.files)
    ? args.files
    : Array.isArray(args.paths)
      ? args.paths.map((path) => ({ name: path }))
      : [];

  const parsed = files.flatMap((file): ReadFilesToolCallFile[] => {
    if (typeof file === "string") {
      return [{ name: file }];
    }
    if (!file || typeof file !== "object") {
      return [];
    }

    const name = (file as { name?: unknown; path?: unknown }).name ?? (file as { path?: unknown }).path;
    if (typeof name !== "string" || !name.trim()) {
      return [];
    }

    const lineRanges = (file as { line_ranges?: unknown; lineRanges?: unknown }).line_ranges
      ?? (file as { lineRanges?: unknown }).lineRanges;
    const parsedRanges = Array.isArray(lineRanges)
      ? lineRanges.flatMap((range): Array<{ start: number; end: number }> => {
          if (!range || typeof range !== "object") {
            return [];
          }
          const start = (range as { start?: unknown }).start;
          const end = (range as { end?: unknown }).end;
          return Number.isInteger(start) && Number.isInteger(end)
            ? [{ start: Number(start), end: Number(end) }]
            : [];
        })
      : undefined;

    return [{ name: name.trim(), lineRanges: parsedRanges }];
  });

  return parsed.length ? parsed : undefined;
}

function promptForProvider(warpRequest: ReturnType<typeof decodeWarpRequest>): string {
  const state = conversationState.get(warpRequest.conversationId) ?? {};
  if (warpRequest.toolResults.length > 0 && state.lastUserPrompt) {
    return [
      "Original user request:",
      state.lastUserPrompt,
      "",
      warpRequest.prompt,
    ].join("\n");
  }

  if (warpRequest.prompt && warpRequest.toolResults.length === 0) {
    conversationState.set(warpRequest.conversationId, {
      ...state,
      lastUserPrompt: warpRequest.prompt,
    });
  }

  return warpRequest.prompt;
}

async function streamChatCompletion(params: {
  prompt: string;
  apiKey?: string;
  baseUrl?: string;
  model?: string;
}): Promise<ProviderResponse> {
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

  const requestBody = {
    model,
    messages,
    temperature: 0.2,
    stream: true,
    ...(shouldEnableTools() ? { tools: [readFilesToolSchema()], tool_choice: "auto" } : {}),
  };

  const completionResponse = await fetch(`${baseUrl}/chat/completions`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey}`,
      "content-type": "application/json",
    },
    body: JSON.stringify(requestBody),
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
  let content = "";
  const toolCalls = new Map<number, ProviderToolCall>();

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
        return {
          content,
          toolCalls: [...toolCalls.values()].filter((toolCall) => toolCall.id && toolCall.name),
        };
      }

      const parsed = JSON.parse(data);
      const chunk = extractStreamingContent(parsed);
      if (chunk) {
        content += chunk;
      }
      extractStreamingToolCalls(parsed, toolCalls);
    }
  }

  return {
    content,
    toolCalls: [...toolCalls.values()].filter((toolCall) => toolCall.id && toolCall.name),
  };
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

      const providerPrompt = promptForProvider(warpRequest);
      const providerResponse = await streamChatCompletion({
        prompt: providerPrompt,
        apiKey: warpRequest.openAiApiKey,
        baseUrl: requestOpenAiBaseUrl,
        model: warpRequest.model,
      });

      if (!providerResponse.content && !providerResponse.toolCalls.length) {
        throw new Error("OpenAI-compatible endpoint returned no assistant content or tool calls.");
      }

      if (providerResponse.content) {
        sendEvent(
          "add_agent_output",
          encodeAddAgentOutput({
            messageId: randomUUID(),
            taskId: warpRequest.rootTaskId,
            requestId: warpRequest.requestId,
            text: providerResponse.content,
          }),
        );
      }

      for (const toolCall of providerResponse.toolCalls) {
        const files = parseReadFilesToolCall(toolCall);
        if (!files) {
          throw new Error(`Unsupported provider tool call: ${toolCall.name}`);
        }

        sendEvent(
          "add_read_files_tool_call",
          encodeAddReadFilesToolCall({
            messageId: randomUUID(),
            taskId: warpRequest.rootTaskId,
            requestId: warpRequest.requestId,
            toolCallId: toolCall.id,
            files,
          }),
        );
      }
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
