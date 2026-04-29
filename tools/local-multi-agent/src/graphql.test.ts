import assert from "node:assert/strict";
import http from "node:http";
import { mkdtempSync, rmSync } from "node:fs";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { handleLocalGraphqlRequest } from "./graphql.js";
import { IntegrationStore } from "./integrationStore.js";

async function withStore(fn: (store: IntegrationStore) => void | Promise<void>): Promise<void> {
  const dir = mkdtempSync(join(tmpdir(), "warp-local-graphql-"));
  const store = new IntegrationStore(join(dir, "test.sqlite"));
  try {
    await fn(store);
  } finally {
    store.close();
    rmSync(dir, { force: true, recursive: true });
  }
}

async function dataOf(result: ReturnType<typeof handleLocalGraphqlRequest>): Promise<Record<string, unknown>> {
  const resolved = await result;
  assert.equal(resolved.status, 200);
  const payload = resolved.payload as { data?: unknown };
  assert.ok(payload.data);
  return payload.data as Record<string, unknown>;
}

async function listenOnLoopback(server: http.Server): Promise<number> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      const address = server.address();
      assert.notEqual(typeof address, "string");
      assert.ok(address);
      resolve((address as AddressInfo).port);
    });
  });
}

async function closeServer(server: http.Server): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    server.close((error) => (error ? reject(error) : resolve()));
  });
}

function withEnv(env: Record<string, string | undefined>, fn: () => Promise<void>): Promise<void> {
  const previous = Object.fromEntries(Object.keys(env).map((key) => [key, process.env[key]]));
  for (const [key, value] of Object.entries(env)) {
    if (value == null) {
      delete process.env[key];
    } else {
      process.env[key] = value;
    }
  }
  return fn().finally(() => {
    for (const [key, value] of Object.entries(previous)) {
      if (value == null) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  });
}

async function expectOk(result: ReturnType<typeof handleLocalGraphqlRequest>): Promise<void> {
  const resolved = await result;
  assert.equal(resolved.status, 200);
}

test("creates and lists simple integrations with GraphQL response field names", async () => {
  await withStore(async (store) => {
    const createData = await dataOf(handleLocalGraphqlRequest({
      operationName: "CreateSimpleIntegration",
      variables: {
        integration_type: "linear",
        is_update: false,
        enabled: true,
        config: {
          environment_uid: "env-1",
          base_prompt: "Local prompt",
          model_id: "auto-coding",
          mcp_servers_json: JSON.stringify({
            local: { command: "node", args: ["server.js"] },
          }),
        },
      },
    }, store));

    assert.deepEqual(createData.createSimpleIntegration, {
      __typename: "CreateSimpleIntegrationOutput",
      authUrl: null,
      success: true,
      message: "Local linear integration saved.",
      txId: null,
    });

    const listData = await dataOf(handleLocalGraphqlRequest({
      operationName: "SimpleIntegrations",
      variables: {
        input: { providers: ["linear", "slack"] },
      },
    }, store));

    const output = listData.simpleIntegrations as {
      __typename: string;
      integrations: Array<{
        providerSlug: string;
        connectionStatus: string;
        integrationConfig: {
          environmentUid: string;
          basePrompt: string;
          modelId: string;
          mcpServersJson: string;
        } | null;
      }>;
      message: string | null;
    };

    assert.equal(output.__typename, "SimpleIntegrationsOutput");
    assert.equal(output.message, null);
    assert.equal(output.integrations[0]?.providerSlug, "linear");
    assert.equal(output.integrations[0]?.connectionStatus, "ACTIVE");
    assert.equal(output.integrations[0]?.integrationConfig?.environmentUid, "env-1");
    assert.equal(output.integrations[0]?.integrationConfig?.basePrompt, "Local prompt");
    assert.equal(output.integrations[0]?.integrationConfig?.modelId, "auto-coding");
    assert.deepEqual(JSON.parse(output.integrations[0]?.integrationConfig?.mcpServersJson ?? ""), {
      local: { command: "node", args: ["server.js"] },
    });
    assert.equal(output.integrations[1]?.providerSlug, "slack");
    assert.equal(output.integrations[1]?.connectionStatus, "INTEGRATION_NOT_CONFIGURED");
    assert.equal(output.integrations[1]?.integrationConfig, null);
  });
});

test("returns deterministic local helper responses", async () => {
  await withStore(async (store) => {
    store.createOrUpdate({
      integrationType: "slack",
      isUpdate: false,
      enabled: true,
      config: { environmentUid: "env-slack" },
    });

    assert.deepEqual((await dataOf(handleLocalGraphqlRequest({
      operationName: "get_oauth_connect_tx_status",
      variables: { input: { txId: "tx-local" } },
    }, store))).getOAuthConnectTxStatus, {
      __typename: "GetOAuthConnectTxStatusOutput",
      status: "COMPLETED",
    });

    assert.deepEqual((await dataOf(handleLocalGraphqlRequest({
      operationName: "GetIntegrationsUsingEnvironment",
      variables: { input: { environment_id: "env-slack" } },
    }, store))).getIntegrationsUsingEnvironment, {
      __typename: "GetIntegrationsUsingEnvironmentOutput",
      providerNames: ["slack"],
    });

    assert.deepEqual((await dataOf(handleLocalGraphqlRequest({
      operationName: "user_github_info",
      variables: {},
    }, store))).userGithubInfo, {
      __typename: "GithubConnectedOutput",
      username: "local",
      installedRepos: [],
      appInstallLink: "",
    });

    assert.deepEqual((await dataOf(handleLocalGraphqlRequest({
      operationName: "user_repo_auth_status",
      variables: {
        input: {
          repos: [{ owner: "warpdotdev", repo: "warp" }],
        },
      },
    }, store))).userRepoAuthStatus, {
      __typename: "UserRepoAuthStatusOutput",
      statuses: [{
        owner: "warpdotdev",
        repo: "warp",
        status: "SUCCESS",
        isPublic: true,
      }],
      authUrl: null,
      txId: null,
    });

    assert.deepEqual((await dataOf(handleLocalGraphqlRequest({
      operationName: "suggest_cloud_environment_image",
      variables: {
        input: {
          repos: [{ owner: "warpdotdev", repo: "warp" }],
        },
      },
    }, store))).suggestCloudEnvironmentImage, {
      __typename: "SuggestCloudEnvironmentImageOutput",
      detectedLanguages: [],
      image: "ubuntu:24.04",
      needsCustomImage: false,
      reason: "Local no-cloud mode uses a deterministic default image.",
      responseContext: {
        serverVersion: "local",
      },
    });
  });
});

test("returns local no-cloud responses for startup cloud metadata operations", async () => {
  await withStore(async (store) => {
    assert.deepEqual((await dataOf(handleLocalGraphqlRequest({
      operationName: "GetUpdatedCloudObjects",
      variables: {
        input: {
          forceRefresh: false,
          folders: [],
          genericStringObjects: [],
          notebooks: [],
          workflows: [],
        },
      },
    }, store))).updatedCloudObjects, {
      __typename: "UpdatedCloudObjectsOutput",
      actionHistories: [],
      deletedObjectUids: {
        folderUids: [],
        genericStringObjectUids: [],
        notebookUids: [],
        workflowUids: [],
      },
      folders: [],
      genericStringObjects: [],
      mcpGallery: [],
      notebooks: [],
      responseContext: {
        serverVersion: "local",
      },
      userProfiles: [],
      workflows: [],
    });

    assert.deepEqual(await dataOf(handleLocalGraphqlRequest({
      operationName: "GetWorkspacesMetadataForUser",
      variables: {},
    }, store)), {
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
    });
  });
});

test("populates local model choices from the configured v1/models endpoint", async () => {
  let authHeader: string | undefined;
  const provider = http.createServer((_request, response) => {
    authHeader = _request.headers.authorization;
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({
      object: "list",
      data: [
        { id: "local-qwen" },
        { id: "local-coder" },
      ],
    }));
  });
  const port = await listenOnLoopback(provider);

  try {
    await withEnv({
      OPENAI_API_KEY: "sk-local-test",
      OPENAI_BASE_URL: `http://127.0.0.1:${port}/v1`,
      OPENAI_MODEL: undefined,
      LOCAL_MODEL_LIST: undefined,
    }, async () => {
      await withStore(async (store) => {
        const data = await dataOf(handleLocalGraphqlRequest({
          operationName: "GetFeatureModelChoices",
          variables: {},
        }, store));
        const user = data.user as {
          user?: {
            workspaces?: Array<{
              featureModelChoice?: {
                agentMode?: {
                  defaultId?: string;
                  choices?: Array<{ id?: string; provider?: string; hostConfigs?: Array<{ modelRoutingHost?: string }> }>;
                };
              };
            }>;
          };
        };
        const agentMode = user.user?.workspaces?.[0]?.featureModelChoice?.agentMode;
        assert.equal(agentMode?.defaultId, "local-qwen");
        assert.deepEqual(agentMode?.choices?.map((choice) => choice.id), ["local-qwen", "local-coder"]);
        assert.equal(agentMode?.choices?.[0]?.provider, "Unknown");
        assert.equal(agentMode?.choices?.[0]?.hostConfigs?.[0]?.modelRoutingHost, "DirectApi");
        assert.equal(authHeader, "Bearer sk-local-test");

        const freeData = await dataOf(handleLocalGraphqlRequest({
          operationName: "FreeAvailableModels",
          variables: { input: {} },
        }, store));
        const freeAvailableModels = freeData.freeAvailableModels as {
          featureModelChoice?: { coding?: { choices?: Array<{ id?: string }> } };
        };
        assert.deepEqual(
          freeAvailableModels.featureModelChoice?.coding?.choices?.map((choice) => choice.id),
          ["local-qwen", "local-coder"],
        );
      });
    });
  } finally {
    await closeServer(provider);
  }
});

test("falls back to local configured model when v1/models is unavailable", async () => {
  await withEnv({
    OPENAI_BASE_URL: "http://127.0.0.1:1/v1",
    OPENAI_MODEL: "fallback-model",
    LOCAL_MODEL_LIST: undefined,
  }, async () => {
    await withStore(async (store) => {
      const data = await dataOf(handleLocalGraphqlRequest({
        operationName: "GetFeatureModelChoices",
        variables: {},
      }, store));
      const user = data.user as {
        user?: {
          workspaces?: Array<{
            featureModelChoice?: { agentMode?: { defaultId?: string; choices?: Array<{ id?: string }> } };
          }>;
        };
      };
      const agentMode = user.user?.workspaces?.[0]?.featureModelChoice?.agentMode;
      assert.equal(agentMode?.defaultId, "fallback-model");
      assert.deepEqual(agentMode?.choices?.map((choice) => choice.id), ["fallback-model"]);
    });
  });
});

test("uses query string operation name and rejects unsupported operations", async () => {
  await withStore(async (store) => {
    const ok = handleLocalGraphqlRequest({
      variables: { input: { providers: ["linear"] } },
    }, store, "SimpleIntegrations");
    await expectOk(ok);

    const unsupported = await handleLocalGraphqlRequest({
      operationName: "GetUser",
      variables: {},
    }, store);
    assert.equal(unsupported.status, 400);
    assert.match(JSON.stringify(unsupported.payload), /unsupported_operation: GetUser/);
  });
});
