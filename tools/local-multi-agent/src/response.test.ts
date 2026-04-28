import assert from "node:assert/strict";
import test from "node:test";
import { collectAssistantOutput } from "./response.js";

async function* chunks(values: string[]): AsyncGenerator<string> {
  yield* values;
}

test("collects provider stream chunks into one assistant output", async () => {
  await assert.doesNotReject(async () => {
    assert.equal(await collectAssistantOutput(chunks(["hello", " ", "warp"])), "hello warp");
  });
});

test("rejects empty provider streams", async () => {
  await assert.rejects(
    () => collectAssistantOutput(chunks([])),
    /returned no assistant content/,
  );
});
