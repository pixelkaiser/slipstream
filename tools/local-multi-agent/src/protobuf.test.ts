import assert from "node:assert/strict";
import test from "node:test";
import {
  decodeWarpRequest,
  encodeAgentOutput,
  encodeBase64Url,
  encodeStreamFinishedDone,
  encodeStreamInit,
} from "./protobuf.js";

function hex(value: string): Uint8Array {
  return Uint8Array.from(Buffer.from(value.replace(/\s+/g, ""), "hex"));
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
      prompt: "hello warp",
      openAiApiKey: "sk-testing",
      model: undefined,
    },
  );
});

test("encodes response events as protobuf payloads", () => {
  assert.equal(encodeBase64Url(encodeStreamInit("c", "r")), "CgkKAWMSAXIaAWM");
  assert.equal(encodeBase64Url(encodeStreamFinishedDone()), "GgISAA");
  assert.ok(encodeAgentOutput({ taskId: "root", requestId: "req", text: "ok" }).length > 0);
});
