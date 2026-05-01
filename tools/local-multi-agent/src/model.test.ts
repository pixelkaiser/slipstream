import assert from "node:assert/strict";
import test from "node:test";
import { defaultModel, resolveProviderModel } from "./model.js";

test("uses OPENAI_MODEL as the fallback when no concrete model is requested", () => {
  assert.equal(
    resolveProviderModel({
      openAiModel: "provider-model",
    }),
    "provider-model",
  );
  assert.equal(
    resolveProviderModel({
      openAiModel: "provider-model",
      warpModel: "auto-unknown",
    }),
    "provider-model",
  );
});

test("maps Warp auto model IDs to the default provider model", () => {
  assert.equal(resolveProviderModel({ warpModel: "auto-efficient" }), defaultModel);
  assert.equal(resolveProviderModel({ warpModel: "auto-coding" }), defaultModel);
});

test("uses LOCAL_MODEL_ALIASES to override built-in model mappings", () => {
  assert.equal(
    resolveProviderModel({
      warpModel: "auto-efficient",
      localModelAliases: JSON.stringify({
        "auto-efficient": "custom-fast-model",
      }),
    }),
    "custom-fast-model",
  );
});

test("passes through provider-native model IDs", () => {
  assert.equal(resolveProviderModel({ warpModel: "Qwen/Qwen3.6-27B-FP8" }), defaultModel);
  assert.equal(
    resolveProviderModel({
      openAiModel: "fallback-model",
      warpModel: "provider-native-model",
    }),
    "provider-native-model",
  );
});
