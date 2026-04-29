import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { IntegrationStore } from "./integrationStore.js";

function withStore(fn: (store: IntegrationStore, dbPath: string) => void): void {
  const dir = mkdtempSync(join(tmpdir(), "warp-local-graphql-"));
  const dbPath = join(dir, "test.sqlite");
  const store = new IntegrationStore(dbPath);
  try {
    fn(store, dbPath);
  } finally {
    store.close();
    rmSync(dir, { force: true, recursive: true });
  }
}

test("creates and lists an integration config", () => {
  withStore((store) => {
    const record = store.createOrUpdate({
      integrationType: "Linear",
      isUpdate: false,
      enabled: true,
      config: {
        environmentUid: "env-1",
        basePrompt: "Handle tickets locally.",
        modelId: "auto-coding",
        mcpServersJson: JSON.stringify({
          linear: { url: "http://127.0.0.1:3000/mcp" },
        }),
        workerHost: "local-host",
      },
    });

    assert.equal(record.providerSlug, "linear");
    assert.equal(record.environmentUid, "env-1");
    assert.equal(record.basePrompt, "Handle tickets locally.");
    assert.equal(record.modelId, "auto-coding");
    assert.equal(record.workerHost, "local-host");
    assert.deepEqual(JSON.parse(record.mcpServersJson), {
      linear: { url: "http://127.0.0.1:3000/mcp" },
    });

    const listed = store.list(["linear", "slack"]);
    assert.equal(listed[0]?.record?.providerSlug, "linear");
    assert.equal(listed[1]?.record, undefined);
  });
});

test("updates patch MCP servers, applies removals, and clears nullable fields", () => {
  withStore((store) => {
    store.createOrUpdate({
      integrationType: "linear",
      isUpdate: false,
      enabled: true,
      config: {
        environmentUid: "env-1",
        basePrompt: "Initial prompt",
        modelId: "auto",
        mcpServersJson: JSON.stringify({
          keep: { command: "keep" },
          remove: { command: "remove" },
        }),
      },
    });

    const updated = store.createOrUpdate({
      integrationType: "linear",
      isUpdate: true,
      enabled: true,
      config: {
        environmentUid: "",
        basePrompt: "",
        modelId: "auto-coding",
        mcpServersJson: JSON.stringify({
          add: { command: "add" },
        }),
        removeMcpServerNames: ["remove"],
      },
    });

    assert.equal(updated.environmentUid, null);
    assert.equal(updated.basePrompt, null);
    assert.equal(updated.modelId, "auto-coding");
    assert.deepEqual(JSON.parse(updated.mcpServersJson), {
      keep: { command: "keep" },
      add: { command: "add" },
    });
  });
});

test("finds providers using an environment and persists across reopen", () => {
  const dir = mkdtempSync(join(tmpdir(), "warp-local-graphql-"));
  const dbPath = join(dir, "test.sqlite");
  const first = new IntegrationStore(dbPath);
  try {
    first.createOrUpdate({
      integrationType: "linear",
      isUpdate: false,
      enabled: true,
      config: { environmentUid: "env-1" },
    });
    first.createOrUpdate({
      integrationType: "slack",
      isUpdate: false,
      enabled: true,
      config: { environmentUid: "env-2" },
    });
  } finally {
    first.close();
  }

  const second = new IntegrationStore(dbPath);
  try {
    assert.deepEqual(second.providersUsingEnvironment("env-1"), ["linear"]);
    assert.equal(second.get("linear")?.environmentUid, "env-1");
  } finally {
    second.close();
    rmSync(dir, { force: true, recursive: true });
  }
});

test("persists AI conversation transcripts across reopen", () => {
  const dir = mkdtempSync(join(tmpdir(), "warp-local-graphql-"));
  const dbPath = join(dir, "test.sqlite");
  const messages = [
    { role: "user", content: "first prompt" },
    { role: "assistant", content: "first answer" },
  ];
  const first = new IntegrationStore(dbPath);
  try {
    first.upsertAiConversation("conversation-1", messages);
  } finally {
    first.close();
  }

  const second = new IntegrationStore(dbPath);
  try {
    assert.deepEqual(second.getAiConversation("conversation-1")?.messages, messages);
    assert.deepEqual(
      second.listAiConversations().map((conversation) => conversation.conversationId),
      ["conversation-1"],
    );
  } finally {
    second.close();
    rmSync(dir, { force: true, recursive: true });
  }
});
