import assert from "node:assert/strict";
import test from "node:test";
import { log } from "./logger.js";

test("redacts sensitive log fields", () => {
  const originalLogLevel = process.env.LOG_LEVEL;
  process.env.LOG_LEVEL = "debug";
  const lines: string[] = [];
  const originalLog = console.log;
  console.log = (line?: unknown) => {
    lines.push(String(line));
  };

  try {
    log("info", "test_event", {
      apiKey: "sk-test",
      authorization: "Bearer secret",
      model: "model",
    });
  } finally {
    console.log = originalLog;
    if (originalLogLevel == null) {
      delete process.env.LOG_LEVEL;
    } else {
      process.env.LOG_LEVEL = originalLogLevel;
    }
  }

  assert.equal(lines.length, 1);
  assert.deepEqual(JSON.parse(lines[0]), {
    ts: JSON.parse(lines[0]).ts,
    level: "info",
    event: "test_event",
    apiKey: "[redacted]",
    authorization: "[redacted]",
    model: "model",
  });
});
