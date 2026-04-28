import assert from "node:assert/strict";
import test from "node:test";
import {
  encodeAddAgentOutput,
  encodeAddReadFilesToolCall,
  encodeAppendAgentOutput,
  encodeCreateTask,
  decodeWarpRequest,
  encodeAgentOutput,
  encodeBase64Url,
  encodeStreamFinishedDone,
  encodeStreamInit,
} from "./protobuf.js";

function hex(value: string): Uint8Array {
  return Uint8Array.from(Buffer.from(value.replace(/\s+/g, ""), "hex"));
}

function encodeVarint(value: number): Uint8Array {
  const bytes: number[] = [];
  let remaining = value;
  while (remaining >= 0x80) {
    bytes.push((remaining & 0x7f) | 0x80);
    remaining >>= 7;
  }
  bytes.push(remaining);
  return Uint8Array.from(bytes);
}

function lengthDelimitedField(fieldNumber: number, value: Uint8Array): Uint8Array {
  assert.ok(value.length < 128, "test helper only supports one-byte lengths");
  return Uint8Array.from([...encodeVarint((fieldNumber << 3) | 2), value.length, ...value]);
}

function stringField(fieldNumber: number, value: string): Uint8Array {
  return lengthDelimitedField(fieldNumber, Buffer.from(value));
}

test("decodes the fields needed from a Warp request", () => {
  const task = hex("0a04726f6f74");
  const taskContext = Uint8Array.from([0x0a, task.length, ...task]);
  const userQuery = hex("0a0b68656c6c6f2077617270");
  const userInput = Uint8Array.from([0x0a, userQuery.length, ...userQuery]);
  const userInputs = Uint8Array.from([0x0a, userInput.length, ...userInput]);
  const input = Uint8Array.from([0x32, userInputs.length, ...userInputs]);
  const apiKeys = hex("120a736b2d74657374696e67");
  const settings = Uint8Array.from([0x92, 0x01, apiKeys.length, ...apiKeys]);
  const metadata = hex("0a0c636f6e766572736174696f6e");
  const request = Uint8Array.from([
    0x0a,
    taskContext.length,
    ...taskContext,
    0x12,
    input.length,
    ...input,
    0x1a,
    settings.length,
    ...settings,
    0x22,
    metadata.length,
    ...metadata,
  ]);

  assert.deepEqual(
    {
      ...decodeWarpRequest(request),
      requestId: "<generated>",
    },
    {
      conversationId: "conversation",
      requestId: "<generated>",
      rootTaskId: "root",
      shouldCreateRootTask: false,
      prompt: "hello warp",
      toolResults: [],
      openAiApiKey: "sk-testing",
      model: undefined,
    },
  );
});

test("extracts supported prompt input variants", () => {
  const cases = [
    {
      name: "deprecated user query",
      input: lengthDelimitedField(2, stringField(1, "legacy prompt")),
      expected: "legacy prompt",
    },
    {
      name: "query with canned response",
      input: lengthDelimitedField(4, stringField(1, "canned prompt")),
      expected: "canned prompt",
    },
    {
      name: "invoke skill user query",
      input: lengthDelimitedField(17, lengthDelimitedField(2, stringField(1, "skill prompt"))),
      expected: "skill prompt",
    },
    {
      name: "summarize conversation",
      input: lengthDelimitedField(13, stringField(1, "summarize prompt")),
      expected: "summarize prompt",
    },
  ];

  for (const item of cases) {
    const request = lengthDelimitedField(2, item.input);
    assert.equal(decodeWarpRequest(request).prompt, item.expected, item.name);
  }
});

test("marks requests without a task context as needing root task creation", () => {
  const userQuery = hex("0a0b68656c6c6f2077617270");
  const userInput = Uint8Array.from([0x0a, userQuery.length, ...userQuery]);
  const userInputs = Uint8Array.from([0x0a, userInput.length, ...userInput]);
  const input = Uint8Array.from([0x32, userInputs.length, ...userInputs]);
  const request = Uint8Array.from([0x12, input.length, ...input]);

  const decoded = decodeWarpRequest(request);

  assert.equal(decoded.shouldCreateRootTask, true);
  assert.notEqual(decoded.rootTaskId, "root");
});

test("decodes read_files tool call results from user inputs", () => {
  const fileContent = Buffer.concat([
    stringField(1, "src/main.ts"),
    stringField(2, "console.log('warp');"),
  ]);
  const textFilesSuccess = lengthDelimitedField(1, fileContent);
  const readFilesResult = lengthDelimitedField(1, textFilesSuccess);
  const toolCallResult = Buffer.concat([
    stringField(1, "call-read-files"),
    lengthDelimitedField(3, readFilesResult),
  ]);
  const userInput = lengthDelimitedField(2, toolCallResult);
  const userInputs = lengthDelimitedField(1, userInput);
  const input = lengthDelimitedField(6, userInputs);
  const request = lengthDelimitedField(2, input);

  const decoded = decodeWarpRequest(request);

  assert.deepEqual(decoded.toolResults, [
    {
      toolCallId: "call-read-files",
      files: [{ filePath: "src/main.ts", content: "console.log('warp');" }],
      error: undefined,
    },
  ]);
  assert.match(decoded.prompt, /src\/main\.ts/);
  assert.match(decoded.prompt, /console\.log/);
});

test("encodes response events as protobuf payloads", () => {
  assert.equal(encodeBase64Url(encodeStreamInit("c", "r")), "CgYKAWMSAXI=");
  assert.equal(encodeBase64Url(encodeStreamFinishedDone()), "GgISAA==");
  assert.ok(encodeAgentOutput({ taskId: "root", requestId: "req", text: "ok" }).length > 0);
  assert.ok(encodeCreateTask({ taskId: "root", description: "hello" }).length > 0);
  const toolCall = encodeAddReadFilesToolCall({
    messageId: "message",
    taskId: "root",
    requestId: "request",
    toolCallId: "call-read-files",
    files: [{ name: "src/main.ts", lineRanges: [{ start: 1, end: 10 }] }],
  });
  assert.ok(Buffer.from(toolCall).includes("call-read-files"));
  assert.ok(Buffer.from(toolCall).includes("src/main.ts"));
});

test("encodes streaming agent output append events with the text field mask", () => {
  const initial = encodeAddAgentOutput({
    messageId: "message",
    taskId: "root",
    requestId: "request",
    text: "",
  });
  const append = encodeAppendAgentOutput({
    messageId: "message",
    taskId: "root",
    requestId: "request",
    text: "chunk",
  });

  assert.ok(initial.length > 0);
  assert.ok(Buffer.from(append).includes("message.agent_output.text"));
});
