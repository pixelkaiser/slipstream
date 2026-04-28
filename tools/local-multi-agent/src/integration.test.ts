import assert from "node:assert/strict";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { setTimeout as delay } from "node:timers/promises";
import test from "node:test";

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

test("serves a Warp multi-agent request through a mock OpenAI-compatible stream", { timeout: 10_000 }, async () => {
  let providerPath: string | undefined;
  let providerBody: unknown;
  const provider = http.createServer(async (request, response) => {
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
    assert.deepEqual((providerBody as { messages?: unknown }).messages, [{ role: "user", content: "hello provider" }]);
    assert.equal((providerBody as { stream?: unknown }).stream, true);
    assert.equal((providerBody as { tool_choice?: unknown }).tool_choice, "auto");
    assert.equal(Array.isArray((providerBody as { tools?: unknown }).tools), true);
    assert.deepEqual(
      ((providerBody as { tools?: Array<{ function?: { name?: string } }> }).tools ?? []).map((tool) => tool.function?.name),
      ["read_files", "file_glob", "grep", "search_codebase", "run_shell_command", "apply_file_diffs", "suggest_plan"],
    );
    assert.equal(events.length, 4);
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
  }
});

test("passes attached selected text context to the provider", { timeout: 10_000 }, async () => {
  let providerBody: unknown;
  const provider = http.createServer(async (request, response) => {
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

    const messages = (providerBody as { messages?: Array<{ content?: string }> }).messages ?? [];
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
  }
});

test("keeps provider conversation history across turns", { timeout: 10_000 }, async () => {
  const providerBodies: unknown[] = [];
  const provider = http.createServer(async (_request, response) => {
    providerBodies.push(JSON.parse(await readBody(_request)));
    response.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
    });
    const content = providerBodies.length === 1 ? "first answer" : "second answer";
    response.write(`data: ${JSON.stringify({ choices: [{ delta: { content } }] })}\n\n`);
    response.write("data: [DONE]\n\n");
    response.end();
  });
  const providerPort = await listenOnLoopback(provider);

  const serviceProbe = http.createServer((_request, response) => response.end("reserved"));
  const servicePort = await listenOnLoopback(serviceProbe);
  await closeServer(serviceProbe);

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
      body: Buffer.from(warpPromptRequest("first prompt", "conversation-history")),
    });
    assert.equal(firstResponse.status, 200);
    await firstResponse.text();

    const secondResponse = await fetch(`http://127.0.0.1:${servicePort}/ai/multi-agent`, {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
      },
      body: Buffer.from(warpPromptRequest("second prompt", "conversation-history")),
    });
    assert.equal(secondResponse.status, 200);
    await secondResponse.text();

    assert.equal(providerBodies.length, 2);
    assert.deepEqual((providerBodies[1] as { messages?: unknown }).messages, [
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
    stopChild(child);
    await closeServer(provider);
  }
});

test("translates an OpenAI read_files tool call into Warp SSE events", { timeout: 10_000 }, async () => {
  const providerBodies: unknown[] = [];
  const provider = http.createServer(async (request, response) => {
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
    assert.deepEqual((providerBodies[1] as { messages?: unknown }).messages, [
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
  ];
  const provider = http.createServer(async (_request, response) => {
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
    assert.equal(events.length, 9);
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
  }
});
