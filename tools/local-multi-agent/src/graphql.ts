import { defaultModel } from "./model.js";
import type {
  GenericStringObjectInput,
  GenericStringObjectRecord,
  IntegrationConfigPatch,
  IntegrationRecord,
  IntegrationStore,
} from "./integrationStore.js";

export type GraphqlResult = {
  status: number;
  payload: unknown;
};

type GraphqlRequest = {
  operationName?: unknown;
  query?: unknown;
  variables?: unknown;
};

type LocalModelConfig = {
  baseModelName?: string;
  creditMultiplier?: number | null;
  description?: string | null;
  disableReason?: string | null;
  displayName?: string;
  id: string;
  provider?: string;
  reasoningLevel?: string | null;
  requestMultiplier?: number;
  visionSupported?: boolean;
};

type ModelCache = {
  baseUrl: string;
  fetchedAtMs: number;
  models: LocalModelConfig[];
};

const providerDescriptions = new Map<string, string>([
  ["linear", "Connect Linear to local Warp agents."],
  ["slack", "Connect Slack to local Warp agents."],
]);

const modelProviderGraphqlEnums = new Map<string, string>([
  ["anthropic", "ANTHROPIC"],
  ["google", "GOOGLE"],
  ["openai", "OPENAI"],
  ["unknown", "UNKNOWN"],
  ["xai", "XAI"],
]);
const knownDisableReasons = new Set(["AdminDisabled", "OutOfRequests", "ProviderOutage", "RequiresUpgrade"]);
const modelCacheTtlMs = 30_000;
let modelCache: ModelCache | null = null;

function asObject(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function optionalString(value: unknown): string | null | undefined {
  if (value == null) {
    return undefined;
  }
  if (typeof value !== "string") {
    throw new Error("expected string value");
  }
  return value;
}

function optionalStringArray(value: unknown): string[] | null | undefined {
  if (value == null) {
    return undefined;
  }
  if (!Array.isArray(value) || !value.every((item) => typeof item === "string")) {
    throw new Error("expected string array value");
  }
  return value;
}

function requiredString(value: unknown, name: string): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`${name} is required`);
  }
  return value;
}

function optionalBoolean(value: unknown, fallback: boolean): boolean {
  if (value == null) {
    return fallback;
  }
  if (typeof value !== "boolean") {
    throw new Error("expected boolean value");
  }
  return value;
}

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

function optionalEnvNumber(value: unknown, fallback: number): number {
  if (value == null) {
    return fallback;
  }
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error("expected numeric model config value");
  }
  return value;
}

function valueAt(source: Record<string, unknown>, ...keys: string[]): unknown {
  for (const key of keys) {
    if (Object.hasOwn(source, key)) {
      return source[key];
    }
  }
  return undefined;
}

function variablesOf(request: GraphqlRequest): Record<string, unknown> {
  return asObject(request.variables);
}

function inputOf(variables: Record<string, unknown>): Record<string, unknown> {
  return asObject(variables.input);
}

function inferOperationName(request: GraphqlRequest, opFromQueryString?: string | null): string | undefined {
  const explicit = typeof request.operationName === "string" && request.operationName.trim()
    ? request.operationName.trim()
    : undefined;
  if (explicit) {
    return explicit;
  }

  if (opFromQueryString?.trim()) {
    return opFromQueryString.trim();
  }

  if (typeof request.query !== "string") {
    return undefined;
  }

  const query = request.query;
  for (const candidate of [
    "createSimpleIntegration",
    "simpleIntegrations",
    "createGenericStringObject",
    "updateGenericStringObject",
    "bulkCreateObjects",
    "getOAuthConnectTxStatus",
    "getIntegrationsUsingEnvironment",
    "userGithubInfo",
    "userRepoAuthStatus",
    "suggestCloudEnvironmentImage",
    "getUpdatedCloudObjects",
    "updatedCloudObjects",
    "getFeatureModelChoices",
    "featureModelChoice",
    "freeAvailableModels",
    "getUserSettings",
    "conversationUsage",
    "getConversationUsage",
    "getUser",
    "getWorkspacesMetadataForUser",
    "workspacesMetadataForUser",
    "pricingInfo",
  ]) {
    if (query.includes(candidate)) {
      return candidate;
    }
  }

  return undefined;
}

function canonicalOperationName(name: string): string {
  switch (name) {
    case "CreateSimpleIntegration":
    case "createSimpleIntegration":
      return "createSimpleIntegration";
    case "SimpleIntegrations":
    case "simpleIntegrations":
      return "simpleIntegrations";
    case "get_oauth_connect_tx_status":
    case "GetOAuthConnectTxStatus":
    case "getOAuthConnectTxStatus":
      return "getOAuthConnectTxStatus";
    case "GetIntegrationsUsingEnvironment":
    case "getIntegrationsUsingEnvironment":
      return "getIntegrationsUsingEnvironment";
    case "user_github_info":
    case "UserGithubInfo":
    case "userGithubInfo":
      return "userGithubInfo";
    case "user_repo_auth_status":
    case "UserRepoAuthStatus":
    case "userRepoAuthStatus":
      return "userRepoAuthStatus";
    case "suggest_cloud_environment_image":
    case "SuggestCloudEnvironmentImage":
    case "suggestCloudEnvironmentImage":
      return "suggestCloudEnvironmentImage";
    case "GetUpdatedCloudObjects":
    case "getUpdatedCloudObjects":
    case "updatedCloudObjects":
      return "updatedCloudObjects";
    case "CreateGenericStringObject":
    case "createGenericStringObject":
      return "createGenericStringObject";
    case "UpdateGenericStringObject":
    case "updateGenericStringObject":
      return "updateGenericStringObject";
    case "BulkCreateObjects":
    case "bulkCreateObjects":
      return "bulkCreateObjects";
    case "GetFeatureModelChoices":
    case "getFeatureModelChoices":
    case "featureModelChoice":
      return "featureModelChoice";
    case "FreeAvailableModels":
    case "free_available_models":
    case "freeAvailableModels":
      return "freeAvailableModels";
    case "GetUser":
    case "getUser":
      return "getUser";
    case "GetUserSettings":
    case "getUserSettings":
      return "getUserSettings";
    case "GetConversationUsage":
    case "getConversationUsage":
    case "conversationUsage":
      return "getConversationUsage";
    case "GetWorkspacesMetadataForUser":
    case "getWorkspacesMetadataForUser":
    case "workspacesMetadataForUser":
    case "pricingInfo":
      return "workspacesMetadataForUser";
    default:
      return name;
  }
}

function integrationConfigFromVariables(variables: Record<string, unknown>): IntegrationConfigPatch {
  const input = inputOf(variables);
  const config = asObject(valueAt(variables, "config") ?? valueAt(input, "config"));

  return {
    basePrompt: optionalString(valueAt(config, "basePrompt", "base_prompt")),
    environmentUid: optionalString(valueAt(config, "environmentUid", "environment_uid")),
    mcpServersJson: optionalString(valueAt(config, "mcpServersJson", "mcp_servers_json")),
    modelId: optionalString(valueAt(config, "modelId", "model_id")),
    removeMcpServerNames: optionalStringArray(valueAt(config, "removeMcpServerNames", "remove_mcp_server_names")),
    workerHost: optionalString(valueAt(config, "workerHost", "worker_host")),
  };
}

function createSimpleIntegration(store: IntegrationStore, variables: Record<string, unknown>): unknown {
  const input = inputOf(variables);
  const integrationType = requiredString(
    valueAt(variables, "integrationType", "integration_type") ?? valueAt(input, "integrationType", "integration_type"),
    "integrationType",
  );
  const record = store.createOrUpdate({
    config: integrationConfigFromVariables(variables),
    enabled: optionalBoolean(valueAt(variables, "enabled") ?? valueAt(input, "enabled"), true),
    integrationType,
    isUpdate: optionalBoolean(valueAt(variables, "isUpdate", "is_update") ?? valueAt(input, "isUpdate", "is_update"), false),
  });

  return {
    data: {
      createSimpleIntegration: {
        __typename: "CreateSimpleIntegrationOutput",
        authUrl: null,
        success: true,
        message: `Local ${record.providerSlug} integration saved.`,
        txId: null,
      },
    },
  };
}

function listedConfig(record: IntegrationRecord): unknown {
  return {
    environmentUid: record.environmentUid ?? "",
    basePrompt: record.basePrompt ?? "",
    modelId: record.modelId ?? "",
    mcpServersJson: record.mcpServersJson,
  };
}

function simpleIntegrationPayload(providerSlug: string, record?: IntegrationRecord): unknown {
  const description = providerDescriptions.get(providerSlug) ?? `Local ${providerSlug} integration.`;
  if (!record) {
    return {
      providerSlug,
      description,
      connectionStatus: "INTEGRATION_NOT_CONFIGURED",
      integrationConfig: null,
      createdAt: null,
      updatedAt: null,
    };
  }

  return {
    providerSlug,
    description,
    connectionStatus: record.enabled ? "ACTIVE" : "NOT_ENABLED",
    integrationConfig: listedConfig(record),
    createdAt: record.createdAt,
    updatedAt: record.updatedAt,
  };
}

function simpleIntegrations(store: IntegrationStore, variables: Record<string, unknown>): unknown {
  const input = inputOf(variables);
  const providers = valueAt(input, "providers");
  if (!Array.isArray(providers) || !providers.every((provider) => typeof provider === "string")) {
    throw new Error("input.providers is required");
  }

  return {
    data: {
      simpleIntegrations: {
        __typename: "SimpleIntegrationsOutput",
        integrations: store
          .list(providers)
          .map(({ providerSlug, record }) => simpleIntegrationPayload(providerSlug, record)),
        message: null,
      },
    },
  };
}

function getOAuthConnectTxStatus(): unknown {
  return {
    data: {
      getOAuthConnectTxStatus: {
        __typename: "GetOAuthConnectTxStatusOutput",
        status: "COMPLETED",
      },
    },
  };
}

function getIntegrationsUsingEnvironment(store: IntegrationStore, variables: Record<string, unknown>): unknown {
  const input = inputOf(variables);
  const environmentId = requiredString(valueAt(input, "environmentId", "environment_id"), "environmentId");
  return {
    data: {
      getIntegrationsUsingEnvironment: {
        __typename: "GetIntegrationsUsingEnvironmentOutput",
        providerNames: store.providersUsingEnvironment(environmentId),
      },
    },
  };
}

function userGithubInfo(): unknown {
  return {
    data: {
      userGithubInfo: {
        __typename: "GithubConnectedOutput",
        username: "local",
        installedRepos: [],
        appInstallLink: "",
      },
    },
  };
}

function userRepoAuthStatus(variables: Record<string, unknown>): unknown {
  const input = inputOf(variables);
  const repos = valueAt(input, "repos");
  if (!Array.isArray(repos)) {
    throw new Error("input.repos is required");
  }

  return {
    data: {
      userRepoAuthStatus: {
        __typename: "UserRepoAuthStatusOutput",
        statuses: repos.map((repo) => {
          const repoObject = asObject(repo);
          return {
            owner: requiredString(repoObject.owner, "repo.owner"),
            repo: requiredString(repoObject.repo, "repo.repo"),
            status: "SUCCESS",
            isPublic: true,
          };
        }),
        authUrl: null,
        txId: null,
      },
    },
  };
}

function suggestCloudEnvironmentImage(): unknown {
  return {
    data: {
      suggestCloudEnvironmentImage: {
        __typename: "SuggestCloudEnvironmentImageOutput",
        detectedLanguages: [],
        image: "ubuntu:24.04",
        needsCustomImage: false,
        reason: "Local no-cloud mode uses a deterministic default image.",
        responseContext: {
          serverVersion: "local",
        },
      },
    },
  };
}

function normalizeModelProvider(provider: string | undefined): string {
  if (!provider) {
    return "UNKNOWN";
  }

  return modelProviderGraphqlEnums.get(provider.trim().toLowerCase()) ?? "UNKNOWN";
}

function normalizeDisableReason(reason: string | null | undefined): string | null {
  if (!reason) {
    return null;
  }

  return knownDisableReasons.has(reason) ? reason : null;
}

function parseLocalModelList(rawModels: string | undefined): LocalModelConfig[] {
  const raw = nonEmpty(rawModels);
  if (!raw) {
    return [{
      id: nonEmpty(process.env.OPENAI_MODEL) ?? defaultModel,
      displayName: nonEmpty(process.env.OPENAI_MODEL) ?? defaultModel,
    }];
  }

  const parseModel = (value: unknown): LocalModelConfig => {
    if (typeof value === "string") {
      const id = requiredString(value, "model id");
      return { id, displayName: id };
    }

    const model = asObject(value);
    const id = requiredString(model.id, "model.id");
    return {
      baseModelName: optionalString(model.baseModelName) ?? optionalString(model.base_model_name) ?? undefined,
      creditMultiplier: model.creditMultiplier === null || model.credit_multiplier === null
        ? null
        : optionalEnvNumber(model.creditMultiplier ?? model.credit_multiplier, 1),
      description: optionalString(model.description) ?? null,
      disableReason: optionalString(model.disableReason) ?? optionalString(model.disable_reason) ?? null,
      displayName: optionalString(model.displayName) ?? optionalString(model.display_name) ?? id,
      id,
      provider: optionalString(model.provider) ?? undefined,
      reasoningLevel: optionalString(model.reasoningLevel) ?? optionalString(model.reasoning_level) ?? null,
      requestMultiplier: optionalEnvNumber(model.requestMultiplier ?? model.request_multiplier, 1),
      visionSupported: optionalBoolean(model.visionSupported ?? model.vision_supported, true),
    };
  };

  const parsed = raw.startsWith("[")
    ? (JSON.parse(raw) as unknown)
    : raw.split(",").map((value) => value.trim()).filter(Boolean);
  if (!Array.isArray(parsed)) {
    throw new Error("LOCAL_MODEL_LIST must be a JSON array or comma-separated model ID list.");
  }

  const models = parsed.map(parseModel);
  if (models.length === 0) {
    throw new Error("LOCAL_MODEL_LIST must include at least one model.");
  }
  return models;
}

function fallbackLocalModels(): LocalModelConfig[] {
  return parseLocalModelList(process.env.LOCAL_MODEL_LIST);
}

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

async function fetchProviderModels(): Promise<LocalModelConfig[]> {
  const baseUrl = nonEmpty(process.env.OPENAI_BASE_URL);
  if (!baseUrl) {
    return fallbackLocalModels();
  }

  const normalizedBaseUrl = trimTrailingSlash(baseUrl);
  const now = Date.now();
  if (
    modelCache
    && modelCache.baseUrl === normalizedBaseUrl
    && now - modelCache.fetchedAtMs < modelCacheTtlMs
  ) {
    return modelCache.models;
  }

  const headers: Record<string, string> = {
    accept: "application/json",
  };
  const apiKey = nonEmpty(process.env.OPENAI_API_KEY);
  if (apiKey) {
    headers.authorization = `Bearer ${apiKey}`;
  }

  try {
    const response = await fetch(`${normalizedBaseUrl}/models`, { headers });
    if (!response.ok) {
      throw new Error(`provider models request failed with HTTP ${response.status}`);
    }

    const payload = asObject(await response.json());
    const data = payload.data;
    if (!Array.isArray(data)) {
      throw new Error("provider models response missing data array");
    }

    const models = data.flatMap((item): LocalModelConfig[] => {
      if (typeof item === "string") {
        return [{ id: item, displayName: item }];
      }

      const model = asObject(item);
      const id = typeof model.id === "string" && model.id.trim() ? model.id.trim() : undefined;
      return id ? [{ id, displayName: id }] : [];
    });

    if (models.length === 0) {
      throw new Error("provider models response had no usable model IDs");
    }

    modelCache = {
      baseUrl: normalizedBaseUrl,
      fetchedAtMs: now,
      models,
    };
    return models;
  } catch {
    return fallbackLocalModels();
  }
}

function localModelInfo(model: LocalModelConfig): unknown {
  return {
    displayName: model.displayName ?? model.id,
    baseModelName: model.baseModelName ?? model.displayName ?? model.id,
    id: model.id,
    reasoningLevel: model.reasoningLevel ?? null,
    usageMetadata: {
      requestMultiplier: Math.max(1, model.requestMultiplier ?? 1),
      creditMultiplier: model.creditMultiplier ?? null,
    },
    description: model.description ?? null,
    disableReason: normalizeDisableReason(model.disableReason),
    visionSupported: model.visionSupported ?? true,
    spec: null,
    provider: normalizeModelProvider(model.provider),
    hostConfigs: [{
      enabled: true,
      modelRoutingHost: "DIRECT_API",
    }],
    pricing: {
      discountPercentage: null,
    },
  };
}

async function localAvailableLlms(): Promise<unknown> {
  const models = await fetchProviderModels();
  const choices = models.map(localModelInfo);
  return {
    defaultId: models[0]?.id ?? defaultModel,
    choices,
    preferredCodexModelId: null,
  };
}

async function localFeatureModelChoice(): Promise<unknown> {
  const available = await localAvailableLlms();
  return {
    agentMode: available,
    planning: available,
    coding: available,
    cliAgent: available,
    computerUseAgent: available,
  };
}

async function getFeatureModelChoices(): Promise<unknown> {
  return {
    data: {
      user: {
        __typename: "UserOutput",
        user: {
          workspaces: [{
            featureModelChoice: await localFeatureModelChoice(),
          }],
        },
      },
    },
  };
}

async function freeAvailableModels(): Promise<unknown> {
  return {
    data: {
      freeAvailableModels: {
        __typename: "FreeAvailableModelsOutput",
        featureModelChoice: await localFeatureModelChoice(),
        responseContext: {
          serverVersion: "local",
        },
      },
    },
  };
}

async function getUser(): Promise<unknown> {
  return {
    data: {
      user: {
        __typename: "UserOutput",
        apiKeyOwnerType: null,
        principalType: "USER",
        user: {
          anonymousUserInfo: null,
          experiments: [],
          isOnWorkDomain: false,
          isOnboarded: true,
          profile: {
            displayName: "Local User",
            email: "local@warp.dev",
            needsSsoLink: false,
            photoUrl: null,
            uid: "local-user",
          },
          llms: await localFeatureModelChoice(),
        },
      },
    },
  };
}

function getUserSettings(): unknown {
  return {
    data: {
      user: {
        __typename: "UserOutput",
        user: {
          settings: {
            isCloudConversationStorageEnabled: false,
            isCrashReportingEnabled: false,
            isTelemetryEnabled: false,
          },
        },
      },
    },
  };
}

function getConversationUsage(): unknown {
  return {
    data: {
      user: {
        __typename: "UserOutput",
        user: {
          conversationUsage: [],
        },
      },
    },
  };
}

const localUserUid = "local-user";

function responseContext(): { serverVersion: string } {
  return { serverVersion: "local" };
}

function genericStringObjectInputFromValue(value: unknown): GenericStringObjectInput {
  const object = asObject(value);
  const clientId = optionalString(valueAt(object, "clientId", "client_id"));
  const format = requiredString(valueAt(object, "format"), "genericStringObject.format");
  const serializedModel = requiredString(
    valueAt(object, "serializedModel", "serialized_model"),
    "genericStringObject.serializedModel",
  );
  const uniquenessKey = asObject(valueAt(object, "uniquenessKey", "uniqueness_key"));

  return {
    clientId,
    format,
    serializedModel,
    uniquenessKey: Object.keys(uniquenessKey).length > 0
      ? {
          key: requiredString(valueAt(uniquenessKey, "key"), "genericStringObject.uniquenessKey.key"),
          uniquePer: requiredString(valueAt(uniquenessKey, "uniquePer", "unique_per"), "genericStringObject.uniquenessKey.uniquePer"),
        }
      : null,
  };
}

function objectMetadata(record: GenericStringObjectRecord): unknown {
  return {
    __typename: "ObjectMetadata",
    creatorUid: localUserUid,
    currentEditorUid: null,
    isWelcomeObject: false,
    lastEditorUid: localUserUid,
    metadataLastUpdatedTs: record.metadataLastUpdatedTs,
    parent: {
      __typename: "Space",
      type: "User",
      uid: localUserUid,
    },
    revisionTs: record.revisionTs,
    trashedTs: null,
    uid: record.uid,
  };
}

function objectPermissions(record: GenericStringObjectRecord): unknown {
  return {
    __typename: "ObjectPermissions",
    anyoneLinkSharing: null,
    guests: [],
    lastUpdatedTs: record.permissionsLastUpdatedTs,
    space: {
      __typename: "Space",
      type: "User",
      uid: localUserUid,
    },
  };
}

function genericStringObjectPayload(record: GenericStringObjectRecord): unknown {
  return {
    __typename: "GenericStringObject",
    format: record.format,
    metadata: objectMetadata(record),
    permissions: objectPermissions(record),
    serializedModel: record.serializedModel,
  };
}

function createGenericStringObjectOutput(record: GenericStringObjectRecord): unknown {
  return {
    __typename: "CreateGenericStringObjectOutput",
    clientId: record.clientId ?? record.uid,
    genericStringObject: genericStringObjectPayload(record),
    responseContext: responseContext(),
    revisionTs: record.revisionTs,
  };
}

function createGenericStringObject(store: IntegrationStore, variables: Record<string, unknown>): unknown {
  const input = inputOf(variables);
  const record = store.createGenericStringObject(
    genericStringObjectInputFromValue(valueAt(input, "genericStringObject", "generic_string_object")),
  );

  return {
    data: {
      createGenericStringObject: createGenericStringObjectOutput(record),
    },
  };
}

function bulkCreateObjects(store: IntegrationStore, variables: Record<string, unknown>): unknown {
  const input = inputOf(variables);
  const genericStringObjects = asObject(valueAt(input, "genericStringObjects", "generic_string_objects"));
  const objects = valueAt(genericStringObjects, "objects");
  if (!Array.isArray(objects)) {
    throw new Error("input.genericStringObjects.objects is required");
  }

  const records = store.bulkCreateGenericStringObjects(objects.map(genericStringObjectInputFromValue));
  return {
    data: {
      bulkCreateObjects: {
        __typename: "BulkCreateObjectsOutput",
        genericStringObjects: {
          __typename: "BulkCreateGenericStringObjectsOutput",
          objects: records.map(createGenericStringObjectOutput),
        },
        responseContext: responseContext(),
      },
    },
  };
}

function updateGenericStringObject(store: IntegrationStore, variables: Record<string, unknown>): unknown {
  const input = inputOf(variables);
  const uid = requiredString(valueAt(input, "uid"), "uid");
  const serializedModel = requiredString(valueAt(input, "serializedModel", "serialized_model"), "serializedModel");
  const record = store.updateGenericStringObject(uid, serializedModel);

  return {
    data: {
      updateGenericStringObject: {
        __typename: "UpdateGenericStringObjectOutput",
        responseContext: responseContext(),
        update: {
          __typename: "ObjectUpdateSuccess",
          lastEditorUid: localUserUid,
          revisionTs: record.revisionTs,
        },
      },
    },
  };
}

function getUpdatedCloudObjects(store: IntegrationStore): unknown {
  return {
    data: {
      updatedCloudObjects: {
        __typename: "UpdatedCloudObjectsOutput",
        actionHistories: [],
        deletedObjectUids: {
          folderUids: [],
          genericStringObjectUids: [],
          notebookUids: [],
          workflowUids: [],
        },
        folders: [],
        genericStringObjects: store.listGenericStringObjects().map(genericStringObjectPayload),
        mcpGallery: [],
        notebooks: [],
        responseContext: responseContext(),
        userProfiles: [],
        workflows: [],
      },
    },
  };
}

function getWorkspacesMetadataForUser(): unknown {
  return {
    data: {
      user: {
        __typename: "UserOutput",
        user: {
          workspaces: [],
          experiments: [],
          discoverableTeams: [],
        },
      },
      pricingInfo: {
        __typename: "PricingInfoOutput",
        pricingInfo: {
          plans: [],
          overages: {
            pricePerRequestUsdCents: 0,
          },
          addonCreditsOptions: [],
        },
      },
    },
  };
}

function unsupportedOperation(operationName: string | undefined): GraphqlResult {
  return {
    status: 400,
    payload: {
      data: null,
      errors: [
        {
          message: `unsupported_operation: ${operationName ?? "unknown"}`,
        },
      ],
    },
  };
}

export async function handleLocalGraphqlRequest(
  request: GraphqlRequest,
  store: IntegrationStore,
  opFromQueryString?: string | null,
): Promise<GraphqlResult> {
  const operationName = inferOperationName(request, opFromQueryString);
  const variables = variablesOf(request);

  try {
    switch (operationName ? canonicalOperationName(operationName) : undefined) {
      case "createSimpleIntegration":
        return { status: 200, payload: createSimpleIntegration(store, variables) };
      case "simpleIntegrations":
        return { status: 200, payload: simpleIntegrations(store, variables) };
      case "getOAuthConnectTxStatus":
        return { status: 200, payload: getOAuthConnectTxStatus() };
      case "getIntegrationsUsingEnvironment":
        return { status: 200, payload: getIntegrationsUsingEnvironment(store, variables) };
      case "userGithubInfo":
        return { status: 200, payload: userGithubInfo() };
      case "userRepoAuthStatus":
        return { status: 200, payload: userRepoAuthStatus(variables) };
      case "suggestCloudEnvironmentImage":
        return { status: 200, payload: suggestCloudEnvironmentImage() };
      case "createGenericStringObject":
        return { status: 200, payload: createGenericStringObject(store, variables) };
      case "updateGenericStringObject":
        return { status: 200, payload: updateGenericStringObject(store, variables) };
      case "bulkCreateObjects":
        return { status: 200, payload: bulkCreateObjects(store, variables) };
      case "updatedCloudObjects":
        return { status: 200, payload: getUpdatedCloudObjects(store) };
      case "featureModelChoice":
        return { status: 200, payload: await getFeatureModelChoices() };
      case "freeAvailableModels":
        return { status: 200, payload: await freeAvailableModels() };
      case "getUser":
        return { status: 200, payload: await getUser() };
      case "getUserSettings":
        return { status: 200, payload: getUserSettings() };
      case "getConversationUsage":
        return { status: 200, payload: getConversationUsage() };
      case "workspacesMetadataForUser":
        return { status: 200, payload: getWorkspacesMetadataForUser() };
      default:
        return unsupportedOperation(operationName);
    }
  } catch (error) {
    return {
      status: 400,
      payload: {
        data: null,
        errors: [
          {
            message: error instanceof Error ? error.message : String(error),
          },
        ],
      },
    };
  }
}
