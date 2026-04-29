import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { handleLocalGraphqlRequest } from "./graphql.js";
import { IntegrationStore } from "./integrationStore.js";

function withStore(fn: (store: IntegrationStore) => void): void {
  const dir = mkdtempSync(join(tmpdir(), "warp-local-graphql-"));
  const store = new IntegrationStore(join(dir, "test.sqlite"));
  try {
    fn(store);
  } finally {
    store.close();
    rmSync(dir, { force: true, recursive: true });
  }
}

function dataOf(result: ReturnType<typeof handleLocalGraphqlRequest>): Record<string, unknown> {
  assert.equal(result.status, 200);
  const payload = result.payload as { data?: unknown };
  assert.ok(payload.data);
  return payload.data as Record<string, unknown>;
}

test("creates and lists simple integrations with GraphQL response field names", () => {
  withStore((store) => {
    const createData = dataOf(handleLocalGraphqlRequest({
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

    const listData = dataOf(handleLocalGraphqlRequest({
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

test("returns deterministic local helper responses", () => {
  withStore((store) => {
    store.createOrUpdate({
      integrationType: "slack",
      isUpdate: false,
      enabled: true,
      config: { environmentUid: "env-slack" },
    });

    assert.deepEqual(dataOf(handleLocalGraphqlRequest({
      operationName: "get_oauth_connect_tx_status",
      variables: { input: { txId: "tx-local" } },
    }, store)).getOAuthConnectTxStatus, {
      __typename: "GetOAuthConnectTxStatusOutput",
      status: "COMPLETED",
    });

    assert.deepEqual(dataOf(handleLocalGraphqlRequest({
      operationName: "GetIntegrationsUsingEnvironment",
      variables: { input: { environment_id: "env-slack" } },
    }, store)).getIntegrationsUsingEnvironment, {
      __typename: "GetIntegrationsUsingEnvironmentOutput",
      providerNames: ["slack"],
    });

    assert.deepEqual(dataOf(handleLocalGraphqlRequest({
      operationName: "user_github_info",
      variables: {},
    }, store)).userGithubInfo, {
      __typename: "GithubConnectedOutput",
      username: "local",
      installedRepos: [],
      appInstallLink: "",
    });

    assert.deepEqual(dataOf(handleLocalGraphqlRequest({
      operationName: "user_repo_auth_status",
      variables: {
        input: {
          repos: [{ owner: "warpdotdev", repo: "warp" }],
        },
      },
    }, store)).userRepoAuthStatus, {
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

    assert.deepEqual(dataOf(handleLocalGraphqlRequest({
      operationName: "suggest_cloud_environment_image",
      variables: {
        input: {
          repos: [{ owner: "warpdotdev", repo: "warp" }],
        },
      },
    }, store)).suggestCloudEnvironmentImage, {
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

test("returns local no-cloud responses for startup cloud metadata operations", () => {
  withStore((store) => {
    assert.deepEqual(dataOf(handleLocalGraphqlRequest({
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
    }, store)).updatedCloudObjects, {
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

    assert.deepEqual(dataOf(handleLocalGraphqlRequest({
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

test("uses query string operation name and rejects unsupported operations", () => {
  withStore((store) => {
    const ok = handleLocalGraphqlRequest({
      variables: { input: { providers: ["linear"] } },
    }, store, "SimpleIntegrations");
    assert.equal(ok.status, 200);

    const unsupported = handleLocalGraphqlRequest({
      operationName: "GetUser",
      variables: {},
    }, store);
    assert.equal(unsupported.status, 400);
    assert.match(JSON.stringify(unsupported.payload), /unsupported_operation: GetUser/);
  });
});
