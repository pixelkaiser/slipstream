import http from "node:http";
import { setTimeout as delay } from "node:timers/promises";
import {
  decodeWarpRequest,
  encodeAgentOutput,
  encodeBase64Url,
  encodeStreamFinishedDone,
  encodeStreamFinishedInternalError,
  encodeStreamInit,
} from "./protobuf.js";

const port = Number.parseInt(process.env.PORT ?? "8787", 10);
const defaultBaseUrl = "https://api.openai.com/v1";
const maxRequestBytes = 25 * 1024 * 1024;

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
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

async function createChatCompletion(params: {
  prompt: string;
  apiKey?: string;
  model?: string;
}): Promise<string> {
  const apiKey = params.apiKey ?? process.env.OPENAI_API_KEY;
  if (!apiKey) {
    throw new Error("OPENAI_API_KEY is not set and the Warp request did not include an OpenAI key.");
  }

  const baseUrl = trimTrailingSlash(process.env.OPENAI_BASE_URL ?? defaultBaseUrl);
  const model = process.env.OPENAI_MODEL ?? params.model;
  if (!model) {
    throw new Error("OPENAI_MODEL is not set and the Warp request did not include a base model.");
  }

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
      stream: false,
    }),
  });

  if (!completionResponse.ok) {
    const body = await completionResponse.text();
    throw new Error(`OpenAI-compatible endpoint returned ${completionResponse.status}: ${body}`);
  }

  const payload = await completionResponse.json() as {
    choices?: Array<{ message?: { content?: string } }>;
  };
  const content = payload.choices?.[0]?.message?.content;
  if (!content) {
    throw new Error("OpenAI-compatible endpoint returned no assistant content.");
  }

  return content;
}

async function handleMultiAgent(
  request: http.IncomingMessage,
  response: http.ServerResponse,
  passiveSuggestions: boolean,
): Promise<void> {
  const body = await readBody(request);
  const warpRequest = decodeWarpRequest(body);

  response.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-cache, no-transform",
    connection: "keep-alive",
    "x-accel-buffering": "no",
  });

  writeSse(response, encodeStreamInit(warpRequest.conversationId, warpRequest.requestId));

  try {
    if (!passiveSuggestions) {
      const output = await createChatCompletion({
        prompt: warpRequest.prompt,
        apiKey: warpRequest.openAiApiKey,
        model: warpRequest.model,
      });

      writeSse(
        response,
        encodeAgentOutput({
          taskId: warpRequest.rootTaskId,
          requestId: warpRequest.requestId,
          text: output,
        }),
      );
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
