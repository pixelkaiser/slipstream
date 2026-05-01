# Slipstream Local Development

This file is for contributors who want to build, run, or debug Slipstream from source. End users should install the macOS `.dmg` from the [GitHub Releases page](https://github.com/pixelkaiser/slipstream/releases).

## Local Backend

Slipstream uses a local multi-agent backend for no-cloud agent requests. It accepts Warp-compatible protobuf requests, calls an OpenAI-compatible `/chat/completions` endpoint, streams responses back to the app, and serves the local GraphQL surface used by no-cloud integrations.

Run it from the repository root:

```sh
make local-agent-dev
```

Useful targets:

```sh
make local-agent-install
make local-agent-build
make local-agent-start
make local-agent-test
```

The backend listens on:

```text
http://127.0.0.1:8787
```

Health check:

```sh
curl http://127.0.0.1:8787/health
```

## Provider Configuration

The local backend works with OpenAI-compatible APIs such as LM Studio, vLLM, LocalAI, Ollama-compatible OpenAI endpoints, and private gateways.

Common settings:

```sh
OPENAI_BASE_URL=http://127.0.0.1:11434/v1
OPENAI_MODEL=your-model-id
OPENAI_API_KEY=your-provider-key-or-dummy-value
```

Additional settings:

```sh
LOCAL_MODEL_ALIASES='{"auto-efficient":"your-model-id","auto-coding":"your-model-id"}'
LOCAL_MODEL_LIST=your-model-id
LOCAL_ENABLE_TOOLS=true
LOCAL_MAX_HISTORY_MESSAGES=80
LOCAL_MODEL_CONTEXT_TOKENS=131072
LOCAL_GRAPHQL_DB_PATH=/path/to/local-graphql.sqlite
LOCAL_SERVICE_LOG_PATH=/path/to/local-multi-agent.log
LOCAL_MULTI_AGENT_SYSTEM_PROMPT="You are running as a local Warp-compatible agent endpoint."
HOST=127.0.0.1
PORT=8787
LOG_LEVEL=info
```

Shell environment variables take precedence over values in `.env`.

## No-Cloud Defaults

Slipstream defaults to no-cloud mode. These values are the expected local defaults:

```sh
WARP_NO_CLOUD=1
WARP_SERVER_ROOT_URL=http://127.0.0.1:8787
```

When no-cloud mode is enabled, authenticated Warp cloud requests are avoided for the local agent and local GraphQL paths. If you point `OPENAI_BASE_URL` at an external provider, model requests still go to that provider.

## Building The App

For local macOS app development:

```sh
make warp-build
```

For an optimized macOS app bundle:

```sh
make warp-build-optimized
```

For local signing utilities:

```sh
make warp-local-signing-identity
make warp-signing-status
make warp-grant-keychain-access
```

To clear local Slipstream settings and state on macOS:

```sh
make warp-trash-local-settings
```

## Validation

Focused backend validation:

```sh
make local-agent-test
```

Basic app validation:

```sh
make warp-check
```

For a README-only or documentation-only change, code tests are not required.

## Troubleshooting

- If the app cannot reach the backend, check `http://127.0.0.1:8787/health`.
- If model choices do not appear, verify that your provider serves `/v1/models` or set `OPENAI_MODEL` and `LOCAL_MODEL_LIST`.
- If context usage looks wrong, set `LOCAL_MODEL_CONTEXT_TOKENS` to the provider's real context window.
- If tool calls fail, confirm `LOCAL_ENABLE_TOOLS=true`.
- If the backend starts but provider calls fail, verify `OPENAI_BASE_URL`, `OPENAI_MODEL`, and the API key requirement for your provider.
- If startup fails silently from the app, set `LOCAL_SERVICE_LOG_PATH` and inspect the backend log.

## Related Files

- Local backend README: [crates/local_multi_agent_service/README.md](crates/local_multi_agent_service/README.md)
- Engineering guide inherited from Warp: [WARP.md](WARP.md)
- Contributing guide inherited from Warp: [CONTRIBUTING.md](CONTRIBUTING.md)
