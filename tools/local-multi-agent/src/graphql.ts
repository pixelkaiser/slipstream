import type { IntegrationConfigPatch, IntegrationRecord, IntegrationStore } from "./integrationStore.js";

export type GraphqlResult = {
  status: number;
  payload: unknown;
};

type GraphqlRequest = {
  operationName?: unknown;
  query?: unknown;
  variables?: unknown;
};

const providerDescriptions = new Map<string, string>([
  ["linear", "Connect Linear to local Warp agents."],
  ["slack", "Connect Slack to local Warp agents."],
]);

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
    "getOAuthConnectTxStatus",
    "getIntegrationsUsingEnvironment",
    "userGithubInfo",
    "userRepoAuthStatus",
    "suggestCloudEnvironmentImage",
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

export function handleLocalGraphqlRequest(
  request: GraphqlRequest,
  store: IntegrationStore,
  opFromQueryString?: string | null,
): GraphqlResult {
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
