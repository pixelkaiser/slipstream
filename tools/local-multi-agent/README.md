# Local Multi-Agent API

This is a local development implementation of Warp's `/ai/multi-agent` protocol and the small GraphQL surface used by CLI integrations. It accepts Warp's protobuf request body, calls an OpenAI-compatible `/chat/completions` endpoint, and streams protobuf response events back over server-sent events. Assistant text is forwarded incrementally: the first provider content chunk creates the Warp assistant message and later chunks append to that same message.

It is intentionally a thin single-agent adapter. It can translate OpenAI-compatible function calls for Warp client-executed tools, but durable orchestration and rich passive suggestions remain follow-up work in `specs/BYOK-local-multi-agent/PLAN.md`.

## Run

### From Source

```sh
npm install
cp .env.example .env
npm run dev
```

The service loads `.env` from this directory at startup. Shell environment variables take precedence over values in `.env`.

Set these environment variables before starting the service:

- `OPENAI_API_KEY`
- `OPENAI_BASE_URL` unless Warp sends `X-Warp-OpenAI-Base-URL` from the BYOK OpenAI Base URL setting. Set this on the service for no-cloud mode because the local GraphQL model picker uses `GET $OPENAI_BASE_URL/models`.
- `OPENAI_MODEL` optionally overrides every `/ai/multi-agent` request. Leave it unset to honor the model selected in Warp. If `/models` is unavailable, it is also used as the fallback model ID.
- `LOCAL_MODEL_LIST` optionally provides a comma-separated or JSON-array fallback model list when `GET $OPENAI_BASE_URL/models` is unavailable. It defaults to `Qwen/Qwen3.6-27B-FP8`.
- `LOCAL_MODEL_ALIASES` optionally maps Warp model IDs to provider model IDs as JSON. Built-in aliases map `auto`, `auto-efficient`, `auto-coding`, and `auto-reasoning` to `Qwen/Qwen3.6-27B-FP8`.
- `LOCAL_ENABLE_TOOLS=false` disables local tool-call advertisement. Tools are enabled by default.
- `LOCAL_MAX_HISTORY_MESSAGES` limits in-memory provider transcript messages per conversation. Defaults to `80`.
- `LOCAL_MODEL_CONTEXT_TOKENS` optionally provides context-window sizes when `/models` does not expose them. It accepts either a single token count or a JSON object keyed by provider model ID, for example `{"Qwen/Qwen3.6-27B-FP8":262144,"default":131072}`. Without provider metadata or an override, the service uses 128k tokens as the default context window.
- `LOCAL_GRAPHQL_DB_PATH` sets the SQLite path for local integration GraphQL config and AI conversation transcripts. Defaults to `./local-graphql.sqlite` from this package.
- `LOG_LEVEL` defaults to `info`; use `debug` to log individual SSE events.
- `LOCAL_SERVICE_LOG_PATH` sets the JSONL log file path. Defaults to `./local-service.log` from this package. Set it to `false`, `off`, or `0` to disable file logging.
- `PORT` defaults to `8787`
- `HOST` defaults to `127.0.0.1` for source runs. The Docker image sets `HOST=0.0.0.0` so published ports work.

### With Docker

Build the image locally:

```sh
docker build -t warp-local-multi-agent .
```

Run it with a persistent SQLite volume:

```sh
docker volume create warp-local-multi-agent-data

docker run --rm \
  -p 8787:8787 \
  -v warp-local-multi-agent-data:/data \
  -e OPENAI_API_KEY="$OPENAI_API_KEY" \
  -e OPENAI_BASE_URL="${OPENAI_BASE_URL:-http://host.docker.internal:11434/v1}" \
  warp-local-multi-agent
```

CI publishes the same image to GitHub Container Registry as:

```text
ghcr.io/<owner>/warp-local-multi-agent
```

For Linux hosts, replace `host.docker.internal` with an address reachable from the container, or add Docker's host gateway mapping.

## Behavior

Supported tool-call names are `read_files`, `file_glob`, `grep`, `search_codebase`, `run_shell_command`, `apply_file_diffs`, `suggest_plan`, `read_mcp_resource`, and `call_mcp_tool`. Warp executes the tool call locally and sends the result back to this service on the next request.

The service keeps OpenAI-compatible chat history per Warp conversation ID, including assistant tool calls and tool results. Active conversations are cached in memory and persisted to SQLite so restarts can continue with prior provider context.

For user prompts, the service also forwards supported Warp input context to the provider. This currently includes selected text, referenced attachments, attached executed shell command blocks, running command snapshots, attached text files, images, current directory, OS/shell/time, git metadata, codebase/project-rule summaries, skills, LSP server summaries, and MCP server/resource/tool summaries.

The service logs JSON lines for startup, HTTP requests, Warp multi-agent requests, provider requests, errors, and completion summaries to stdout/stderr and `LOCAL_SERVICE_LOG_PATH`. API keys and authorization-like fields are redacted. GraphQL responses also log operation diagnostics, selected variable/input keys, status codes, and GraphQL error messages so local 400s can be inspected after Warp exits.

When the provider's OpenAI-compatible `/models` response includes a context-window field such as `context_length`, `max_context_length`, `max_model_len`, `max_sequence_length`, or `n_ctx`, the service estimates local conversation context usage and sends it to Warp in `StreamFinished.conversation_usage_metadata`. Providers are not required to return this metadata, so the service falls back to known local model defaults, then 128k tokens, unless `LOCAL_MODEL_CONTEXT_TOKENS` is set.

## Protobufs

Generated TypeScript descriptors live under `src/generated/warp_multi_agent/v1`. Regenerate them from the `warp_multi_agent_api` revision pinned in the repo root `Cargo.toml` with:

```sh
npm run proto:generate
```

Set `WARP_PROTO_APIS_DIR=/path/to/warp-proto-apis` to generate from an existing checkout instead of the script-managed `.proto-cache`.

Point Warp's BYOK `Local Multi-Agent Server URL` field at:

```text
http://127.0.0.1:8787
```

For local integration GraphQL calls, run Warp or the CLI with:

```sh
WARP_NO_CLOUD=1 WARP_SERVER_ROOT_URL=http://127.0.0.1:8787
```

The service handles `POST /graphql/v2` for local integration create/update/list, OAuth status, GitHub auth status, environment usage lookup, cloud-environment image suggestion, startup workspace metadata, model-list, and cloud-object refresh calls. Integration config and AI conversation transcripts are stored in SQLite and never proxied to Warp cloud. Model-list calls read `GET $OPENAI_BASE_URL/models` from your local inference endpoint and fall back to `LOCAL_MODEL_LIST`, `OPENAI_MODEL`, or the built-in default.
