import Database from "better-sqlite3";
import { randomUUID } from "node:crypto";

export type IntegrationConfigPatch = {
  basePrompt?: string | null;
  environmentUid?: string | null;
  mcpServersJson?: string | null;
  modelId?: string | null;
  removeMcpServerNames?: string[] | null;
  workerHost?: string | null;
};

export type CreateOrUpdateIntegrationInput = {
  config: IntegrationConfigPatch;
  enabled: boolean;
  integrationType: string;
  isUpdate: boolean;
};

export type IntegrationRecord = {
  providerSlug: string;
  enabled: boolean;
  environmentUid: string | null;
  basePrompt: string | null;
  modelId: string | null;
  mcpServersJson: string;
  workerHost: string | null;
  createdAt: string;
  updatedAt: string;
};

export type AiConversationRecord = {
  conversationId: string;
  messages: unknown[];
  createdAt: string;
  updatedAt: string;
};

export type GenericStringObjectInput = {
  clientId?: string | null;
  format: string;
  serializedModel: string;
  uniquenessKey?: { key: string; uniquePer: string } | null;
};

export type GenericStringObjectRecord = {
  uid: string;
  clientId: string | null;
  format: string;
  serializedModel: string;
  revisionTs: string;
  metadataLastUpdatedTs: string;
  permissionsLastUpdatedTs: string;
  createdAt: string;
  updatedAt: string;
};

type IntegrationRow = {
  provider_slug: string;
  enabled: 0 | 1;
  environment_uid: string | null;
  base_prompt: string | null;
  model_id: string | null;
  mcp_servers_json: string;
  worker_host: string | null;
  created_at: string;
  updated_at: string;
};

type AiConversationRow = {
  conversation_id: string;
  messages_json: string;
  created_at: string;
  updated_at: string;
};

type GenericStringObjectRow = {
  uid: string;
  client_id: string | null;
  format: string;
  serialized_model: string;
  revision_ts: string;
  metadata_last_updated_ts: string;
  permissions_last_updated_ts: string;
  created_at: string;
  updated_at: string;
};

function normalizeProviderSlug(value: string): string {
  const slug = value.trim().toLowerCase();
  if (!slug) {
    throw new Error("integration_type is required");
  }
  return slug;
}

function normalizeConversationId(value: string): string {
  const conversationId = value.trim();
  if (!conversationId) {
    throw new Error("conversation_id is required");
  }
  return conversationId;
}

function rowToRecord(row: IntegrationRow): IntegrationRecord {
  return {
    providerSlug: row.provider_slug,
    enabled: row.enabled === 1,
    environmentUid: row.environment_uid,
    basePrompt: row.base_prompt,
    modelId: row.model_id,
    mcpServersJson: row.mcp_servers_json,
    workerHost: row.worker_host,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
  };
}

function rowToAiConversationRecord(row: AiConversationRow): AiConversationRecord {
  const messages = JSON.parse(row.messages_json) as unknown;
  if (!Array.isArray(messages)) {
    throw new Error(`stored AI conversation ${row.conversation_id} does not contain a message array`);
  }

  return {
    conversationId: row.conversation_id,
    messages,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
  };
}

function rowToGenericStringObjectRecord(row: GenericStringObjectRow): GenericStringObjectRecord {
  return {
    uid: row.uid,
    clientId: row.client_id,
    format: inferGenericStringObjectFormat(row.serialized_model) ?? row.format,
    serializedModel: row.serialized_model,
    revisionTs: row.revision_ts,
    metadataLastUpdatedTs: row.metadata_last_updated_ts,
    permissionsLastUpdatedTs: row.permissions_last_updated_ts,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
  };
}

function normalizeGenericStringObjectFormat(value: string): string {
  const format = value.trim();
  if (!format) {
    throw new Error("generic string object format is required");
  }
  return format;
}

function inferGenericStringObjectFormat(serializedModel: string): string | undefined {
  try {
    const parsed = JSON.parse(serializedModel) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return undefined;
    }

    const model = parsed as Record<string, unknown>;
    if (Object.hasOwn(model, "storage_key")) {
      return "JsonPreference";
    }
    if (Object.hasOwn(model, "template") || Object.hasOwn(model, "json_template")) {
      return "JsonTemplatableMCPServer";
    }
    if (
      Object.hasOwn(model, "is_default_profile")
      || Object.hasOwn(model, "apply_code_diffs")
      || Object.hasOwn(model, "mcp_allowlist")
      || Object.hasOwn(model, "mcp_denylist")
    ) {
      return "JsonAIExecutionProfile";
    }
    if (Object.hasOwn(model, "transport_type") || Object.hasOwn(model, "command")) {
      return "JsonMCPServer";
    }
  } catch {
    return undefined;
  }

  return undefined;
}

function applyNullableString(current: string | null, next: string | null | undefined, isUpdate: boolean): string | null {
  if (next == null) {
    return isUpdate ? current : null;
  }
  return next === "" ? null : next;
}

function parseMcpMap(json: string | null | undefined): Record<string, unknown> | undefined {
  if (json == null || json.trim() === "") {
    return undefined;
  }

  const parsed = JSON.parse(json) as unknown;
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("mcp_servers_json must encode a JSON object");
  }

  return parsed as Record<string, unknown>;
}

function mergeMcpServers(params: {
  currentJson: string | null | undefined;
  patchJson: string | null | undefined;
  removeNames: string[] | null | undefined;
  isUpdate: boolean;
}): string {
  const current = params.isUpdate ? parseMcpMap(params.currentJson) ?? {} : {};
  const patch = parseMcpMap(params.patchJson);
  const merged = params.isUpdate ? { ...current, ...(patch ?? {}) } : (patch ?? {});

  for (const name of params.removeNames ?? []) {
    delete merged[name];
  }

  return JSON.stringify(merged);
}

export class IntegrationStore {
  private readonly db: Database.Database;

  constructor(path: string) {
    this.db = new Database(path);
    this.db.pragma("journal_mode = WAL");
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS integrations (
        provider_slug TEXT PRIMARY KEY NOT NULL,
        enabled INTEGER NOT NULL,
        environment_uid TEXT,
        base_prompt TEXT,
        model_id TEXT,
        mcp_servers_json TEXT NOT NULL DEFAULT '{}',
        worker_host TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
      )
    `);
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS ai_conversations (
        conversation_id TEXT PRIMARY KEY NOT NULL,
        messages_json TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
      )
    `);
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS generic_string_objects (
        uid TEXT PRIMARY KEY NOT NULL,
        client_id TEXT,
        format TEXT NOT NULL,
        serialized_model TEXT NOT NULL,
        revision_ts TEXT NOT NULL,
        metadata_last_updated_ts TEXT NOT NULL,
        permissions_last_updated_ts TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
      )
    `);
    this.repairGenericStringObjectFormats();
  }

  close(): void {
    this.db.close();
  }

  createOrUpdate(input: CreateOrUpdateIntegrationInput): IntegrationRecord {
    const providerSlug = normalizeProviderSlug(input.integrationType);
    const existing = this.get(providerSlug);
    const now = new Date().toISOString();
    const isUpdate = input.isUpdate && existing != null;
    const createdAt = existing?.createdAt ?? now;

    const record: IntegrationRecord = {
      providerSlug,
      enabled: input.enabled,
      environmentUid: applyNullableString(existing?.environmentUid ?? null, input.config.environmentUid, isUpdate),
      basePrompt: applyNullableString(existing?.basePrompt ?? null, input.config.basePrompt, isUpdate),
      modelId: applyNullableString(existing?.modelId ?? null, input.config.modelId, isUpdate),
      mcpServersJson: mergeMcpServers({
        currentJson: existing?.mcpServersJson,
        patchJson: input.config.mcpServersJson,
        removeNames: input.config.removeMcpServerNames,
        isUpdate,
      }),
      workerHost: applyNullableString(existing?.workerHost ?? null, input.config.workerHost, isUpdate),
      createdAt,
      updatedAt: now,
    };

    this.db.prepare(`
      INSERT INTO integrations (
        provider_slug,
        enabled,
        environment_uid,
        base_prompt,
        model_id,
        mcp_servers_json,
        worker_host,
        created_at,
        updated_at
      )
      VALUES (
        @providerSlug,
        @enabled,
        @environmentUid,
        @basePrompt,
        @modelId,
        @mcpServersJson,
        @workerHost,
        @createdAt,
        @updatedAt
      )
      ON CONFLICT(provider_slug) DO UPDATE SET
        enabled = excluded.enabled,
        environment_uid = excluded.environment_uid,
        base_prompt = excluded.base_prompt,
        model_id = excluded.model_id,
        mcp_servers_json = excluded.mcp_servers_json,
        worker_host = excluded.worker_host,
        updated_at = excluded.updated_at
    `).run({
      ...record,
      enabled: record.enabled ? 1 : 0,
    });

    return record;
  }

  get(providerSlug: string): IntegrationRecord | undefined {
    const row = this.db
      .prepare("SELECT * FROM integrations WHERE provider_slug = ?")
      .get(normalizeProviderSlug(providerSlug)) as IntegrationRow | undefined;
    return row ? rowToRecord(row) : undefined;
  }

  list(providerSlugs: string[]): Array<{ providerSlug: string; record?: IntegrationRecord }> {
    return providerSlugs.map((providerSlug) => {
      const normalized = normalizeProviderSlug(providerSlug);
      return {
        providerSlug: normalized,
        record: this.get(normalized),
      };
    });
  }

  providersUsingEnvironment(environmentId: string): string[] {
    const rows = this.db
      .prepare("SELECT provider_slug FROM integrations WHERE environment_uid = ? ORDER BY provider_slug ASC")
      .all(environmentId) as Array<{ provider_slug: string }>;
    return rows.map((row) => row.provider_slug);
  }

  upsertAiConversation(conversationId: string, messages: readonly unknown[]): AiConversationRecord {
    const normalized = normalizeConversationId(conversationId);
    const existing = this.getAiConversation(normalized);
    const now = new Date().toISOString();
    const record: AiConversationRecord = {
      conversationId: normalized,
      messages: [...messages],
      createdAt: existing?.createdAt ?? now,
      updatedAt: now,
    };

    this.db.prepare(`
      INSERT INTO ai_conversations (
        conversation_id,
        messages_json,
        created_at,
        updated_at
      )
      VALUES (
        @conversationId,
        @messagesJson,
        @createdAt,
        @updatedAt
      )
      ON CONFLICT(conversation_id) DO UPDATE SET
        messages_json = excluded.messages_json,
        updated_at = excluded.updated_at
    `).run({
      conversationId: record.conversationId,
      messagesJson: JSON.stringify(record.messages),
      createdAt: record.createdAt,
      updatedAt: record.updatedAt,
    });

    return record;
  }

  getAiConversation(conversationId: string): AiConversationRecord | undefined {
    const row = this.db
      .prepare("SELECT * FROM ai_conversations WHERE conversation_id = ?")
      .get(normalizeConversationId(conversationId)) as AiConversationRow | undefined;
    return row ? rowToAiConversationRecord(row) : undefined;
  }

  listAiConversations(): AiConversationRecord[] {
    const rows = this.db
      .prepare("SELECT * FROM ai_conversations ORDER BY updated_at ASC")
      .all() as AiConversationRow[];
    return rows.map(rowToAiConversationRecord);
  }

  createGenericStringObject(input: GenericStringObjectInput): GenericStringObjectRecord {
    const now = new Date().toISOString();
    const record: GenericStringObjectRecord = {
      uid: `local-gso-${randomUUID()}`,
      clientId: input.clientId ?? null,
      format: normalizeGenericStringObjectFormat(input.format),
      serializedModel: input.serializedModel,
      revisionTs: now,
      metadataLastUpdatedTs: now,
      permissionsLastUpdatedTs: now,
      createdAt: now,
      updatedAt: now,
    };

    this.db.prepare(`
      INSERT INTO generic_string_objects (
        uid,
        client_id,
        format,
        serialized_model,
        revision_ts,
        metadata_last_updated_ts,
        permissions_last_updated_ts,
        created_at,
        updated_at
      )
      VALUES (
        @uid,
        @clientId,
        @format,
        @serializedModel,
        @revisionTs,
        @metadataLastUpdatedTs,
        @permissionsLastUpdatedTs,
        @createdAt,
        @updatedAt
      )
    `).run(record);

    return record;
  }

  bulkCreateGenericStringObjects(inputs: readonly GenericStringObjectInput[]): GenericStringObjectRecord[] {
    const createMany = this.db.transaction((objects: readonly GenericStringObjectInput[]) => {
      return objects.map((object) => this.createGenericStringObject(object));
    });
    return createMany(inputs) as GenericStringObjectRecord[];
  }

  updateGenericStringObject(uid: string, serializedModel: string): GenericStringObjectRecord {
    const existing = this.getGenericStringObject(uid);
    if (!existing) {
      const now = new Date().toISOString();
      const record: GenericStringObjectRecord = {
        uid,
        clientId: null,
        format: inferGenericStringObjectFormat(serializedModel) ?? "JsonMCPServer",
        serializedModel,
        revisionTs: now,
        metadataLastUpdatedTs: now,
        permissionsLastUpdatedTs: now,
        createdAt: now,
        updatedAt: now,
      };

      this.db.prepare(`
        INSERT INTO generic_string_objects (
          uid,
          client_id,
          format,
          serialized_model,
          revision_ts,
          metadata_last_updated_ts,
          permissions_last_updated_ts,
          created_at,
          updated_at
        )
        VALUES (
          @uid,
          @clientId,
          @format,
          @serializedModel,
          @revisionTs,
          @metadataLastUpdatedTs,
          @permissionsLastUpdatedTs,
          @createdAt,
          @updatedAt
        )
      `).run(record);

      return record;
    }

    const now = new Date().toISOString();
    const format = inferGenericStringObjectFormat(serializedModel) ?? existing.format;
    this.db.prepare(`
      UPDATE generic_string_objects
      SET
        format = @format,
        serialized_model = @serializedModel,
        revision_ts = @revisionTs,
        metadata_last_updated_ts = @metadataLastUpdatedTs,
        updated_at = @updatedAt
      WHERE uid = @uid
    `).run({
      uid,
      format,
      serializedModel,
      revisionTs: now,
      metadataLastUpdatedTs: now,
      updatedAt: now,
    });

    const updated = this.getGenericStringObject(uid);
    if (!updated) {
      throw new Error(`generic string object disappeared after update: ${uid}`);
    }
    return updated;
  }

  getGenericStringObject(uid: string): GenericStringObjectRecord | undefined {
    const row = this.db
      .prepare("SELECT * FROM generic_string_objects WHERE uid = ?")
      .get(uid) as GenericStringObjectRow | undefined;
    return row ? rowToGenericStringObjectRecord(row) : undefined;
  }

  listGenericStringObjects(): GenericStringObjectRecord[] {
    const rows = this.db
      .prepare("SELECT * FROM generic_string_objects ORDER BY created_at ASC, uid ASC")
      .all() as GenericStringObjectRow[];
    return rows.map(rowToGenericStringObjectRecord);
  }

  private repairGenericStringObjectFormats(): void {
    const rows = this.db
      .prepare("SELECT uid, format, serialized_model FROM generic_string_objects")
      .all() as Array<Pick<GenericStringObjectRow, "uid" | "format" | "serialized_model">>;

    const updateFormat = this.db.prepare(`
      UPDATE generic_string_objects
      SET format = @format
      WHERE uid = @uid
    `);

    for (const row of rows) {
      const inferredFormat = inferGenericStringObjectFormat(row.serialized_model);
      if (inferredFormat && inferredFormat !== row.format) {
        updateFormat.run({ uid: row.uid, format: inferredFormat });
      }
    }
  }
}
