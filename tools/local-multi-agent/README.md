# Local Multi-Agent API

This is a local development implementation of Warp's `/ai/multi-agent` protocol. It accepts Warp's protobuf request body, calls an OpenAI-compatible `/chat/completions` endpoint, and streams protobuf response events back over server-sent events.

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
- `LOG_LEVEL` defaults to `info`; use `debug` to log individual SSE events.
- `PORT` defaults to `8787`

Supported tool-call names are `read_files`, `file_glob`, `grep`, `run_shell_command`, `apply_file_diffs`, and `suggest_plan`. Warp executes the tool call locally and sends the result back to this service on the next request.

The service logs JSON lines for startup, HTTP requests, Warp multi-agent requests, provider requests, errors, and completion summaries. API keys and authorization-like fields are redacted.

Point Warp's BYOK `Local Multi-Agent Server URL` field at:

```text
http://127.0.0.1:8787
```
