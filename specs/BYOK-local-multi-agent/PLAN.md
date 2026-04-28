# Local BYOK Multi-Agent Implementation Plan

## Purpose

Enable Warp OSS users to run the Warp Agent against a local OpenAI-compatible API without relying on Warp's hosted multi-agent backend.

This document is the implementation source of truth. Each implementation step should update this file by changing task status, adding notes under "Implementation Log", and adjusting details discovered during development.

## Current State

- Warp sends agent requests to `{server_root_url}/ai/multi-agent`.
- The request body is `application/x-protobuf` encoded as `warp.multi_agent.v1.Request`.
- The response is Server-Sent Events. Each SSE message `data` value is base64-url-encoded protobuf bytes for `warp.multi_agent.v1.ResponseEvent`.
- `WARP_SERVER_ROOT_URL` can override the global server root URL, but that affects all server APIs, not only agent requests.
- BYOK is enabled for OSS builds on this branch.
- The UI now has local secure storage for an `OpenAI Base URL`.
- The pinned multi-agent protobuf has no field for an OpenAI-compatible base URL, so Warp passes it to the local multi-agent service with the `X-Warp-OpenAI-Base-URL` request header when a local multi-agent server URL is configured.

## Key Decision

Prefer a targeted local AI endpoint setting over exposing only raw `WARP_SERVER_ROOT_URL`.

Raw `WARP_SERVER_ROOT_URL` is useful for advanced development, but it redirects all Warp server calls, including auth, GraphQL, cloud objects, telemetry, billing, and AI. The user-facing BYOK path should instead route only:

- `POST /ai/multi-agent`
- `POST /ai/passive-suggestions`

The BYOK UI can still expose `WARP_SERVER_ROOT_URL` as an advanced override, but the primary implementation should be a narrower local multi-agent server URL.

## Non-Goals

- Do not implement the local multi-agent service in Rust.
- Do not add an Ollama-specific README example.
- Do not replace the entire Warp hosted backend.
- Do not support every Warp tool in the MVP.
- Do not make local service auth mandatory for localhost MVP.

## Target Architecture

```text
Warp OSS client
  |
  | protobuf Request over HTTP POST
  v
Local TypeScript multi-agent service
  |
  | OpenAI-compatible chat/completions or responses API
  v
Local or self-hosted model provider
```

The local service owns:

- decoding Warp protobuf requests
- extracting prompts and conversation state
- calling an OpenAI-compatible model endpoint
- encoding Warp protobuf response events
- streaming them over SSE

Warp owns:

- UI
- task/message rendering
- tool execution after tool calls are emitted
- BYOK/local endpoint configuration

## Proposed Repository Layout

```text
tools/local-multi-agent/
  package.json
  tsconfig.json
  src/
    server.ts
    config.ts
    proto.ts
    warp_sse.ts
    request_mapper.ts
    openai_client.ts
    task_state.ts
    errors.ts
  test/
    proto.test.ts
    warp_sse.test.ts
    request_mapper.test.ts
    integration.test.ts
  README.md
Makefile
```

## Local Service Runtime Config

Environment variables:

- `PORT=8787`
- `OPENAI_BASE_URL=http://127.0.0.1:1234/v1` unless Warp sends `X-Warp-OpenAI-Base-URL`
- `OPENAI_API_KEY=...`
- `OPENAI_MODEL=Qwen/Qwen3.6-27B-FP8` by default unless overridden
- `LOCAL_MODEL_ALIASES={"auto-efficient":"Qwen/Qwen3.6-27B-FP8"}` to map Warp model IDs to provider model IDs
- `LOG_LEVEL=debug`

Supported README examples:

- LM Studio
- LocalAI
- vLLM
- Generic OpenAI-compatible endpoint

## Makefile Targets

Add a repo-root `Makefile` with targets that wrap local service and Warp app commands.

```makefile
LOCAL_AGENT_DIR := tools/local-multi-agent

.PHONY: local-agent-dev
local-agent-dev:
	npm --prefix $(LOCAL_AGENT_DIR) run dev

.PHONY: local-agent-build
local-agent-build:
	npm --prefix $(LOCAL_AGENT_DIR) run build

.PHONY: local-agent-test
local-agent-test:
	npm --prefix $(LOCAL_AGENT_DIR) test

.PHONY: local-agent-proto
local-agent-proto:
	npm --prefix $(LOCAL_AGENT_DIR) run proto:generate

.PHONY: warp-check
warp-check:
	cargo fmt --check
	cargo check -p warp --bin warp-oss --features gui

.PHONY: warp-build-oss
warp-build-oss:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color FEATURES=gui WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss ./script/macos/run --dont-open
```

## Warp Client Tasks

- [x] Add a setting for `local_multi_agent_server_root_url`.
- [x] Store the setting locally, not synced to cloud.
- [x] Expose the setting under `Settings -> Agents -> Warp Agent -> API Keys`.
- [x] Label the field `Local Multi-Agent Server URL`.
- [x] Use placeholder `http://127.0.0.1:8787`.
- [ ] Validate absolute HTTP/HTTPS URLs.
- [ ] Add a `Test connection` action that calls `/health`.
- [x] Route `ServerApi::generate_multi_agent_output` to the local multi-agent URL when configured.
- [x] Route passive suggestions to the local URL when configured.
- [x] Keep all other hosted Warp APIs on `ChannelState::server_root_url()`.
- [ ] Add an advanced BYOK/dev setting for raw `WARP_SERVER_ROOT_URL` if still desired.
- [ ] Make it visually clear that raw `WARP_SERVER_ROOT_URL` affects all server APIs.
- [x] Add focused Rust tests for URL selection.
- [ ] Add UI-level tests if there is an existing settings test pattern that fits.

## Local Service Tasks

- [x] Scaffold `tools/local-multi-agent`.
- [x] Add TypeScript build/test/dev scripts.
- [ ] Add protobuf generation from the pinned `warp-proto-apis` revision.
- [x] Decode inbound `warp.multi_agent.v1.Request` for the MVP fields.
- [x] Encode outbound `warp.multi_agent.v1.ResponseEvent` for the MVP events.
- [x] Implement SSE writer with base64-url protobuf payloads.
- [x] Implement `GET /health`.
- [x] Implement `POST /ai/multi-agent`.
- [x] Implement `POST /ai/passive-suggestions`.
- [x] Extract user text from supported request input variants.
- [x] Convert Warp request context into a simple model prompt.
- [x] Call an OpenAI-compatible streaming API.
- [x] Emit `StreamInit` as the first response event.
- [x] Emit minimal task/message client actions.
- [ ] Stream assistant output through message append actions.
- [x] Emit `StreamFinished.Done` as the final successful event.
- [x] Map generic provider failures to Warp `InternalError` finish reason.
- [ ] Add in-memory conversation/task state keyed by conversation ID.
- [ ] Add optional JSON persistence for local state.
- [x] Document local run workflow.

## MVP Protocol Behavior

The MVP should support simple user prompts without tool calls.

For each `/ai/multi-agent` request:

1. Decode protobuf request.
2. Resolve or generate `conversation_id`.
3. Generate `request_id` and `run_id`.
4. Emit `ResponseEvent.StreamInit`.
5. Find or create a task.
6. Add the user message if needed.
7. Create an assistant `Message.AgentOutput`.
8. Stream model text into that assistant message.
9. Emit `ResponseEvent.StreamFinished.Done`.

For `/ai/passive-suggestions`:

1. Decode request.
2. Emit `StreamInit`.
3. Emit `StreamFinished.Done`.

Passive suggestions can be expanded later after the main agent path works.

## Prompt Extraction MVP

Handle these request input variants first:

- `Input.UserInputs.UserQuery`
- deprecated `Input.UserQuery`
- `Input.QueryWithCannedResponse`
- `Input.InvokeSkill.user_query`
- `Input.SummarizeConversation`

Return `StreamFinished.InternalError` for unsupported variants until implemented.

## Tool Support Roadmap

MVP should not emit tool calls.

Add tools incrementally in this order:

1. `ReadFiles`
2. `FileGlob`
3. `Grep`
4. `RunShellCommand`
5. `ApplyFileDiffs`
6. `SuggestPlan`

For each tool:

- Advertise or use only the tool variants the local service supports.
- Convert OpenAI tool calls into Warp `Message.ToolCall`.
- Wait for Warp to return `ToolCallResult` in the next request.
- Feed the tool result back into the OpenAI conversation.
- Add service-level tests and a manual Warp validation note.

## Error Mapping

- Invalid API key -> `StreamFinished.InvalidApiKey`
- Provider unavailable -> `StreamFinished.LlmUnavailable`
- Context too large -> `StreamFinished.ContextWindowExceeded`
- Unknown failure -> `StreamFinished.InternalError`

Always try to end the SSE stream with `StreamFinished` so the Warp UI can leave the request in a coherent state.

## Verification Plan

Local service:

- [x] Unit test protobuf decode/encode.
- [x] Unit test streaming append event field-mask encoding.
- [x] Unit test model alias resolution.
- [x] Unit test buffered provider stream collection.
- [ ] Unit test SSE event formatting.
- [ ] Unit test prompt extraction.
- [ ] Integration test with a mock OpenAI-compatible server.
- [x] Manual smoke test for `/health`.

Warp client:

- [x] `cargo fmt --check`
- [x] `cargo check -p warp --bin warp-oss --features gui`
- [x] `make warp-check`
- [x] focused Rust tests for URL routing
- [ ] `make warp-build-oss`

End-to-end:

- [x] Start local service with `npm start`.
- [ ] Configure Warp BYOK settings to use the local service URL.
- [x] Configure OpenAI Base URL/API key in the local service environment.
- [ ] Confirm assistant output streams into Warp UI without `ExchangeNotFound`.
- [ ] Send a simple prompt in Warp Agent.
- [ ] Confirm assistant output streams into the Warp UI.
- [ ] Confirm hosted auth/cloud APIs still use the normal Warp server root.

## Implementation Order

- [x] Step 1: Persist client setting for local multi-agent URL.
- [ ] Step 2: Add BYOK UI controls for local multi-agent URL and advanced server root override.
- [x] Step 3: Route only AI agent endpoints to the local multi-agent URL.
- [x] Step 4: Add root `Makefile`.
- [x] Step 5: Scaffold TypeScript local service.
- [ ] Step 6: Generate/load Warp protobuf types.
- [x] Step 7: Implement `/health`.
- [x] Step 8: Implement protobuf SSE helpers.
- [x] Step 9: Implement simple prompt extraction.
- [x] Step 10: Implement OpenAI-compatible streaming call.
- [x] Step 11: Emit minimal Warp task/message events.
- [x] Step 12: Add local service tests.
- [x] Step 13: Add Warp routing tests.
- [x] Step 14: Add local run docs.
- [ ] Step 15: Run end-to-end manual validation.
- [x] Step 16: Update this plan with completed status and follow-up tasks.

## Implementation Log

### 2026-04-28

- Created this plan.
- Decided to prefer a targeted local multi-agent URL over using raw `WARP_SERVER_ROOT_URL` as the main BYOK control.
- Included Makefile requirement.
- Removed Ollama-specific documentation example from scope.
- Added local BYOK secure storage for `local_multi_agent_server_root_url`.
- Added the `Local Multi-Agent Server URL` editor to the BYOK API Keys UI.
- Routed `/ai/multi-agent` and `/ai/passive-suggestions` through the local URL when configured, while leaving other Warp APIs on the normal server root.
- Added focused URL-construction tests for the local multi-agent routing helper.
- Added the root `Makefile` targets for local-agent install/dev/build/start/test and Warp OSS app build.
- Scaffolded `tools/local-multi-agent` as a TypeScript service with `GET /health`, `POST /ai/multi-agent`, and `POST /ai/passive-suggestions`.
- Implemented an MVP hand-written protobuf codec for the fields needed to decode a simple Warp request and encode `StreamInit`, `ClientActions.AddMessagesToTask`, `StreamFinished.Done`, and `StreamFinished.InternalError`.
- Wired the local service to an OpenAI-compatible `/chat/completions` endpoint via environment variables.
- Added local service unit tests for protobuf request decoding and response event encoding.
- Smoke-tested the local service with a provided OpenAI-compatible endpoint using environment variables only; no test secrets were written to the repo.
- Reworked the local service provider call to use OpenAI-compatible streaming chat completions.
- Added streaming Warp response support: the service emits an initial empty `AgentOutput` message, then sends `AppendToMessageContent` events with the `message.agent_output.text` field mask for each provider chunk.
- Added a unit test for the streaming append field mask.
- Smoke-tested the streaming path with the provided OpenAI-compatible endpoint using environment variables only; the response emitted multiple SSE data events.
- Fixed SSE event encoding to use padded URL-safe base64 because the Warp client decodes events with Rust's `BASE64_URL_SAFE` engine, which rejects unpadded payloads whose lengths are not multiples of 4.
- Routed the BYOK OpenAI Base URL from the Warp setting to the local service with `X-Warp-OpenAI-Base-URL`; the local service now prefers that header over `OPENAI_BASE_URL` and only falls back to `https://api.openai.com/v1` if neither is provided.
- Smoke-tested the header path with `OPENAI_BASE_URL` unset in the service environment to confirm the request header controls the provider URL.
- Added a default Makefile `help` target and set the local service default model to `Qwen/Qwen3.6-27B-FP8` unless `OPENAI_MODEL` is supplied.
- Added local service model aliasing so Warp's internal model IDs, such as `auto-efficient`, can map to OpenAI-compatible provider model IDs.
- Made `make local-agent-start` depend on `local-agent-build` so it does not run stale compiled `dist` output.
- Temporarily changed the local service to buffer provider stream chunks and emit one final `AddMessagesToTask` event. Per-chunk `AppendToMessageContent` caused `Conversation(ExchangeNotFound)` in the Warp UI because the local service was not yet matching Warp's exchange sequencing contract.
- Added unit tests for model alias mapping and buffered provider stream collection.
- Smoke-tested the buffered response path; the local service now emits exactly three SSE events for a successful request: `StreamInit`, one `AddMessagesToTask`, and `StreamFinished.Done`.
