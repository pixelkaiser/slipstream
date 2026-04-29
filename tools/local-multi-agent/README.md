# Local Multi-Agent API

This is a local development implementation of Warp's `/ai/multi-agent` protocol and the small GraphQL surface used by CLI integrations. It accepts Warp's protobuf request body, calls an OpenAI-compatible `/chat/completions` endpoint, and streams protobuf response events back over server-sent events. Assistant text is forwarded incrementally: the first provider content chunk creates the Warp assistant message and later chunks append to that same message.

It is intentionally a thin single-agent adapter. It can translate OpenAI-compatible function calls for Warp client-executed tools, but durable orchestration and rich passive suggestions remain follow-up work in `specs/BYOK-local-multi-agent/PLAN.md`.

## Run

```sh
npm install
cp .env.example .env
npm run dev
```

Set these environment variables before starting the service:

- `OPENAI_API_KEY`
- `OPENAI_BASE_URL` unless Warp sends `X-Warp-OpenAI-Base-URL` from the BYOK OpenAI Base URL setting
- `OPENAI_MODEL` globally overrides model selection and otherwise defaults to `Qwen/Qwen3.6-27B-FP8`.
- `LOCAL_MODEL_ALIASES` optionally maps Warp model IDs to provider model IDs as JSON. Built-in aliases map `auto`, `auto-efficient`, `auto-coding`, and `auto-reasoning` to `Qwen/Qwen3.6-27B-FP8`.
- `LOCAL_ENABLE_TOOLS=false` disables local tool-call advertisement. Tools are enabled by default.
- `LOCAL_MAX_HISTORY_MESSAGES` limits in-memory provider transcript messages per conversation. Defaults to `80`.
- `LOCAL_STATE_PATH` optionally persists provider transcripts to a local JSON file across service restarts.
- `LOCAL_GRAPHQL_DB_PATH` sets the SQLite path for local integration GraphQL config. Defaults to `./local-graphql.sqlite` from this package.
- `LOG_LEVEL` defaults to `info`; use `debug` to log individual SSE events.
- `PORT` defaults to `8787`

Supported tool-call names are `read_files`, `file_glob`, `grep`, `search_codebase`, `run_shell_command`, `apply_file_diffs`, and `suggest_plan`. Warp executes the tool call locally and sends the result back to this service on the next request.

The service keeps OpenAI-compatible chat history in memory per Warp conversation ID, including assistant tool calls and tool results. Restarting the service clears this local history.

For user prompts, the service also forwards supported Warp input context to the provider. This currently includes selected text, referenced attachments, attached executed shell command blocks, running command snapshots, attached text files, images, current directory, OS/shell/time, git metadata, codebase/project-rule summaries, skills, LSP server summaries, and MCP server/resource/tool summaries.

The service logs JSON lines for startup, HTTP requests, Warp multi-agent requests, provider requests, errors, and completion summaries. API keys and authorization-like fields are redacted.

Point Warp's BYOK `Local Multi-Agent Server URL` field at:

```text
http://127.0.0.1:8787
```

For local integration GraphQL calls, run Warp or the CLI with:

```sh
WARP_NO_CLOUD=1 WARP_SERVER_ROOT_URL=http://127.0.0.1:8787
```

The service handles `POST /graphql/v2` for local integration create/update/list, OAuth status, GitHub auth status, environment usage lookup, and cloud-environment image suggestion calls. Integration config is stored in SQLite and never proxied to Warp cloud.
