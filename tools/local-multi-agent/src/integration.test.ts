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

function warpPromptRequest(prompt: string): Uint8Array {
  const deprecatedUserQuery = lengthDelimitedField(2, stringField(1, prompt));
  return lengthDelimitedField(2, deprecatedUserQuery);
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
    assert.deepEqual(providerBody, {
      model: "mock-model",
      messages: [{ role: "user", content: "hello provider" }],
      temperature: 0.2,
      stream: true,
    });
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
