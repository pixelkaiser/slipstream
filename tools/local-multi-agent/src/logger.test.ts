import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { log, logFilePath } from "./logger.js";

test("redacts sensitive log fields", () => {
  const originalLogLevel = process.env.LOG_LEVEL;
  const originalLogPath = process.env.LOCAL_SERVICE_LOG_PATH;
  const dir = mkdtempSync(join(tmpdir(), "warp-local-log-"));
  process.env.LOCAL_SERVICE_LOG_PATH = join(dir, "service.log");
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
    rmSync(dir, { force: true, recursive: true });
    if (originalLogLevel == null) {
      delete process.env.LOG_LEVEL;
    } else {
      process.env.LOG_LEVEL = originalLogLevel;
    }
    if (originalLogPath == null) {
      delete process.env.LOCAL_SERVICE_LOG_PATH;
    } else {
      process.env.LOCAL_SERVICE_LOG_PATH = originalLogPath;
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

test("persists JSON log lines to configured local service log path", () => {
  const originalLogLevel = process.env.LOG_LEVEL;
  const originalLogPath = process.env.LOCAL_SERVICE_LOG_PATH;
  const dir = mkdtempSync(join(tmpdir(), "warp-local-log-"));
  const path = join(dir, "nested", "service.log");
  process.env.LOG_LEVEL = "debug";
  process.env.LOCAL_SERVICE_LOG_PATH = path;
  const originalLog = console.log;
  console.log = () => {};

  try {
    log("info", "persisted_event", {
      token: "secret-token",
      operationName: "CreateGenericStringObject",
    });

    assert.equal(logFilePath(), path);
    const logLines = readFileSync(path, "utf8").trim().split("\n");
    assert.equal(logLines.length, 1);
    assert.deepEqual(JSON.parse(logLines[0] ?? ""), {
      ts: JSON.parse(logLines[0] ?? "").ts,
      level: "info",
      event: "persisted_event",
      token: "[redacted]",
      operationName: "CreateGenericStringObject",
    });
  } finally {
    console.log = originalLog;
    rmSync(dir, { force: true, recursive: true });
    if (originalLogLevel == null) {
      delete process.env.LOG_LEVEL;
    } else {
      process.env.LOG_LEVEL = originalLogLevel;
    }
    if (originalLogPath == null) {
      delete process.env.LOCAL_SERVICE_LOG_PATH;
    } else {
      process.env.LOCAL_SERVICE_LOG_PATH = originalLogPath;
    }
  }
});
