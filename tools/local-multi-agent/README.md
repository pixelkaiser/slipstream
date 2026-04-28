# Local Multi-Agent API

This is a local development implementation of Warp's `/ai/multi-agent` protocol. It accepts Warp's protobuf request body, calls an OpenAI-compatible `/chat/completions` endpoint, and streams protobuf response events back over server-sent events.

It is intentionally a thin single-agent adapter for the first implementation pass. Tool calls, durable orchestration, and passive suggestions are planned follow-up work in `specs/BYOK-local-multi-agent/PLAN.md`.

## Run

```sh
npm install
cp .env.example .env
npm run dev
```

Set these environment variables before starting the service:

- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_MODEL`
- `PORT` defaults to `8787`

Point Warp's BYOK `Local Multi-Agent Server URL` field at:

```text
http://127.0.0.1:8787
```
