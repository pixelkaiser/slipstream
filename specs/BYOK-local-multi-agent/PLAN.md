# Local BYOK Multi-Agent Implementation Plan

## Purpose

Enable Warp OSS users to run the Warp Agent against a local OpenAI-compatible API without relying on Warp's hosted multi-agent backend.

This document is the implementation source of truth. Each implementation step should update this file by changing task status, adding notes under "Implementation Log", and adjusting details discovered during development.

## Current State

- Warp sends agent requests to `{server_root_url}/ai/multi-agent`.
- The request body is `application/x-protobuf` encoded as `warp.multi_agent.v1.Request`.
- The response is Server-Sent Events. Each SSE message `data` value is base64-url-encoded protobuf bytes for `warp.multi_agent.v1.ResponseEvent`.
- BYOK is enabled for OSS builds on this branch.
- The UI now has local secure storage for an `OpenAI Base URL`.
- The pinned multi-agent protobuf has no field for an OpenAI-compatible base URL, so Warp passes it to the local multi-agent service with the `X-Warp-OpenAI-Base-URL` request header when a local multi-agent server URL is configured.

## Key Decision

Use a targeted local AI endpoint setting for BYOK. The user-facing BYOK path routes only:

- `POST /ai/multi-agent`
- `POST /ai/passive-suggestions`

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
- [x] Validate absolute HTTP/HTTPS URLs.
- [ ] Add a `Test connection` action that calls `/health`.
- [x] Route `ServerApi::generate_multi_agent_output` to the local multi-agent URL when configured.
- [x] Route passive suggestions to the local URL when configured.
- [x] Keep all other hosted Warp APIs on `ChannelState::server_root_url()`.
- [x] Add focused Rust tests for URL selection.
- [ ] Add UI-level tests if there is an existing settings test pattern that fits.

## Local Service Tasks

- [x] Scaffold `tools/local-multi-agent`.
- [x] Add TypeScript build/test/dev scripts.
- [x] Add protobuf generation from the pinned `warp-proto-apis` revision.
- [x] Decode inbound `warp.multi_agent.v1.Request` for the MVP fields.
- [x] Encode outbound `warp.multi_agent.v1.ResponseEvent` for the MVP events.
- [x] Implement SSE writer with base64-url protobuf payloads.
- [x] Implement `GET /health`.
- [x] Implement `POST /ai/multi-agent`.
- [x] Implement `POST /ai/passive-suggestions`.
- [x] Extract user text from supported request input variants.
- [x] Extract selected text, referenced attachments, terminal/file/image context, environment metadata, skills, LSP, and MCP summaries from supported request fields.
- [x] Convert Warp request context into a simple model prompt.
- [x] Call an OpenAI-compatible streaming API.
- [x] Emit `StreamInit` as the first response event.
- [x] Emit minimal task/message client actions.
- [x] Emit `ReadFiles` tool-call messages from OpenAI-compatible tool calls.
- [x] Decode `ReadFilesResult` inputs from Warp for follow-up requests.
- [x] Emit `FileGlob`, `Grep`, `SearchCodebase`, `RunShellCommand`, `ApplyFileDiffs`, and `SuggestPlan` tool-call messages from OpenAI-compatible tool calls.
- [x] Decode `FileGlob`, `Grep`, `SearchCodebase`, `RunShellCommand`, `ApplyFileDiffs`, `SuggestPlan`, and generic follow-up results from Warp for follow-up requests.
- [x] Stream assistant output through message append actions.
- [x] Emit `StreamFinished.Done` as the final successful event.
- [x] Map provider authentication, quota, availability, context-window, and generic failures to Warp stream finish reasons.
- [x] Add in-memory conversation transcript state keyed by conversation ID.
- [x] Add optional JSON persistence for local state.
- [x] Document local run workflow.

## MVP Protocol Behavior

The MVP should support simple user prompts without tool calls.

For each `/ai/multi-agent` request:

1. Decode protobuf request.
2. Resolve or generate `conversation_id`.
3. Generate `request_id`.
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

Add tools incrementally in this order:

1. [x] `ReadFiles`
2. [x] `FileGlob`
3. [x] `Grep`
4. [x] `SearchCodebase`
5. [x] `RunShellCommand`
6. [x] `ApplyFileDiffs`
7. [x] `SuggestPlan`

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
- [x] Unit test log redaction.
- [x] Unit test SSE event formatting.
- [x] Unit test prompt extraction.
- [x] Unit test selected text, command, environment, image, and generic tool result extraction.
- [x] Integration test with a mock OpenAI-compatible server.
- [x] Integration test forwarding selected text context to the provider.
- [x] Integration test preserving OpenAI-compatible conversation history across turns.
- [x] Integration test translating an OpenAI-compatible `read_files` tool call to Warp SSE events.
- [x] Integration test translating every supported OpenAI-compatible tool call to Warp SSE events.
- [x] Manual smoke test for `/health`.

Warp client:

- [x] `cargo fmt --check`
- [x] `cargo check -p warp --bin warp-oss --features gui`
- [x] `make warp-check`
- [x] focused Rust tests for URL routing
- [ ] `make warp-build-oss`

End-to-end:

- [x] Start local service with `npm start`.
- [x] Configure Warp BYOK settings to use the local service URL.
- [x] Configure OpenAI Base URL/API key in the local service environment.
- [x] Confirm assistant output streams into Warp UI without `ExchangeNotFound`.
- [x] Send a simple prompt in Warp Agent.
- [x] Confirm assistant output renders in the Warp UI for non-tool-call prompts.
- [ ] Confirm hosted auth/cloud APIs still use the normal Warp server root.

## Implementation Order

- [x] Step 1: Persist client setting for local multi-agent URL.
- [x] Step 2: Add BYOK UI controls for local multi-agent URL.
- [x] Step 3: Route only AI agent endpoints to the local multi-agent URL.
- [x] Step 4: Add root `Makefile`.
- [x] Step 5: Scaffold TypeScript local service.
- [x] Step 6: Generate/load Warp protobuf types.
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
- Decided to use a targeted local multi-agent URL as the BYOK control.
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
- Fixed new-conversation task creation by emitting `CreateTask` before `AddMessagesToTask` when the inbound request has no server task in `task_context`; this addresses `Conversation(TaskNotFound)` from targeting a synthetic fallback task ID.
- Smoke-tested the taskless request path; the service now emits four SSE events for a new conversation: `StreamInit`, `CreateTask`, `AddMessagesToTask`, and `StreamFinished.Done`.
- Added JSON-line logging for local service startup, HTTP request routing, Warp request metadata, provider request metadata, errors, completion summaries, and debug-level SSE event emission. Sensitive key/token/auth fields are redacted.
- Stopped sending a synthetic `run_id` in local `StreamInit` events so Warp does not try to sync a non-hosted task with the hosted GraphQL task API.
- Added local service tests for SSE data formatting, supported prompt extraction variants, and an integration path that sends a Warp protobuf request through the local service to a mock OpenAI-compatible streaming `/chat/completions` endpoint.
- Added storage-layer validation for BYOK OpenAI Base URL and Local Multi-Agent Server URL settings; only absolute `http`/`https` URLs with a host are persisted, and trailing slashes are normalized.
- Removed raw global server-root override scope from this plan; local BYOK remains targeted to the multi-agent endpoints.
- Added the first local tool-call path for `ReadFiles`: the service advertises an OpenAI-compatible `read_files` tool, accumulates streamed provider tool-call deltas, emits Warp `Message.ToolCall` events, decodes `ReadFilesResult` inputs on follow-up requests, keeps a minimal in-memory prompt cache so tool-result turns include the original user request, and has unit/integration coverage for the protobuf and SSE paths.
- Added the remaining planned local tool-call paths: `FileGlob`, `Grep`, `RunShellCommand`, `ApplyFileDiffs`, and `SuggestPlan`. The service now advertises all supported tool schemas, converts streamed OpenAI-compatible tool calls into Warp `Message.ToolCall` events, decodes their Warp `ToolCallResult` follow-up inputs, and includes unit/integration coverage for the expanded set.

### 2026-04-29

- Replaced the minimal local prompt cache with in-memory OpenAI-compatible conversation transcripts keyed by Warp conversation ID. Follow-up turns now include prior user messages, assistant responses, assistant tool calls, and Warp tool results as provider chat messages.
- Added integration coverage for multi-turn provider history and updated the read-files tool follow-up test to assert the provider receives a real assistant-tool-result transcript instead of a flattened prompt.
- Noted that the local transcript state is process-local; restarting the service clears history unless optional persistence is added later.
- The Warp log warning `No metadata returned for conversation` is a separate hosted metadata lookup and not the source of local provider context loss.
- Added local decoding for `InputContext.selected_text`, deprecated `InputContext.executed_shell_commands`, attached text files, and current directory. User prompts sent to the provider now include an `Attached context` section, so selected terminal output such as command results is visible to the local model.
- Expanded local request decoding for more `InputContext` fields, referenced attachments, MCP summaries, image attachments, CLI-agent prompts, passive-suggestion prompts, environment creation prompts, and additional tool results that can be represented as generic provider follow-up content.
- Added specific Warp stream finish reason mapping for invalid API keys, quota/rate limits, provider unavailability, and context-window failures.
- Added `LOCAL_MAX_HISTORY_MESSAGES` transcript trimming and optional `LOCAL_STATE_PATH` JSON persistence for local conversation history.
- Added `search_codebase` tool-call support and generic provider follow-up formatting for additional Warp tool results such as MCP reads/calls, shell-output reads, skill reads, conversation fetches, and codebase search results.
- Restored incremental assistant output streaming using Warp's exchange sequencing contract: the first provider content chunk creates the assistant message with `AddMessagesToTask`, and subsequent chunks append to that same message with `AppendToMessageContent`.
- Added integration coverage that verifies a chunked provider stream emits both the initial assistant output event and a later append event with the `agent_output.text` field mask.
- Fixed the local append field mask to `agent_output.text`, which is rooted at Warp's `Message` descriptor. The previous `message.agent_output.text` path was silently ignored by the client append operation.
- Manually confirmed in Warp that streamed local agent output renders in the UI after the append field-mask fix.
- Added `npm run proto:generate` / `make local-agent-proto` to clone the pinned `warp-proto-apis` revision from `Cargo.toml`, normalize the Edition 2023 protos for JavaScript generation, and generate checked-in TypeScript descriptors with `protoc-gen-es`.
- Added local service test coverage that loads the generated descriptors and checks the hand-rolled wire field numbers against the generated request, response, client action, and message schemas.
