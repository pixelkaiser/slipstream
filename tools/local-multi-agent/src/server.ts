import { randomUUID } from "node:crypto";
import http from "node:http";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import {
  encodeAddAgentOutput,
  encodeAddToolCall,
  encodeAppendAgentOutput,
  encodeAddConversationSummary,
  encodeCreateTask,
  decodeWarpRequest,
  encodeStreamFinishedDone,
  encodeStreamFinishedContextWindowExceeded,
  encodeStreamFinishedInternalError,
  encodeStreamFinishedInvalidApiKey,
  encodeStreamFinishedLlmUnavailable,
  encodeStreamFinishedQuotaLimit,
  encodeStreamInit,
  type McpToolSummary,
  type ReadFilesToolCallFile,
  type WarpToolCall,
} from "./protobuf.js";
import { resolveProviderModel } from "./model.js";
import { log, logFilePath } from "./logger.js";
import { formatSseDataEvent } from "./sse.js";
import { handleLocalGraphqlRequest } from "./graphql.js";
import { IntegrationStore } from "./integrationStore.js";
import { loadDotEnv } from "./env.js";

loadDotEnv();

const port = Number.parseInt(process.env.PORT ?? "8787", 10);
const host = process.env.HOST?.trim() || "127.0.0.1";
const serviceVersion = "0.1.0";
const defaultBaseUrl = "https://api.openai.com/v1";
const maxRequestBytes = 25 * 1024 * 1024;
const openAiBaseUrlHeader = "x-warp-openai-base-url";
const conversationState = new Map<string, { messages: ProviderChatMessage[] }>();
const localGraphqlDbPath = process.env.LOCAL_GRAPHQL_DB_PATH?.trim()
  || fileURLToPath(new URL("../local-graphql.sqlite", import.meta.url));
const integrationStore = new IntegrationStore(localGraphqlDbPath);
const maxConversationMessages = Math.max(
  4,
  Number.parseInt(process.env.LOCAL_MAX_HISTORY_MESSAGES ?? "80", 10) || 80,
);
const defaultContextWindowTokens = 128 * 1024;
const builtInModelContextWindows = new Map<string, number>([
  ["Qwen/Qwen3.6-27B-FP8", 262144],
]);
const modelContextCacheTtlMs = 30_000;
const modelContextCache = new Map<string, { fetchedAtMs: number; contextWindowsByModel: Map<string, number> }>();

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

function warpUrlScheme(): string {
  const scheme = nonEmpty(process.env.WARP_URL_SCHEME) ?? "warp";
  return /^[a-z][a-z0-9+.-]*$/i.test(scheme) ? scheme : "warp";
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function sharedSessionIntentUrl(url: URL): string | undefined {
  const match =
    /^\/session\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i.exec(
      url.pathname,
    );
  if (!match) {
    return undefined;
  }

  const intentUrl = new URL(`${warpUrlScheme()}://shared_session/${match[1]}`);
  intentUrl.search = url.search;
  return intentUrl.toString();
}

function sendRedirect(response: http.ServerResponse, location: string): void {
  response.writeHead(302, {
    location,
    "content-type": "text/html; charset=utf-8",
  });
  const escapedLocation = escapeHtml(location);
  response.end(
    `<!doctype html><html><head><meta http-equiv="refresh" content="0;url=${escapedLocation}"></head><body><a href="${escapedLocation}">Open shared session in Warp</a></body></html>`,
  );
}

function sendJson(response: http.ServerResponse, status: number, payload: unknown): void {
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
  });
  response.end(JSON.stringify(payload));
}

function objectValue(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function graphqlErrorMessages(payload: unknown): string[] {
  const errors = objectValue(payload).errors;
  if (!Array.isArray(errors)) {
    return [];
  }

  return errors.map((error) => {
    const message = objectValue(error).message;
    return typeof message === "string" ? message : String(error);
  });
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

type ProviderContentPart =
  | { type: "text"; text: string }
  | { type: "image_url"; image_url: { url: string } };

type ProviderChatMessage = {
  role: "system" | "user" | "assistant" | "tool";
  content?: string | ProviderContentPart[];
  tool_call_id?: string;
  tool_calls?: Array<{
    id: string;
    type: "function";
    function: {
      name: string;
      arguments: string;
    };
  }>;
};

type ProviderResponse = {
  content: string;
  toolCalls: ProviderToolCall[];
  contextWindowUsage?: number;
  contextWindowTokens?: number;
};

type FinishReason = "invalid_api_key" | "llm_unavailable" | "context_window_exceeded" | "quota_limit" | "internal_error";

class LocalAgentError extends Error {
  constructor(
    message: string,
    readonly finishReason: FinishReason = "internal_error",
    readonly modelName?: string,
  ) {
    super(message);
    this.name = "LocalAgentError";
  }
}

function providerToolCallMessage(toolCall: ProviderToolCall): NonNullable<ProviderChatMessage["tool_calls"]>[number] {
  return {
    id: toolCall.id,
    type: "function",
    function: {
      name: toolCall.name,
      arguments: toolCall.argumentsText,
    },
  };
}

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

function runShellCommandToolSchema(): object {
  return {
    type: "function",
    function: {
      name: "run_shell_command",
      description: "Run a shell command in the user's current terminal context.",
      parameters: {
        type: "object",
        additionalProperties: false,
        properties: {
          command: { type: "string" },
          is_read_only: { type: "boolean", description: "Whether the command only reads state." },
          is_risky: { type: "boolean", description: "Whether the command may modify files, processes, or external state." },
          uses_pager: { type: "boolean" },
          wait_until_complete: { type: "boolean" },
        },
        required: ["command"],
      },
    },
  };
}

function grepToolSchema(): object {
  return {
    type: "function",
    function: {
      name: "grep",
      description: "Search for text or patterns in files under a path.",
      parameters: {
        type: "object",
        additionalProperties: false,
        properties: {
          queries: { type: "array", items: { type: "string" }, minItems: 1 },
          query: { type: "string" },
          path: { type: "string", description: "File or directory to search. Defaults to the current directory." },
        },
      },
    },
  };
}

function searchCodebaseToolSchema(): object {
  return {
    type: "function",
    function: {
      name: "search_codebase",
      description: "Search indexed codebase context for relevant files and snippets.",
      parameters: {
        type: "object",
        additionalProperties: false,
        properties: {
          query: { type: "string" },
          path_filters: { type: "array", items: { type: "string" } },
          codebase_path: { type: "string" },
        },
        required: ["query"],
      },
    },
  };
}

function fileGlobToolSchema(): object {
  return {
    type: "function",
    function: {
      name: "file_glob",
      description: "Find files whose names match glob patterns.",
      parameters: {
        type: "object",
        additionalProperties: false,
        properties: {
          patterns: { type: "array", items: { type: "string" }, minItems: 1 },
          pattern: { type: "string" },
          search_dir: { type: "string", description: "Directory to search. Defaults to the current directory." },
          max_matches: { type: "integer", minimum: 0 },
          max_depth: { type: "integer", minimum: 0 },
          min_depth: { type: "integer", minimum: 0 },
        },
      },
    },
  };
}

function applyFileDiffsToolSchema(): object {
  return {
    type: "function",
    function: {
      name: "apply_file_diffs",
      description: "Request edits to local files using search/replace diffs, file creation, deletion, or V4A hunks.",
      parameters: {
        type: "object",
        additionalProperties: false,
        properties: {
          summary: { type: "string" },
          diffs: {
            type: "array",
            items: {
              type: "object",
              additionalProperties: false,
              properties: {
                file_path: { type: "string" },
                search: { type: "string" },
                replace: { type: "string" },
              },
              required: ["file_path"],
            },
          },
          new_files: {
            type: "array",
            items: {
              type: "object",
              additionalProperties: false,
              properties: {
                file_path: { type: "string" },
                content: { type: "string" },
              },
              required: ["file_path", "content"],
            },
          },
          deleted_files: {
            type: "array",
            items: {
              type: "object",
              additionalProperties: false,
              properties: {
                file_path: { type: "string" },
              },
              required: ["file_path"],
            },
          },
        },
      },
    },
  };
}

function suggestPlanToolSchema(): object {
  return {
    type: "function",
    function: {
      name: "suggest_plan",
      description: "Suggest a plan for the user to review before continuing.",
      parameters: {
        type: "object",
        additionalProperties: false,
        properties: {
          summary: { type: "string" },
          tasks: {
            type: "array",
            minItems: 1,
            items: {
              type: "object",
              additionalProperties: false,
              properties: {
                description: { type: "string" },
              },
              required: ["description"],
            },
          },
        },
        required: ["summary", "tasks"],
      },
    },
  };
}

function readMcpResourceToolSchema(): object {
  return {
    type: "function",
    function: {
      name: "read_mcp_resource",
      description: "Read one MCP resource by URI from the MCP resources listed in the request context.",
      parameters: {
        type: "object",
        additionalProperties: false,
        properties: {
          uri: { type: "string", description: "The exact MCP resource URI to read." },
          server_id: { type: "string", description: "Optional MCP server id from the request context." },
        },
        required: ["uri"],
      },
    },
  };
}

function callMcpToolSchema(): object {
  return {
    type: "function",
    function: {
      name: "call_mcp_tool",
      description: "Call one MCP tool from the MCP tools listed in the request context.",
      parameters: {
        type: "object",
        additionalProperties: false,
        properties: {
          name: { type: "string", description: "The exact MCP tool name to call." },
          server_id: { type: "string", description: "Optional MCP server id from the request context." },
          args: {
            type: "object",
            description: "JSON object arguments for the MCP tool.",
            additionalProperties: true,
          },
        },
        required: ["name"],
      },
    },
  };
}

function localToolSchemas(): object[] {
  return [
    readFilesToolSchema(),
    fileGlobToolSchema(),
    grepToolSchema(),
    searchCodebaseToolSchema(),
    runShellCommandToolSchema(),
    applyFileDiffsToolSchema(),
    suggestPlanToolSchema(),
    readMcpResourceToolSchema(),
    callMcpToolSchema(),
  ];
}

function localToolUseSystemPrompt(mcpTools: McpToolSummary[]): string {
  const mcpToolNames = [...new Set(mcpTools.map((tool) => tool.name))].sort();
  const mcpToolText = mcpToolNames.length
    ? `\nAvailable MCP tool names from request context include: ${mcpToolNames.join(", ")}.`
    : "";

  return [
    "Use only the OpenAI function tools explicitly provided in this request.",
    "To call any MCP tool, always use the provided call_mcp_tool function.",
    "Do not emit provider tool calls named after MCP tools directly, such as list_issues or search_docs.",
    "For call_mcp_tool, set name to the exact MCP tool name from the request context and pass the MCP tool arguments in args.",
    "If the MCP context includes a server_id for the desired tool, include that server_id. This is required when multiple MCP servers expose the same tool name.",
    mcpToolText,
  ].join("\n");
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

function finitePositiveNumber(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value) && value > 0) {
    return value;
  }
  if (typeof value === "string" && value.trim()) {
    const parsed = Number(value);
    return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
  }
  return undefined;
}

function configuredContextWindowTokens(model: string): number | undefined {
  const raw = nonEmpty(process.env.LOCAL_MODEL_CONTEXT_TOKENS)
    ?? nonEmpty(process.env.LOCAL_CONTEXT_WINDOW_TOKENS);
  if (!raw) {
    return undefined;
  }

  const asNumber = finitePositiveNumber(raw);
  if (asNumber != null) {
    return asNumber;
  }

  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return undefined;
    }
    const values = parsed as Record<string, unknown>;
    return finitePositiveNumber(values[model]) ?? finitePositiveNumber(values.default);
  } catch {
    return undefined;
  }
}

function contextWindowFromProviderModel(model: Record<string, unknown>): number | undefined {
  const directKeys = [
    "context_length",
    "contextLength",
    "max_context_length",
    "maxContextLength",
    "max_model_len",
    "maxModelLen",
    "max_sequence_length",
    "maxSequenceLength",
    "n_ctx",
    "nCtx",
  ];
  for (const key of directKeys) {
    const value = finitePositiveNumber(model[key]);
    if (value != null) {
      return value;
    }
  }

  for (const key of ["metadata", "model_info", "modelInfo"]) {
    const nested = model[key];
    if (nested && typeof nested === "object" && !Array.isArray(nested)) {
      const value = contextWindowFromProviderModel(nested as Record<string, unknown>);
      if (value != null) {
        return value;
      }
    }
  }

  return undefined;
}

async function fetchProviderModelContextWindows(baseUrl: string, apiKey?: string): Promise<Map<string, number>> {
  const now = Date.now();
  const cached = modelContextCache.get(baseUrl);
  if (cached && now - cached.fetchedAtMs < modelContextCacheTtlMs) {
    return cached.contextWindowsByModel;
  }

  const headers: Record<string, string> = { accept: "application/json" };
  if (apiKey) {
    headers.authorization = `Bearer ${apiKey}`;
  }

  const contextWindowsByModel = new Map<string, number>();
  try {
    const response = await fetch(`${baseUrl}/models`, { headers });
    if (!response.ok) {
      throw new Error(`provider models request failed with HTTP ${response.status}`);
    }
    const payload = await response.json() as unknown;
    const data = payload && typeof payload === "object" && !Array.isArray(payload)
      ? (payload as Record<string, unknown>).data
      : undefined;
    if (Array.isArray(data)) {
      for (const item of data) {
        if (!item || typeof item !== "object" || Array.isArray(item)) {
          continue;
        }
        const providerModel = item as Record<string, unknown>;
        const id = typeof providerModel.id === "string" && providerModel.id.trim()
          ? providerModel.id.trim()
          : undefined;
        const contextWindow = contextWindowFromProviderModel(providerModel);
        if (id && contextWindow != null) {
          contextWindowsByModel.set(id, contextWindow);
        }
      }
    }
  } catch (error) {
    log("debug", "provider_model_context_fetch_failed", {
      baseUrl,
      message: error instanceof Error ? error.message : String(error),
    });
  }

  modelContextCache.set(baseUrl, { fetchedAtMs: now, contextWindowsByModel });
  return contextWindowsByModel;
}

async function contextWindowTokensForModel(params: {
  baseUrl: string;
  apiKey?: string;
  model: string;
}): Promise<number | undefined> {
  const configured = configuredContextWindowTokens(params.model);
  if (configured != null) {
    return configured;
  }

  const providerModels = await fetchProviderModelContextWindows(params.baseUrl, params.apiKey);
  return providerModels.get(params.model)
    ?? builtInModelContextWindows.get(params.model)
    ?? defaultContextWindowTokens;
}

function parseReadFilesToolCall(toolCall: ProviderToolCall): ReadFilesToolCallFile[] | undefined {
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

function stringList(value: unknown): string[] {
  if (Array.isArray(value)) {
    return value.filter((item): item is string => typeof item === "string" && item.trim().length > 0);
  }
  if (typeof value === "string" && value.trim()) {
    return [value.trim()];
  }
  return [];
}

function optionalNumber(value: unknown): number | undefined {
  return Number.isInteger(value) ? Number(value) : undefined;
}

function optionalObject(value: unknown): Record<string, unknown> | undefined {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }
  if (typeof value === "string" && value.trim()) {
    try {
      const parsed = JSON.parse(value) as unknown;
      return parsed && typeof parsed === "object" && !Array.isArray(parsed)
        ? parsed as Record<string, unknown>
        : undefined;
    } catch {
      return undefined;
    }
  }
  return undefined;
}

function parseNativeMcpToolCall(
  toolCall: ProviderToolCall,
  args: Record<string, unknown>,
  mcpTools: McpToolSummary[],
): WarpToolCall | undefined {
  const matchingTools = mcpTools.filter((tool) => tool.name === toolCall.name);
  if (matchingTools.length !== 1) {
    return undefined;
  }

  const serverId = matchingTools[0]?.serverId;
  const singleArgsObject = Object.keys(args).length === 1 ? optionalObject(args.args) : undefined;

  return {
    toolCallId: toolCall.id,
    tool: {
      type: "call_mcp_tool",
      name: toolCall.name,
      ...(serverId ? { serverId } : {}),
      args: singleArgsObject ?? args,
    },
  };
}

function parseToolCall(toolCall: ProviderToolCall, mcpTools: McpToolSummary[] = []): WarpToolCall | undefined {
  const args = JSON.parse(toolCall.argumentsText || "{}") as Record<string, unknown>;

  switch (toolCall.name) {
    case "read_files": {
      const files = parseReadFilesToolCall(toolCall);
      return files ? { toolCallId: toolCall.id, tool: { type: "read_files", files } } : undefined;
    }
    case "run_shell_command": {
      const command = args.command;
      if (typeof command !== "string" || !command.trim()) {
        return undefined;
      }
      return {
        toolCallId: toolCall.id,
        tool: {
          type: "run_shell_command",
          command,
          isReadOnly: typeof args.is_read_only === "boolean" ? args.is_read_only : undefined,
          isRisky: typeof args.is_risky === "boolean"
            ? args.is_risky
            : typeof args.is_read_only === "boolean"
              ? !args.is_read_only
              : true,
          usesPager: typeof args.uses_pager === "boolean" ? args.uses_pager : undefined,
          waitUntilComplete: typeof args.wait_until_complete === "boolean" ? args.wait_until_complete : undefined,
        },
      };
    }
    case "grep": {
      const queries = stringList(args.queries).concat(stringList(args.query));
      if (!queries.length) {
        return undefined;
      }
      return {
        toolCallId: toolCall.id,
        tool: {
          type: "grep",
          queries,
          path: typeof args.path === "string" && args.path.trim() ? args.path.trim() : undefined,
        },
      };
    }
    case "search_codebase": {
      const query = args.query;
      if (typeof query !== "string" || !query.trim()) {
        return undefined;
      }
      return {
        toolCallId: toolCall.id,
        tool: {
          type: "search_codebase",
          query: query.trim(),
          pathFilters: stringList(args.path_filters),
          codebasePath: typeof args.codebase_path === "string" && args.codebase_path.trim()
            ? args.codebase_path.trim()
            : undefined,
        },
      };
    }
    case "file_glob": {
      const patterns = stringList(args.patterns).concat(stringList(args.pattern));
      if (!patterns.length) {
        return undefined;
      }
      return {
        toolCallId: toolCall.id,
        tool: {
          type: "file_glob",
          patterns,
          searchDir: typeof args.search_dir === "string" && args.search_dir.trim() ? args.search_dir.trim() : undefined,
          maxMatches: optionalNumber(args.max_matches),
          maxDepth: optionalNumber(args.max_depth),
          minDepth: optionalNumber(args.min_depth),
        },
      };
    }
    case "read_mcp_resource": {
      const uri = args.uri ?? args.resource_uri;
      if (typeof uri !== "string" || !uri.trim()) {
        return undefined;
      }
      return {
        toolCallId: toolCall.id,
        tool: {
          type: "read_mcp_resource",
          uri: uri.trim(),
          serverId: typeof args.server_id === "string" && args.server_id.trim()
            ? args.server_id.trim()
            : typeof args.serverId === "string" && args.serverId.trim()
              ? args.serverId.trim()
              : undefined,
        },
      };
    }
    case "call_mcp_tool": {
      const name = args.name ?? args.tool_name ?? args.tool;
      if (typeof name !== "string" || !name.trim()) {
        return undefined;
      }
      return {
        toolCallId: toolCall.id,
        tool: {
          type: "call_mcp_tool",
          name: name.trim(),
          serverId: typeof args.server_id === "string" && args.server_id.trim()
            ? args.server_id.trim()
            : typeof args.serverId === "string" && args.serverId.trim()
              ? args.serverId.trim()
              : undefined,
          args: optionalObject(args.args) ?? optionalObject(args.arguments),
        },
      };
    }
    case "apply_file_diffs": {
      return {
        toolCallId: toolCall.id,
        tool: {
          type: "apply_file_diffs",
          summary: typeof args.summary === "string" ? args.summary : "Apply file edits",
          diffs: Array.isArray(args.diffs)
            ? args.diffs.flatMap((diff): Array<{ filePath: string; search?: string; replace?: string }> => {
                if (!diff || typeof diff !== "object") {
                  return [];
                }
                const filePath = (diff as { file_path?: unknown; filePath?: unknown }).file_path
                  ?? (diff as { filePath?: unknown }).filePath;
                if (typeof filePath !== "string" || !filePath.trim()) {
                  return [];
                }
                return [{
                  filePath,
                  search: typeof (diff as { search?: unknown }).search === "string"
                    ? (diff as { search: string }).search
                    : undefined,
                  replace: typeof (diff as { replace?: unknown }).replace === "string"
                    ? (diff as { replace: string }).replace
                    : undefined,
                }];
              })
            : undefined,
          newFiles: Array.isArray(args.new_files)
            ? args.new_files.flatMap((file): Array<{ filePath: string; content: string }> => {
                if (!file || typeof file !== "object") {
                  return [];
                }
                const filePath = (file as { file_path?: unknown; filePath?: unknown }).file_path
                  ?? (file as { filePath?: unknown }).filePath;
                const content = (file as { content?: unknown }).content;
                return typeof filePath === "string" && typeof content === "string"
                  ? [{ filePath, content }]
                  : [];
              })
            : undefined,
          deletedFiles: Array.isArray(args.deleted_files)
            ? args.deleted_files.flatMap((file): Array<{ filePath: string }> => {
                if (!file || typeof file !== "object") {
                  return [];
                }
                const filePath = (file as { file_path?: unknown; filePath?: unknown }).file_path
                  ?? (file as { filePath?: unknown }).filePath;
                return typeof filePath === "string" ? [{ filePath }] : [];
              })
            : undefined,
        },
      };
    }
    case "suggest_plan": {
      const tasks = Array.isArray(args.tasks)
        ? args.tasks.flatMap((task): Array<{ description: string }> => {
            if (typeof task === "string") {
              return task.trim() ? [{ description: task.trim() }] : [];
            }
            if (!task || typeof task !== "object") {
              return [];
            }
            const description = (task as { description?: unknown; title?: unknown }).description
              ?? (task as { title?: unknown }).title;
            return typeof description === "string" && description.trim()
              ? [{ description: description.trim() }]
              : [];
          })
        : [];
      if (!tasks.length) {
        return undefined;
      }
      return {
        toolCallId: toolCall.id,
        tool: {
          type: "suggest_plan",
          summary: typeof args.summary === "string" ? args.summary : "Plan",
          tasks,
        },
      };
    }
    default:
      return parseNativeMcpToolCall(toolCall, args, mcpTools);
  }
}

function formatToolResultForProvider(result: ReturnType<typeof decodeWarpRequest>["toolResults"][number]): string {
  if ("error" in result && result.error) {
    return `Error: ${result.error}`;
  }

  switch (result.type) {
    case "read_files":
      return result.files.map((file) => `File: ${file.filePath}\n${file.content}`).join("\n\n");
    case "run_shell_command":
      return `Command: ${result.command ?? ""}\nExit code: ${result.exitCode ?? ""}\nOutput:\n${result.output ?? ""}`;
    case "grep":
      return result.matchedFiles.map((file) => {
        const lines = file.lineNumbers.length ? ` lines ${file.lineNumbers.join(", ")}` : "";
        return `${file.filePath}${lines}`;
      }).join("\n");
    case "file_glob":
      return `${result.matchedFiles.join("\n")}${result.warnings ? `\nWarnings:\n${result.warnings}` : ""}`;
    case "apply_file_diffs":
      return `Updated files:\n${result.updatedFiles.join("\n")}\nDeleted files:\n${result.deletedFiles.join("\n")}`;
    case "suggest_plan":
      return `Status: ${result.status}${result.planText ? `\nPlan:\n${result.planText}` : ""}`;
    case "generic":
      return `${result.name} result:\n${result.content}`;
  }
}

function formatUserMessageForProvider(warpRequest: ReturnType<typeof decodeWarpRequest>): string {
  const prompt = warpRequest.prompt || "Please use the attached context.";
  if (!warpRequest.contextText) {
    return prompt;
  }

  return [
    "Attached context:",
    warpRequest.contextText,
    "",
    "User request:",
    prompt,
  ].join("\n");
}

function formatUserContentForProvider(warpRequest: ReturnType<typeof decodeWarpRequest>): string | ProviderContentPart[] {
  const text = formatUserMessageForProvider(warpRequest);
  if (!warpRequest.contextImages?.length) {
    return text;
  }

  return [
    { type: "text", text },
    ...warpRequest.contextImages.map((image) => ({
      type: "image_url" as const,
      image_url: { url: `data:${image.mimeType};base64,${image.data}` },
    })),
  ];
}

function formatSummarizationRequestForProvider(warpRequest: ReturnType<typeof decodeWarpRequest>): string {
  return [
    "Summarize the conversation so far into a compact handoff for continuing the same task.",
    "Preserve current goals, decisions, constraints, important file paths, commands, errors, and outstanding next steps.",
    "Omit repetitive transcript detail and keep the summary dense.",
    warpRequest.summarizationPrompt
      ? `Additional user instruction:\n${warpRequest.summarizationPrompt}`
      : undefined,
  ].filter((line): line is string => line != null).join("\n\n");
}

function formatCompactedConversationSummary(summary: string): string {
  return [
    "The conversation before this point was compacted.",
    "Summary:",
    summary.trim(),
  ].join("\n");
}

function providerMessageContentLength(content: ProviderChatMessage["content"]): number {
  if (typeof content === "string") {
    return content.length;
  }
  if (Array.isArray(content)) {
    return content.reduce((sum, part) => sum + (part.type === "text" ? part.text.length : part.image_url.url.length), 0);
  }
  return 0;
}

function providerSystemContent(content: ProviderChatMessage["content"]): string | undefined {
  if (typeof content === "string") {
    return nonEmpty(content);
  }
  if (Array.isArray(content)) {
    return nonEmpty(content
      .filter((part) => part.type === "text")
      .map((part) => part.text)
      .join("\n"));
  }
  return undefined;
}

function buildProviderMessages(
  systemPromptContents: Array<string | undefined>,
  conversationMessages: ProviderChatMessage[],
): ProviderChatMessage[] {
  const systemContents = [...systemPromptContents];
  const nonSystemMessages: ProviderChatMessage[] = [];

  for (const message of conversationMessages) {
    if (message.role === "system") {
      systemContents.push(providerSystemContent(message.content));
    } else {
      nonSystemMessages.push(message);
    }
  }

  const systemContent = systemContents
    .map((content) => nonEmpty(content))
    .filter((content): content is string => content != null)
    .join("\n\n");

  // Some OpenAI-compatible providers reject multiple system messages, even
  // when every system message is at the beginning of the request.
  return systemContent
    ? [{ role: "system", content: systemContent }, ...nonSystemMessages]
    : nonSystemMessages;
}

function approximateTokenCount(chars: number): number {
  return Math.ceil(chars / 4);
}

function providerToolCallContentLength(toolCalls: ProviderToolCall[]): number {
  return toolCalls.reduce((sum, toolCall) => sum + toolCall.id.length + toolCall.name.length + toolCall.argumentsText.length, 0);
}

function estimateContextWindowUsage(params: {
  messages: ProviderChatMessage[];
  tools?: object[];
  assistantResponse?: ProviderResponse;
  contextWindowTokens?: number;
}): number | undefined {
  if (!params.contextWindowTokens) {
    return undefined;
  }

  const messageChars = params.messages.reduce(
    (sum, message) => sum + message.role.length + providerMessageContentLength(message.content) + providerToolCallContentLength(
      (message.tool_calls ?? []).map((toolCall) => ({
        id: toolCall.id,
        name: toolCall.function.name,
        argumentsText: toolCall.function.arguments,
      })),
    ),
    0,
  );
  const toolsChars = params.tools?.length ? JSON.stringify(params.tools).length : 0;
  const assistantChars = params.assistantResponse
    ? providerMessageContentLength(params.assistantResponse.content) + providerToolCallContentLength(params.assistantResponse.toolCalls)
    : 0;
  const estimatedTokens = approximateTokenCount(messageChars + toolsChars + assistantChars);
  return Math.min(1, estimatedTokens / params.contextWindowTokens);
}

function streamFinishedForError(error: unknown): Uint8Array {
  if (error instanceof LocalAgentError) {
    switch (error.finishReason) {
      case "invalid_api_key":
        return encodeStreamFinishedInvalidApiKey(error.modelName);
      case "llm_unavailable":
        return encodeStreamFinishedLlmUnavailable();
      case "context_window_exceeded":
        return encodeStreamFinishedContextWindowExceeded();
      case "quota_limit":
        return encodeStreamFinishedQuotaLimit();
      case "internal_error":
        break;
    }
  }

  const message = error instanceof Error ? error.message : String(error);
  return encodeStreamFinishedInternalError(message);
}

function isProviderChatMessage(value: unknown): value is ProviderChatMessage {
  if (!value || typeof value !== "object") {
    return false;
  }
  const role = (value as { role?: unknown }).role;
  return role === "system" || role === "user" || role === "assistant" || role === "tool";
}

function messagesFromStoredConversation(messages: unknown[]): ProviderChatMessage[] {
  return messages.filter(isProviderChatMessage).slice(-maxConversationMessages);
}

function loadConversationState(): void {
  try {
    const conversations = integrationStore.listAiConversations();
    for (const conversation of conversations) {
      conversationState.set(conversation.conversationId, {
        messages: messagesFromStoredConversation(conversation.messages),
      });
    }
    log("info", "conversation_state_loaded", {
      path: localGraphqlDbPath,
      conversationCount: conversationState.size,
    });
  } catch (error) {
    log("warn", "conversation_state_load_failed", {
      path: localGraphqlDbPath,
      message: error instanceof Error ? error.message : String(error),
    });
  }
}

function persistConversationState(conversationId: string): void {
  const state = conversationState.get(conversationId);
  if (!state) {
    return;
  }

  try {
    integrationStore.upsertAiConversation(conversationId, state.messages);
  } catch (error) {
    log("warn", "conversation_state_persist_failed", {
      path: localGraphqlDbPath,
      conversationId,
      message: error instanceof Error ? error.message : String(error),
    });
  }
}

function trimConversationState(state: { messages: ProviderChatMessage[] }): void {
  if (state.messages.length > maxConversationMessages) {
    state.messages.splice(0, state.messages.length - maxConversationMessages);
  }
}

function classifyProviderError(status: number, body: string, model: string): LocalAgentError {
  const lowerBody = body.toLowerCase();
  const message = `OpenAI-compatible endpoint returned ${status}: ${body}`;
  if (status === 401 || status === 403 || /invalid[_ ]api[_ ]key|incorrect api key|unauthorized/.test(lowerBody)) {
    return new LocalAgentError(message, "invalid_api_key", model);
  }
  if (status === 429) {
    return new LocalAgentError(message, "quota_limit", model);
  }
  if (
    status === 413
    || /context[_ -]?window|context length|maximum context|too many tokens|token limit|input is too long/.test(lowerBody)
  ) {
    return new LocalAgentError(message, "context_window_exceeded", model);
  }
  if ([408, 500, 502, 503, 504].includes(status)) {
    return new LocalAgentError(message, "llm_unavailable", model);
  }
  return new LocalAgentError(message, "internal_error", model);
}

function stateForConversation(conversationId: string): { messages: ProviderChatMessage[] } {
  const existing = conversationState.get(conversationId);
  if (existing) {
    return existing;
  }

  const persisted = integrationStore.getAiConversation(conversationId);
  if (persisted) {
    const loaded = {
      messages: messagesFromStoredConversation(persisted.messages),
    };
    conversationState.set(conversationId, loaded);
    return loaded;
  }

  const created = { messages: [] };
  conversationState.set(conversationId, created);
  return created;
}

function pendingProviderMessages(warpRequest: ReturnType<typeof decodeWarpRequest>): ProviderChatMessage[] {
  const pendingMessages: ProviderChatMessage[] = [];

  if (warpRequest.isSummarizationRequest) {
    pendingMessages.push({
      role: "user",
      content: formatSummarizationRequestForProvider(warpRequest),
    });
  } else if (warpRequest.toolResults.length > 0) {
    for (const result of warpRequest.toolResults) {
      pendingMessages.push({
        role: "tool",
        tool_call_id: result.toolCallId,
        content: formatToolResultForProvider(result),
      });
    }
  } else if (warpRequest.prompt || warpRequest.contextText || warpRequest.contextImages?.length) {
    pendingMessages.push({
      role: "user",
      content: formatUserContentForProvider(warpRequest),
    });
  }

  return pendingMessages;
}

function prepareProviderMessages(warpRequest: ReturnType<typeof decodeWarpRequest>): {
  messages: ProviderChatMessage[];
  pendingMessages: ProviderChatMessage[];
} {
  const state = stateForConversation(warpRequest.conversationId);
  const pendingMessages = pendingProviderMessages(warpRequest);

  return {
    messages: state.messages.concat(pendingMessages),
    pendingMessages,
  };
}

function rememberProviderResponse(
  conversationId: string,
  pendingMessages: ProviderChatMessage[],
  providerResponse: ProviderResponse,
): void {
  const state = stateForConversation(conversationId);
  state.messages.push(...pendingMessages);
  state.messages.push({
    role: "assistant",
    content: providerResponse.content,
    ...(providerResponse.toolCalls.length
      ? { tool_calls: providerResponse.toolCalls.map(providerToolCallMessage) }
      : {}),
  });
  trimConversationState(state);
  persistConversationState(conversationId);
}

function rememberProviderSummarization(
  conversationId: string,
  providerResponse: ProviderResponse,
): void {
  const state = stateForConversation(conversationId);
  state.messages = [{
    role: "system",
    content: formatCompactedConversationSummary(providerResponse.content),
  }];
  persistConversationState(conversationId);
}

async function streamChatCompletion(params: {
  messages: ProviderChatMessage[];
  apiKey?: string;
  baseUrl?: string;
  model?: string;
  mcpTools?: McpToolSummary[];
  enableTools?: boolean;
  onContentChunk?: (chunk: string) => void;
}): Promise<ProviderResponse> {
  const apiKey = params.apiKey ?? process.env.OPENAI_API_KEY;
  if (!apiKey) {
    throw new LocalAgentError("OPENAI_API_KEY is not set and the Warp request did not include an OpenAI key.", "invalid_api_key", params.model);
  }

  const baseUrl = trimTrailingSlash(nonEmpty(params.baseUrl) ?? nonEmpty(process.env.OPENAI_BASE_URL) ?? defaultBaseUrl);
  const model = resolveProviderModel({
    openAiModel: process.env.OPENAI_MODEL,
    warpModel: params.model,
    localModelAliases: process.env.LOCAL_MODEL_ALIASES,
  });
  const contextWindowTokens = await contextWindowTokensForModel({
    baseUrl,
    apiKey,
    model,
  });
  log("info", "provider_request", {
    baseUrl,
    model,
    warpModel: params.model ?? null,
    messageCount: params.messages.length,
    promptChars: params.messages.reduce((sum, message) => sum + providerMessageContentLength(message.content), 0),
    contextWindowTokens: contextWindowTokens ?? null,
    baseUrlSource: nonEmpty(params.baseUrl) ? "request_header" : nonEmpty(process.env.OPENAI_BASE_URL) ? "env" : "default",
  });

  const tools = params.enableTools !== false && shouldEnableTools() ? localToolSchemas() : undefined;
  const messages = buildProviderMessages([
    tools ? localToolUseSystemPrompt(params.mcpTools ?? []) : undefined,
    process.env.LOCAL_MULTI_AGENT_SYSTEM_PROMPT,
  ], params.messages);

  const requestBody = {
    model,
    messages,
    temperature: 0.2,
    stream: true,
    ...(tools ? { tools, tool_choice: "auto" } : {}),
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
    throw classifyProviderError(completionResponse.status, body, model);
  }

  if (!completionResponse.body) {
    throw new LocalAgentError("OpenAI-compatible endpoint returned no response stream.", "llm_unavailable", model);
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
          contextWindowTokens,
          contextWindowUsage: estimateContextWindowUsage({
            messages,
            tools,
            assistantResponse: {
              content,
              toolCalls: [...toolCalls.values()].filter((toolCall) => toolCall.id && toolCall.name),
            },
            contextWindowTokens,
          }),
        };
      }

      const parsed = JSON.parse(data);
      const chunk = extractStreamingContent(parsed);
      if (chunk) {
        content += chunk;
        params.onContentChunk?.(chunk);
      }
      extractStreamingToolCalls(parsed, toolCalls);
    }
  }

  return {
    content,
    toolCalls: [...toolCalls.values()].filter((toolCall) => toolCall.id && toolCall.name),
    contextWindowTokens,
    contextWindowUsage: estimateContextWindowUsage({
      messages,
      tools,
      assistantResponse: {
        content,
        toolCalls: [...toolCalls.values()].filter((toolCall) => toolCall.id && toolCall.name),
      },
      contextWindowTokens,
    }),
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
    contextChars: warpRequest.contextText?.length ?? 0,
    contextImageCount: warpRequest.contextImages?.length ?? 0,
    toolResultTypes: warpRequest.toolResults.map((result) => result.type === "generic" ? result.name : result.type),
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

  let providerResponseForUsage: ProviderResponse | undefined;
  let summarizedConversation = false;
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

      const providerMessages = prepareProviderMessages(warpRequest);
      const assistantMessageId = randomUUID();
      let streamedAgentOutput = false;
      const providerResponse = await streamChatCompletion({
        messages: providerMessages.messages.length
          ? providerMessages.messages
          : [{ role: "user", content: "Continue." }],
        apiKey: warpRequest.openAiApiKey,
        baseUrl: requestOpenAiBaseUrl,
        model: warpRequest.model,
        mcpTools: warpRequest.mcpTools,
        enableTools: !warpRequest.isSummarizationRequest,
        onContentChunk: warpRequest.isSummarizationRequest ? undefined : (chunk) => {
          if (!streamedAgentOutput) {
            streamedAgentOutput = true;
            sendEvent(
              "add_agent_output",
              encodeAddAgentOutput({
                messageId: assistantMessageId,
                taskId: warpRequest.rootTaskId,
                requestId: warpRequest.requestId,
                text: chunk,
              }),
            );
            return;
          }

          sendEvent(
            "append_agent_output",
            encodeAppendAgentOutput({
              messageId: assistantMessageId,
              taskId: warpRequest.rootTaskId,
              requestId: warpRequest.requestId,
              text: chunk,
            }),
          );
        },
      });

      if (!providerResponse.content && !providerResponse.toolCalls.length) {
        throw new LocalAgentError("OpenAI-compatible endpoint returned no assistant content or tool calls.");
      }

      providerResponseForUsage = providerResponse;
      if (warpRequest.isSummarizationRequest) {
        summarizedConversation = true;
        rememberProviderSummarization(warpRequest.conversationId, providerResponse);
        providerResponseForUsage = {
          ...providerResponse,
          contextWindowUsage: estimateContextWindowUsage({
            messages: stateForConversation(warpRequest.conversationId).messages,
            contextWindowTokens: providerResponse.contextWindowTokens,
          }),
        };
      } else {
        rememberProviderResponse(warpRequest.conversationId, providerMessages.pendingMessages, providerResponse);
      }

      if (warpRequest.isSummarizationRequest) {
        sendEvent(
          "add_conversation_summary",
          encodeAddConversationSummary({
            messageId: assistantMessageId,
            taskId: warpRequest.rootTaskId,
            requestId: warpRequest.requestId,
            text: providerResponse.content,
            tokenCount: approximateTokenCount(providerResponse.content.length),
          }),
        );
      } else if (providerResponse.content && !streamedAgentOutput) {
        sendEvent(
          "add_agent_output",
          encodeAddAgentOutput({
            messageId: assistantMessageId,
            taskId: warpRequest.rootTaskId,
            requestId: warpRequest.requestId,
            text: providerResponse.content,
          }),
        );
      }

      for (const toolCall of providerResponse.toolCalls) {
        const parsedToolCall = parseToolCall(toolCall, warpRequest.mcpTools);
        if (!parsedToolCall) {
          throw new LocalAgentError(`Unsupported provider tool call: ${toolCall.name}`);
        }

        sendEvent(
          `add_${parsedToolCall.tool.type}_tool_call`,
          encodeAddToolCall({
            messageId: randomUUID(),
            taskId: warpRequest.rootTaskId,
            requestId: warpRequest.requestId,
            ...parsedToolCall,
          }),
        );
      }
    }

    sendEvent("stream_finished_done", encodeStreamFinishedDone({
      contextWindowUsage: providerResponseForUsage?.contextWindowUsage,
      summarized: summarizedConversation,
    }));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    log("error", "multi_agent_error", {
      requestId: warpRequest.requestId,
      message,
    });
    sendEvent("stream_finished_error", streamFinishedForError(error));
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
      sendJson(response, 200, {
        ok: true,
        version: serviceVersion,
        configHash: process.env.LOCAL_CONFIG_HASH ?? null,
      });
      log("info", "http_response", {
        method,
        path: url.pathname,
        statusCode: response.statusCode,
        durationMs: Date.now() - startedAt,
      });
      return;
    }

    if (method === "GET") {
      const sharedSessionUrl = sharedSessionIntentUrl(url);
      if (sharedSessionUrl) {
        sendRedirect(response, sharedSessionUrl);
        log("info", "http_response", {
          method,
          path: url.pathname,
          statusCode: response.statusCode,
          durationMs: Date.now() - startedAt,
        });
        return;
      }
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

    if (method === "POST" && url.pathname === "/graphql/v2") {
      const body = Buffer.from(await readBody(request)).toString("utf8");
      const graphqlRequest = JSON.parse(body) as unknown;
      const result = await handleLocalGraphqlRequest(
        graphqlRequest && typeof graphqlRequest === "object" ? graphqlRequest : {},
        integrationStore,
        url.searchParams.get("op"),
      );
      const errorMessages = graphqlErrorMessages(result.payload);
      log(result.status >= 400 || errorMessages.length > 0 ? "warn" : "info", "graphql_response", {
        ...result.diagnostics,
        statusCode: result.status,
        errorMessages,
      });
      sendJson(response, result.status, result.payload);
      log("info", "http_response", {
        method,
        path: url.pathname,
        graphqlOperationName: result.diagnostics.operationName,
        graphqlCanonicalOperationName: result.diagnostics.canonicalOperationName,
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

loadConversationState();

server.listen(port, host, () => {
  log("info", "server_started", {
    url: `http://${host}:${port}`,
    logLevel: process.env.LOG_LEVEL?.trim() || "info",
    logFilePath: logFilePath() ?? null,
    hasOpenAiBaseUrlEnv: nonEmpty(process.env.OPENAI_BASE_URL) != null,
    hasOpenAiModelEnv: nonEmpty(process.env.OPENAI_MODEL) != null,
    hasModelAliasesEnv: nonEmpty(process.env.LOCAL_MODEL_ALIASES) != null,
    localGraphqlDbPath,
    conversationStateCount: conversationState.size,
    maxConversationMessages,
  });
});
