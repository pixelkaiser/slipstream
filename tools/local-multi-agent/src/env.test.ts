import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { loadDotEnv } from "./env.js";

test("loads dotenv values without overriding shell environment", () => {
  const dir = mkdtempSync(join(tmpdir(), "warp-local-env-"));
  const previousBaseUrl = process.env.OPENAI_BASE_URL;
  const previousModel = process.env.OPENAI_MODEL;
  const previousQuoted = process.env.LOCAL_TEST_QUOTED;

  try {
    process.env.OPENAI_BASE_URL = "http://shell.example/v1";
    delete process.env.OPENAI_MODEL;
    delete process.env.LOCAL_TEST_QUOTED;

    const envPath = join(dir, ".env");
    writeFileSync(envPath, [
      "OPENAI_BASE_URL=http://dotenv.example/v1",
      "OPENAI_MODEL=dotenv-model",
      "LOCAL_TEST_QUOTED=\"quoted value\"",
      "",
    ].join("\n"));

    loadDotEnv(envPath);

    assert.equal(process.env.OPENAI_BASE_URL, "http://shell.example/v1");
    assert.equal(process.env.OPENAI_MODEL, "dotenv-model");
    assert.equal(process.env.LOCAL_TEST_QUOTED, "quoted value");
  } finally {
    if (previousBaseUrl == null) {
      delete process.env.OPENAI_BASE_URL;
    } else {
      process.env.OPENAI_BASE_URL = previousBaseUrl;
    }
    if (previousModel == null) {
      delete process.env.OPENAI_MODEL;
    } else {
      process.env.OPENAI_MODEL = previousModel;
    }
    if (previousQuoted == null) {
      delete process.env.LOCAL_TEST_QUOTED;
    } else {
      process.env.LOCAL_TEST_QUOTED = previousQuoted;
    }
    rmSync(dir, { force: true, recursive: true });
  }
});
