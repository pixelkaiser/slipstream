import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { defaultModel } from "./model.js";

type ConfigSchema = {
  version: number;
  defaults: Record<string, unknown>;
};

function readConfigSchema(): ConfigSchema {
  return JSON.parse(
    readFileSync(new URL("../config-schema.json", import.meta.url), "utf8"),
  ) as ConfigSchema;
}

test("Warp config schema defaults match sidecar env defaults", () => {
  const schema = readConfigSchema();

  assert.equal(schema.version, 1);
  assert.equal(schema.defaults.HOST, "127.0.0.1");
  assert.equal(schema.defaults.PORT, 8787);
  assert.equal(schema.defaults.OPENAI_MODEL, "");
  assert.equal(schema.defaults.LOCAL_MODEL_LIST, defaultModel);
  assert.equal(schema.defaults.LOCAL_ENABLE_TOOLS, true);
  assert.equal(schema.defaults.LOCAL_MAX_HISTORY_MESSAGES, 80);
  assert.equal(schema.defaults.LOG_LEVEL, "info");
  assert.equal(typeof schema.defaults.LOCAL_MULTI_AGENT_SYSTEM_PROMPT, "string");
  assert.notEqual(schema.defaults.LOCAL_MULTI_AGENT_SYSTEM_PROMPT, "");
});
