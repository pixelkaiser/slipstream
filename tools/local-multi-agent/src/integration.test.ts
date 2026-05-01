import assert from "node:assert/strict";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import test from "node:test";
import { fromBinary } from "@bufbuild/protobuf";
import { ResponseEventSchema } from "./generated/warp_multi_agent/v1/response_pb.js";

function lengthDelimitedField(fieldNumber: number, value: Uint8Array): Uint8Array {
  assert.ok(value.length < 128, "test helper only supports one-byte lengths");
  return Uint8Array.from([(fieldNumber << 3) | 2, value.length, ...value]);
}

function stringField(fieldNumber: number, value: string): Uint8Array {
  return lengthDelimitedField(fieldNumber, Buffer.from(value));
}

function metadata(conversationId?: string): Uint8Array {
  return conversationId ? lengthDelimitedField(4, stringField(1, conversationId)) : new Uint8Array();
}

function warpPromptRequest(prompt: string, conversationId?: string): Uint8Array {
  const deprecatedUserQuery = lengthDelimitedField(2, stringField(1, prompt));
  return Buffer.concat([lengthDelimitedField(2, deprecatedUserQuery), metadata(conversationId)]);
}

function warpSummarizeRequest(conversationId: string, prompt?: string): Uint8Array {
  const summarizeConversation = prompt ? stringField(1, prompt) : new Uint8Array();
  return Buffer.concat([lengthDelimitedField(2, lengthDelimitedField(13, summarizeConversation)), metadata(conversationId)]);
}

function warpPromptRequestWithMcpTool(prompt: string, params: {
  toolName: string;
  toolDescription?: string;
  serverName?: string;
  serverId?: string;
  conversationId?: string;
}): Uint8Array {
  const tool = Buffer.concat([
    stringField(1, params.toolName),
    params.toolDescription ? stringField(2, params.toolDescription) : new Uint8Array(),
  ]);
  const server = Buffer.concat([
    stringField(1, params.serverName ?? "MCP Server"),
    lengthDelimitedField(4, tool),
    params.serverId ? stringField(5, params.serverId) : new Uint8Array(),
  ]);
  const mcpContext = lengthDelimitedField(6, lengthDelimitedField(3, server));
  return Buffer.concat([warpPromptRequest(prompt, params.conversationId), mcpContext]);
}

function warpPromptRequestWithDuplicateMcpTools(prompt: string): Uint8Array {
  const servers = ["linear-server-a", "linear-server-b"].map((serverId) => {
    const tool = stringField(1, "list_issues");
    return lengthDelimitedField(3, Buffer.concat([
      stringField(1, "Linear"),
      lengthDelimitedField(4, tool),
      stringField(5, serverId),
    ]));
  });
  return Buffer.concat([warpPromptRequest(prompt), lengthDelimitedField(6, Buffer.concat(servers))]);
}

function warpPromptRequestWithSelectedText(prompt: string, selectedText: string, conversationId?: string): Uint8Array {
  const selectedTextContext = lengthDelimitedField(6, stringField(1, selectedText));
  const context = lengthDelimitedField(1, selectedTextContext);
  const deprecatedUserQuery = lengthDelimitedField(2, stringField(1, prompt));
  return Buffer.concat([lengthDelimitedField(2, Buffer.concat([context, deprecatedUserQuery])), metadata(conversationId)]);
}

function warpReadFilesResultRequest(params: {
  conversationId: string;
  toolCallId: string;
  filePath: string;
  content: string;
}): Uint8Array {
  const fileContent = Buffer.concat([
    stringField(1, params.filePath),
    stringField(2, params.content),
  ]);
  const textFilesSuccess = lengthDelimitedField(1, fileContent);
  const readFilesResult = lengthDelimitedField(1, textFilesSuccess);
  const toolCallResult = Buffer.concat([
    stringField(1, params.toolCallId),
    lengthDelimitedField(3, readFilesResult),
  ]);
  const userInput = lengthDelimitedField(2, toolCallResult);
  const userInputs = lengthDelimitedField(1, userInput);
  const input = lengthDelimitedField(6, userInputs);
  return Buffer.concat([lengthDelimitedField(2, input), metadata(params.conversationId)]);
}

async function readBody(request: http.IncomingMessage): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of request) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  return Buffer.concat(chunks).toString("utf8");
}

async function listenOnLoopback(server: http.Server): Promise<number> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      const address = server.address();
      assert.notEqual(typeof address, "string");
      assert.ok(address);
      const { port } = address as AddressInfo;
      resolve(port);
    });
  });
}

async function closeServer(server: http.Server): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
}

async function waitForHealth(port: number, child: ChildProcessWithoutNullStreams): Promise<void> {
  const healthUrl = `http://127.0.0.1:${port}/health`;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (child.exitCode != null) {
      throw new Error(`local service exited with code ${child.exitCode}`);
    }

    try {
      const response = await fetch(healthUrl);
      if (response.ok) {
        return;
      }
    } catch {
      // Retry while the server starts listening.
    }

    await delay(25);
  }

  throw new Error("local service did not become healthy");
}

function stopChild(child: ChildProcessWithoutNullStreams): void {
  if (child.exitCode == null && !child.killed) {
    child.kill();
  }
}

function sseEventBytes(event: string): Buffer {
  return Buffer.from(event.replace(/^data: /, "").replace(/-/g, "+").replace(/_/g, "/"), "base64");
}

function sseResponseEvent(event: string) {
  return fromBinary(ResponseEventSchema, sseEventBytes(event));
}

test("redirects shared session web links to Warp URL scheme", { timeout: 10_000 }, async () => {
  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-session-link-service-"));
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(servicePort),
      LOG_LEVEL: "error",
      WARP_URL_SCHEME: "warplocal",
      LOCAL_GRAPHQL_DB_PATH: join(dir, "local.sqlite"),
    },
  });

  try {
    await waitForHealth(servicePort, child);
    const sessionId = "00000000-0000-0000-0000-000000000000";
    const response = await fetch(
      `http://127.0.0.1:${servicePort}/session/${sessionId}?pwd=secret&preview=true`,
      { redirect: "manual" },
    );

    assert.equal(response.status, 302);
    assert.equal(
      response.headers.get("location"),
      `warplocal://shared_session/${sessionId}?pwd=secret&preview=true`,
    );
  } finally {
    stopChild(child);
    rmSync(dir, { recursive: true, force: true });
  }
});

function maybeHandleProviderModelsRequest(request: http.IncomingMessage, response: http.ServerResponse): boolean {
  if (request.method !== "GET" || request.url !== "/v1/models") {
    return false;
  }

  response.writeHead(200, { "content-type": "application/json" });
  response.end(JSON.stringify({
    object: "list",
    data: [{ id: "mock-model", context_length: 131072 }],
  }));
  return true;
}

function providerNonSystemMessages(body: unknown): unknown[] {
  const messages = (body as { messages?: unknown[] }).messages ?? [];
  return messages.filter((message) => (message as { role?: unknown }).role !== "system");
}

function providerSystemMessages(body: unknown): Array<{ role?: unknown; content?: unknown }> {
  const messages = (body as { messages?: Array<{ role?: unknown; content?: unknown }> }).messages ?? [];
  return messages.filter((message) => message.role === "system");
}

test("serves local GraphQL integration config over HTTP", { timeout: 10_000 }, async () => {
  const provider = http.createServer((request, response) => {
    if (request.url !== "/v1/models") {
      response.writeHead(404);
      response.end();
      return;
    }

    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({
      object: "list",
      data: [{ id: "route-local-model" }],
    }));
  });
  const providerPort = await listenOnLoopback(provider);

  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-graphql-service-"));
  const stdout: string[] = [];
  const stderr: string[] = [];
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(servicePort),
      LOG_LEVEL: "error",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      LOCAL_GRAPHQL_DB_PATH: join(dir, "local.sqlite"),
    },
  });
  child.stdout.on("data", (chunk) => stdout.push(String(chunk)));
  child.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  try {
    await waitForHealth(servicePort, child);
    const createResponse = await fetch(`http://127.0.0.1:${servicePort}/graphql/v2?op=CreateSimpleIntegration`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        variables: {
          integration_type: "linear",
          is_update: false,
          enabled: true,
          config: {
            base_prompt: "Route test prompt",
            mcp_servers_json: JSON.stringify({ local: { command: "node" } }),
          },
        },
      }),
    });
    assert.equal(createResponse.status, 200);
    const createJson = await createResponse.json() as {
      data?: { createSimpleIntegration?: { success?: boolean; authUrl?: string | null } };
    };
    assert.equal(createJson.data?.createSimpleIntegration?.success, true);
    assert.equal(createJson.data?.createSimpleIntegration?.authUrl, null);

    const listResponse = await fetch(`http://127.0.0.1:${servicePort}/graphql/v2`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        operationName: "SimpleIntegrations",
        variables: { input: { providers: ["linear"] } },
      }),
    });
    assert.equal(listResponse.status, 200);
    const listJson = await listResponse.json() as {
      data?: {
        simpleIntegrations?: {
          integrations?: Array<{
            connectionStatus?: string;
            integrationConfig?: { basePrompt?: string; mcpServersJson?: string };
          }>;
        };
      };
    };
    const integration = listJson.data?.simpleIntegrations?.integrations?.[0];
    assert.equal(integration?.connectionStatus, "ACTIVE");
    assert.equal(integration?.integrationConfig?.basePrompt, "Route test prompt");
    assert.deepEqual(JSON.parse(integration?.integrationConfig?.mcpServersJson ?? ""), {
      local: { command: "node" },
    });

    const cloudObjectsResponse = await fetch(`http://127.0.0.1:${servicePort}/graphql/v2`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        operationName: "GetUpdatedCloudObjects",
        variables: { input: { forceRefresh: false } },
      }),
    });
    assert.equal(cloudObjectsResponse.status, 200);
    const cloudObjectsJson = await cloudObjectsResponse.json() as {
      data?: { updatedCloudObjects?: { __typename?: string; notebooks?: unknown[] } };
      errors?: unknown[];
    };
    assert.equal(cloudObjectsJson.errors, undefined);
    assert.equal(cloudObjectsJson.data?.updatedCloudObjects?.__typename, "UpdatedCloudObjectsOutput");
    assert.deepEqual(cloudObjectsJson.data?.updatedCloudObjects?.notebooks, []);

    const workspacesResponse = await fetch(`http://127.0.0.1:${servicePort}/graphql/v2`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        operationName: "GetWorkspacesMetadataForUser",
        variables: {},
      }),
    });
    assert.equal(workspacesResponse.status, 200);
    const workspacesJson = await workspacesResponse.json() as {
      data?: {
        user?: { __typename?: string; user?: { workspaces?: unknown[] } };
        pricingInfo?: { __typename?: string };
      };
      errors?: unknown[];
    };
    assert.equal(workspacesJson.errors, undefined);
    assert.equal(workspacesJson.data?.user?.__typename, "UserOutput");
    assert.deepEqual(workspacesJson.data?.user?.user?.workspaces, []);
    assert.equal(workspacesJson.data?.pricingInfo?.__typename, "PricingInfoOutput");

    const modelsResponse = await fetch(`http://127.0.0.1:${servicePort}/graphql/v2`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        operationName: "GetFeatureModelChoices",
        variables: {},
      }),
    });
    assert.equal(modelsResponse.status, 200);
    const modelsJson = await modelsResponse.json() as {
      data?: {
        user?: {
          user?: {
            workspaces?: Array<{
              featureModelChoice?: { agentMode?: { choices?: Array<{ id?: string }> } };
            }>;
          };
        };
      };
      errors?: unknown[];
    };
    assert.equal(modelsJson.errors, undefined);
    assert.deepEqual(
      modelsJson.data?.user?.user?.workspaces?.[0]?.featureModelChoice?.agentMode?.choices?.map((choice) => choice.id),
      ["route-local-model"],
    );
  } catch (error) {
    const diagnostics = [
      `stdout:\n${stdout.join("")}`,
      `stderr:\n${stderr.join("")}`,
    ].join("\n");
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
  } finally {
    stopChild(child);
    await closeServer(provider);
    rmSync(dir, { force: true, recursive: true });
  }
});

test("serves a Warp multi-agent request through a mock OpenAI-compatible stream", { timeout: 10_000 }, async () => {
  let providerPath: string | undefined;
  let providerBody: unknown;
  const provider = http.createServer(async (request, response) => {
    if (maybeHandleProviderModelsRequest(request, response)) {
      return;
    }

    providerPath = request.url;
    providerBody = JSON.parse(await readBody(request));
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
    });
    response.write('data: {"choices":[{"delta":{"content":"hello"}}]}\n\n');
    response.write('data: {"choices":[{"delta":{"content":" warp"}}]}\n\n');
    response.write("data: [DONE]\n\n");
    response.end();
  });
  const providerPort = await listenOnLoopback(provider);

  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-agent-service-"));
  const stdout: string[] = [];
  const stderr: string[] = [];
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(servicePort),
      OPENAI_API_KEY: "sk-local-test",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "mock-model",
      LOG_LEVEL: "error",
      LOCAL_GRAPHQL_DB_PATH: join(dir, "local.sqlite"),
    },
  });
  child.stdout.on("data", (chunk) => stdout.push(String(chunk)));
  child.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  try {
    await waitForHealth(servicePort, child);
    const response = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequest("hello provider")),
    });

    assert.equal(response.status, 200);
    assert.match(response.headers.get("content-type") ?? "", /text\/event-stream/);
    const streamText = await response.text();
    const events = streamText.split("\n\n").filter(Boolean);

    assert.equal(providerPath, "/v1/chat/completions");
    assert.equal((providerBody as { model?: unknown }).model, "mock-model");
    assert.deepEqual(providerNonSystemMessages(providerBody), [{ role: "user", content: "hello provider" }]);
    assert.equal((providerBody as { stream?: unknown }).stream, true);
    assert.equal((providerBody as { tool_choice?: unknown }).tool_choice, "auto");
    assert.equal(Array.isArray((providerBody as { tools?: unknown }).tools), true);
    assert.deepEqual(
      ((providerBody as { tools?: Array<{ function?: { name?: string } }> }).tools ?? []).map((tool) => tool.function?.name),
      [
        "read_files",
        "file_glob",
        "grep",
        "search_codebase",
        "run_shell_command",
        "apply_file_diffs",
        "suggest_plan",
        "read_mcp_resource",
        "call_mcp_tool",
      ],
    );
    assert.equal(events.length, 5);
    assert.ok(events.every((event) => /^data: [-_A-Za-z0-9]+=*$/.test(event)));
    assert.equal(sseEventBytes(events[2]).includes("hello"), true);
    assert.equal(sseEventBytes(events[3]).includes(" warp"), true);
    assert.equal(sseEventBytes(events[3]).includes("agent_output.text"), true);
    assert.equal(sseEventBytes(events[3]).includes("message.agent_output.text"), false);
  } catch (error) {
    const diagnostics = [
      `stdout:\n${stdout.join("")}`,
      `stderr:\n${stderr.join("")}`,
    ].join("\n");
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
  } finally {
    stopChild(child);
    await closeServer(provider);
    rmSync(dir, { force: true, recursive: true });
  }
});

test("passes attached selected text context to the provider", { timeout: 10_000 }, async () => {
  let providerBody: unknown;
  const provider = http.createServer(async (request, response) => {
    if (maybeHandleProviderModelsRequest(request, response)) {
      return;
    }

    providerBody = JSON.parse(await readBody(request));
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
    });
    response.write('data: {"choices":[{"delta":{"content":"explained"}}]}\n\n');
    response.write("data: [DONE]\n\n");
    response.end();
  });
  const providerPort = await listenOnLoopback(provider);

  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-agent-context-"));
  const stdout: string[] = [];
  const stderr: string[] = [];
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(servicePort),
      OPENAI_API_KEY: "sk-local-test",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "mock-model",
      LOG_LEVEL: "error",
      LOCAL_GRAPHQL_DB_PATH: join(dir, "local.sqlite"),
    },
  });
  child.stdout.on("data", (chunk) => stdout.push(String(chunk)));
  child.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  try {
    await waitForHealth(servicePort, child);
    const response = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequestWithSelectedText(
        "explain this output",
        "Filesystem Size Used Avail Use% Mounted on\n/dev/md3 1.5T 42G 1.4T 3% /",
        "conversation-selected-text",
      )),
    });

    assert.equal(response.status, 200);
    await response.text();

    const messages = providerNonSystemMessages(providerBody) as Array<{ content?: string }>;
    assert.equal(messages.length, 1);
    assert.match(messages[0]?.content ?? "", /Attached context:/);
    assert.match(messages[0]?.content ?? "", /Selected text:\nFilesystem Size Used Avail/);
    assert.match(messages[0]?.content ?? "", /User request:\nexplain this output/);
  } catch (error) {
    const diagnostics = [
      `stdout:\n${stdout.join("")}`,
      `stderr:\n${stderr.join("")}`,
    ].join("\n");
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
  } finally {
    stopChild(child);
    await closeServer(provider);
    rmSync(dir, { force: true, recursive: true });
  }
});

test("keeps provider conversation history across turns and service restarts", { timeout: 10_000 }, async () => {
  const providerBodies: unknown[] = [];
  const provider = http.createServer(async (request, response) => {
    if (maybeHandleProviderModelsRequest(request, response)) {
      return;
    }

    providerBodies.push(JSON.parse(await readBody(request)));
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
    });
    const content = providerBodies.length === 1 ? "first answer" : "second answer";
    response.write(`data: ${JSON.stringify({ choices: [{ delta: { content } }] })}\n\n`);
    response.write("data: [DONE]\n\n");
    response.end();
  });
  const providerPort = await listenOnLoopback(provider);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-agent-history-"));
  const dbPath = join(dir, "local.sqlite");
  const stdout: string[] = [];
  const stderr: string[] = [];
  let firstChild: ChildProcessWithoutNullStreams | undefined;
  let secondChild: ChildProcessWithoutNullStreams | undefined;

  async function startService(): Promise<{ child: ChildProcessWithoutNullStreams; port: number }> {
    const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
    const servicePort = await listenOnLoopback(serviceProbe);
    await closeServer(serviceProbe);
    const child = spawn(process.execPath, ["dist/server.js"], {
      cwd: process.cwd(),
      env: {
        ...process.env,
        PORT: String(servicePort),
        OPENAI_API_KEY: "sk-local-test",
        OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
        OPENAI_MODEL: "mock-model",
        LOG_LEVEL: "error",
        LOCAL_GRAPHQL_DB_PATH: dbPath,
      },
    });
    child.stdout.on("data", (chunk) => stdout.push(String(chunk)));
    child.stderr.on("data", (chunk) => stderr.push(String(chunk)));
    await waitForHealth(servicePort, child);
    return { child, port: servicePort };
  }

  try {
    const firstService = await startService();
    firstChild = firstService.child;
    const firstResponse = await fetch(`http://127.0.0.1:${firstService.port}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequest("first prompt", "conversation-history")),
    });
    assert.equal(firstResponse.status, 200);
    await firstResponse.text();
    stopChild(firstChild);
    firstChild = undefined;

    const secondService = await startService();
    secondChild = secondService.child;
    const secondResponse = await fetch(`http://127.0.0.1:${secondService.port}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequest("second prompt", "conversation-history")),
    });
    assert.equal(secondResponse.status, 200);
    await secondResponse.text();

    assert.equal(providerBodies.length, 2);
    assert.deepEqual(providerNonSystemMessages(providerBodies[1]), [
      { role: "user", content: "first prompt" },
      { role: "assistant", content: "first answer" },
      { role: "user", content: "second prompt" },
    ]);
  } catch (error) {
    const diagnostics = [
      `stdout:\n${stdout.join("")}`,
      `stderr:\n${stderr.join("")}`,
    ].join("\n");
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
  } finally {
    if (firstChild) {
      stopChild(firstChild);
    }
    if (secondChild) {
      stopChild(secondChild);
    }
    await closeServer(provider);
    rmSync(dir, { force: true, recursive: true });
  }
});

test("compacts local conversation state and reports lower context usage for summarize requests", { timeout: 10_000 }, async () => {
  const providerBodies: unknown[] = [];
  const provider = http.createServer(async (request, response) => {
    if (maybeHandleProviderModelsRequest(request, response)) {
      return;
    }

    providerBodies.push(JSON.parse(await readBody(request)));
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
    });
    const content = providerBodies.length === 1
      ? "first answer"
      : providerBodies.length === 2
        ? "summary of prior exchange"
        : "after compact answer";
    response.write(`data: ${JSON.stringify({ choices: [{ delta: { content } }] })}\n\n`);
    response.write("data: [DONE]\n\n");
    response.end();
  });
  const providerPort = await listenOnLoopback(provider);

  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-agent-compact-"));
  const stdout: string[] = [];
  const stderr: string[] = [];
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(servicePort),
      OPENAI_API_KEY: "sk-local-test",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "mock-model",
      LOG_LEVEL: "error",
      LOCAL_GRAPHQL_DB_PATH: join(dir, "local.sqlite"),
    },
  });
  child.stdout.on("data", (chunk) => stdout.push(String(chunk)));
  child.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  try {
    await waitForHealth(servicePort, child);

    const firstResponse = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequest("first prompt", "conversation-compact")),
    });
    assert.equal(firstResponse.status, 200);
    await firstResponse.text();

    const compactResponse = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpSummarizeRequest("conversation-compact")),
    });
    assert.equal(compactResponse.status, 200);
    const compactEvents = (await compactResponse.text()).split("\n\n").filter(Boolean);

    const summarizationRequestMessages = providerNonSystemMessages(providerBodies[1]) as Array<{ role?: string; content?: string }>;
    assert.equal(summarizationRequestMessages.length, 3);
    assert.deepEqual(summarizationRequestMessages.slice(0, 2), [
      { role: "user", content: "first prompt" },
      { role: "assistant", content: "first answer" },
    ]);
    assert.equal(summarizationRequestMessages[2]?.role, "user");
    assert.match(summarizationRequestMessages[2]?.content ?? "", /Summarize the conversation so far/);

    const summaryAction = compactEvents
      .map(sseResponseEvent)
      .flatMap((event) => event.type.case === "clientActions" ? event.type.value.actions : [])
      .map((clientAction) => clientAction.action)
      .find((action) => action.case === "addMessagesToTask" && action.value.messages[0]?.message.case === "summarization");
    assert.ok(summaryAction);

    const compactFinished = sseResponseEvent(compactEvents[compactEvents.length - 1]);
    assert.equal(compactFinished.type.case, "finished");
    assert.equal(compactFinished.type.value.conversationUsageMetadata?.summarized, true);
    assert.ok((compactFinished.type.value.conversationUsageMetadata?.contextWindowUsage ?? 1) < 0.01);

    const followUpResponse = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequest("after compact", "conversation-compact")),
    });
    assert.equal(followUpResponse.status, 200);
    await followUpResponse.text();

    assert.equal(providerBodies.length, 3);
    assert.deepEqual(providerNonSystemMessages(providerBodies[2]), [
      { role: "user", content: "after compact" },
    ]);
    const followUpSystemPrompt = providerSystemMessages(providerBodies[2])
      .map((message) => String(message.content ?? ""))
      .join("\n");
    assert.match(followUpSystemPrompt, /The conversation before this point was compacted/);
    assert.match(followUpSystemPrompt, /summary of prior exchange/);
    assert.doesNotMatch(followUpSystemPrompt, /first prompt/);
  } catch (error) {
    const diagnostics = [
      `stdout:\n${stdout.join("")}`,
      `stderr:\n${stderr.join("")}`,
    ].join("\n");
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
  } finally {
    stopChild(child);
    await closeServer(provider);
    rmSync(dir, { force: true, recursive: true });
  }
});

test("translates an OpenAI read_files tool call into Warp SSE events", { timeout: 10_000 }, async () => {
  const providerBodies: unknown[] = [];
  const provider = http.createServer(async (request, response) => {
    if (maybeHandleProviderModelsRequest(request, response)) {
      return;
    }

    const providerBody = JSON.parse(await readBody(request));
    providerBodies.push(providerBody);
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
    });
    if (providerBodies.length === 1) {
      response.write(`data: ${JSON.stringify({
        choices: [
          {
            delta: {
              tool_calls: [
                {
                  index: 0,
                  id: "call-read-files",
                  type: "function",
                  function: {
                    name: "read_files",
                    arguments: JSON.stringify({ files: [{ name: "src/main.ts" }] }),
                  },
                },
              ],
            },
          },
        ],
      })}\n\n`);
    } else {
      response.write('data: {"choices":[{"delta":{"content":"used file contents"}}]}\n\n');
    }
    response.write("data: [DONE]\n\n");
    response.end();
  });
  const providerPort = await listenOnLoopback(provider);

  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-agent-tools-"));
  const stdout: string[] = [];
  const stderr: string[] = [];
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(servicePort),
      OPENAI_API_KEY: "sk-local-test",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "mock-model",
      LOG_LEVEL: "error",
      LOCAL_GRAPHQL_DB_PATH: join(dir, "local.sqlite"),
    },
  });
  child.stdout.on("data", (chunk) => stdout.push(String(chunk)));
  child.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  try {
    await waitForHealth(servicePort, child);
    const response = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequest("read src/main.ts", "conversation-tools")),
    });

    assert.equal(response.status, 200);
    const streamText = await response.text();
    const events = streamText.split("\n\n").filter(Boolean);

    assert.equal((providerBodies[0] as { tool_choice?: unknown }).tool_choice, "auto");
    assert.equal(Array.isArray((providerBodies[0] as { tools?: unknown }).tools), true);
    assert.equal(events.length, 4);
    assert.ok(events.every((event) => /^data: [-_A-Za-z0-9]+=*$/.test(event)));

    const followUpResponse = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpReadFilesResultRequest({
        conversationId: "conversation-tools",
        toolCallId: "call-read-files",
        filePath: "src/main.ts",
        content: "console.log('tool result');",
      })),
    });
    assert.equal(followUpResponse.status, 200);
    const followUpEvents = (await followUpResponse.text()).split("\n\n").filter(Boolean);
    assert.equal(followUpEvents.length, 4);
    assert.deepEqual(providerNonSystemMessages(providerBodies[1]), [
      { role: "user", content: "read src/main.ts" },
      {
        role: "assistant",
        content: "",
        tool_calls: [
          {
            id: "call-read-files",
            type: "function",
            function: {
              name: "read_files",
              arguments: JSON.stringify({ files: [{ name: "src/main.ts" }] }),
            },
          },
        ],
      },
      {
        role: "tool",
        tool_call_id: "call-read-files",
        content: "File: src/main.ts\nconsole.log('tool result');",
      },
    ]);
  } catch (error) {
    const diagnostics = [
      `stdout:\n${stdout.join("")}`,
      `stderr:\n${stderr.join("")}`,
    ].join("\n");
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
  } finally {
    stopChild(child);
    await closeServer(provider);
    rmSync(dir, { force: true, recursive: true });
  }
});

test("translates all supported OpenAI tool calls into Warp SSE events", { timeout: 10_000 }, async () => {
  const toolCalls = [
    {
      id: "call-glob",
      function: {
        name: "file_glob",
        arguments: JSON.stringify({ patterns: ["*.ts"], search_dir: "src" }),
      },
    },
    {
      id: "call-grep",
      function: {
        name: "grep",
        arguments: JSON.stringify({ queries: ["TODO"], path: "." }),
      },
    },
    {
      id: "call-search",
      function: {
        name: "search_codebase",
        arguments: JSON.stringify({ query: "auth flow", path_filters: ["src"] }),
      },
    },
    {
      id: "call-shell",
      function: {
        name: "run_shell_command",
        arguments: JSON.stringify({ command: "pwd", is_read_only: true }),
      },
    },
    {
      id: "call-apply",
      function: {
        name: "apply_file_diffs",
        arguments: JSON.stringify({
          summary: "edit file",
          diffs: [{ file_path: "src/main.ts", search: "old", replace: "new" }],
        }),
      },
    },
    {
      id: "call-plan",
      function: {
        name: "suggest_plan",
        arguments: JSON.stringify({ summary: "plan", tasks: [{ description: "Do work" }] }),
      },
    },
    {
      id: "call-mcp-resource",
      function: {
        name: "read_mcp_resource",
        arguments: JSON.stringify({ uri: "mcp://local/resource", server_id: "local-server" }),
      },
    },
    {
      id: "call-mcp-tool",
      function: {
        name: "call_mcp_tool",
        arguments: JSON.stringify({ name: "list_items", server_id: "local-server", args: { limit: 2 } }),
      },
    },
  ];
  const provider = http.createServer(async (request, response) => {
    if (maybeHandleProviderModelsRequest(request, response)) {
      return;
    }

    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
    });
    response.write(`data: ${JSON.stringify({
      choices: [
        {
          delta: {
            tool_calls: toolCalls.map((toolCall, index) => ({
              index,
              id: toolCall.id,
              type: "function",
              function: toolCall.function,
            })),
          },
        },
      ],
    })}\n\n`);
    response.write("data: [DONE]\n\n");
    response.end();
  });
  const providerPort = await listenOnLoopback(provider);

  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-agent-all-tools-"));
  const stdout: string[] = [];
  const stderr: string[] = [];
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(servicePort),
      OPENAI_API_KEY: "sk-local-test",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "mock-model",
      LOG_LEVEL: "error",
      LOCAL_GRAPHQL_DB_PATH: join(dir, "local.sqlite"),
    },
  });
  child.stdout.on("data", (chunk) => stdout.push(String(chunk)));
  child.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  try {
    await waitForHealth(servicePort, child);
    const response = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequest("use tools")),
    });

    assert.equal(response.status, 200);
    const events = (await response.text()).split("\n\n").filter(Boolean);
    assert.equal(events.length, 11);
    assert.ok(events.every((event) => /^data: [-_A-Za-z0-9]+=*$/.test(event)));
  } catch (error) {
    const diagnostics = [
      `stdout:\n${stdout.join("")}`,
      `stderr:\n${stderr.join("")}`,
    ].join("\n");
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
  } finally {
    stopChild(child);
    await closeServer(provider);
    rmSync(dir, { force: true, recursive: true });
  }
});

test("translates provider-native MCP tool calls into Warp MCP tool events", { timeout: 10_000 }, async () => {
  const providerBodies: unknown[] = [];
  const provider = http.createServer(async (request, response) => {
    if (maybeHandleProviderModelsRequest(request, response)) {
      return;
    }

    providerBodies.push(JSON.parse(await readBody(request)));
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
    });
    response.write(`data: ${JSON.stringify({
      choices: [
        {
          delta: {
            tool_calls: [
              {
                index: 0,
                id: "call-linear-list-issues",
                type: "function",
                function: {
                  name: "list_issues",
                  arguments: JSON.stringify({ created_after: "2026-04-30" }),
                },
              },
            ],
          },
        },
      ],
    })}\n\n`);
    response.write("data: [DONE]\n\n");
    response.end();
  });
  const providerPort = await listenOnLoopback(provider);

  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-agent-native-mcp-"));
  const stdout: string[] = [];
  const stderr: string[] = [];
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(servicePort),
      OPENAI_API_KEY: "sk-local-test",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "mock-model",
      LOG_LEVEL: "error",
      LOCAL_GRAPHQL_DB_PATH: join(dir, "local.sqlite"),
      LOCAL_MULTI_AGENT_SYSTEM_PROMPT: "local endpoint prompt",
    },
  });
  child.stdout.on("data", (chunk) => stdout.push(String(chunk)));
  child.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  try {
    await waitForHealth(servicePort, child);
    const response = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequestWithMcpTool("any linear issues from today?", {
        toolName: "list_issues",
        toolDescription: "List Linear issues",
        serverName: "Linear",
        serverId: "linear-server",
      })),
    });

    assert.equal(response.status, 200);
    const events = (await response.text()).split("\n\n").filter(Boolean);

    assert.equal(providerBodies.length, 1);
    const systemMessages = providerSystemMessages(providerBodies[0]);
    assert.equal(systemMessages.length, 1);
    const systemPrompt = String(systemMessages[0]?.content ?? "");
    assert.match(systemPrompt, /always use the provided call_mcp_tool function/);
    assert.match(systemPrompt, /list_issues/);
    assert.match(systemPrompt, /local endpoint prompt/);
    assert.equal(events.length, 4);
    const toolCallBytes = sseEventBytes(events[2]);
    assert.equal(toolCallBytes.includes("call-linear-list-issues"), true);
    assert.equal(toolCallBytes.includes("list_issues"), true);
    assert.equal(toolCallBytes.includes("linear-server"), true);
    assert.equal(toolCallBytes.includes("created_after"), true);
  } catch (error) {
    const diagnostics = [
      `stdout:\n${stdout.join("")}`,
      `stderr:\n${stderr.join("")}`,
    ].join("\n");
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
  } finally {
    stopChild(child);
    await closeServer(provider);
    rmSync(dir, { force: true, recursive: true });
  }
});

test("does not guess provider-native MCP tool calls when names are ambiguous", { timeout: 10_000 }, async () => {
  const provider = http.createServer(async (request, response) => {
    if (maybeHandleProviderModelsRequest(request, response)) {
      return;
    }

    await readBody(request);
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
    });
    response.write(`data: ${JSON.stringify({
      choices: [
        {
          delta: {
            tool_calls: [
              {
                index: 0,
                id: "call-ambiguous-list-issues",
                type: "function",
                function: {
                  name: "list_issues",
                  arguments: JSON.stringify({ created_after: "2026-04-30" }),
                },
              },
            ],
          },
        },
      ],
    })}\n\n`);
    response.write("data: [DONE]\n\n");
    response.end();
  });
  const providerPort = await listenOnLoopback(provider);

  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

  const dir = mkdtempSync(join(tmpdir(), "warp-local-agent-ambiguous-mcp-"));
  const stdout: string[] = [];
  const stderr: string[] = [];
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      PORT: String(servicePort),
      OPENAI_API_KEY: "sk-local-test",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "mock-model",
      LOG_LEVEL: "error",
      LOCAL_GRAPHQL_DB_PATH: join(dir, "local.sqlite"),
    },
  });
  child.stdout.on("data", (chunk) => stdout.push(String(chunk)));
  child.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  try {
    await waitForHealth(servicePort, child);
    const response = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequestWithDuplicateMcpTools("any linear issues from today?")),
    });

    assert.equal(response.status, 200);
    const events = (await response.text()).split("\n\n").filter(Boolean);

    assert.equal(events.length, 3);
    assert.equal(sseEventBytes(events[2]).includes("Unsupported provider tool call: list_issues"), true);
  } catch (error) {
    const diagnostics = [
      `stdout:\n${stdout.join("")}`,
      `stderr:\n${stderr.join("")}`,
    ].join("\n");
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`);
  } finally {
    stopChild(child);
    await closeServer(provider);
    rmSync(dir, { force: true, recursive: true });
  }
});
