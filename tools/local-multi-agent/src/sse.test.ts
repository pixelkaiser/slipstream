import assert from "node:assert/strict";
import test from "node:test";
import { formatSseDataEvent } from "./sse.js";

test("formats protobuf bytes as a padded base64-url SSE data event", () => {
  assert.equal(formatSseDataEvent(Uint8Array.from([0xfb, 0xff])), "data: -_8=\n\n");
});
