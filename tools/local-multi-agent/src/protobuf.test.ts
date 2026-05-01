import assert from "node:assert/strict";
import test from "node:test";
import {
  encodeAddAgentOutput,
  encodeAddConversationSummary,
  encodeAddReadFilesToolCall,
  encodeAddToolCall,
  encodeAppendAgentOutput,
  encodeCreateTask,
  decodeWarpRequest,
  encodeAgentOutput,
  encodeBase64Url,
  encodeStreamFinishedDone,
  encodeStreamFinishedContextWindowExceeded,
  encodeStreamFinishedInvalidApiKey,
  encodeStreamFinishedLlmUnavailable,
  encodeStreamFinishedQuotaLimit,
  encodeStreamInit,
} from "./protobuf.js";
import {
  RequestSchema,
  Request_InputSchema,
  Request_Input_UserInputs_UserInputSchema,
} from "./generated/warp_multi_agent/v1/request_pb.js";
import {
  ClientActionSchema,
  ClientAction_AppendToMessageContentSchema,
  ResponseEventSchema,
  ResponseEvent_ClientActionsSchema,
} from "./generated/warp_multi_agent/v1/response_pb.js";
import { MessageSchema } from "./generated/warp_multi_agent/v1/task_pb.js";

function hex(value: string): Uint8Array {
  return Uint8Array.from(Buffer.from(value.replace(/\s+/g, ""), "hex"));
}

function encodeVarint(value: number): Uint8Array {
  const bytes: number[] = [];
  let remaining = value;
  while (remaining >= 0x80) {
    bytes.push((remaining & 0x7f) | 0x80);
    remaining >>= 7;
  }
  bytes.push(remaining);
  return Uint8Array.from(bytes);
}

function lengthDelimitedField(fieldNumber: number, value: Uint8Array): Uint8Array {
  return Uint8Array.from([...encodeVarint((fieldNumber << 3) | 2), ...encodeVarint(value.length), ...value]);
}

function stringField(fieldNumber: number, value: string): Uint8Array {
  return lengthDelimitedField(fieldNumber, Buffer.from(value));
}

function varintField(fieldNumber: number, value: number): Uint8Array {
  return Uint8Array.from([...encodeVarint(fieldNumber << 3), ...encodeVarint(value)]);
}

function toolResultRequest(toolCallResult: Uint8Array): Uint8Array {
  const userInput = lengthDelimitedField(2, toolCallResult);
  const userInputs = lengthDelimitedField(1, userInput);
  const input = lengthDelimitedField(6, userInputs);
  return lengthDelimitedField(2, input);
}

test("loads generated multi-agent protobuf descriptors for hand-rolled wire compatibility", () => {
  assert.equal(RequestSchema.field.taskContext.number, 1);
  assert.equal(RequestSchema.field.input.number, 2);
  assert.equal(RequestSchema.field.settings.number, 3);
  assert.equal(RequestSchema.field.metadata.number, 4);
  assert.equal(RequestSchema.field.mcpContext.number, 6);
  assert.equal(Request_InputSchema.field.context.number, 1);
  assert.equal(Request_InputSchema.field.userQuery.number, 2);
  assert.equal(Request_InputSchema.field.toolCallResult.number, 3);
  assert.equal(Request_InputSchema.field.userInputs.number, 6);
  assert.equal(Request_Input_UserInputs_UserInputSchema.field.userQuery.number, 1);
  assert.equal(Request_Input_UserInputs_UserInputSchema.field.toolCallResult.number, 2);

  assert.equal(ResponseEventSchema.field.init.number, 1);
  assert.equal(ResponseEventSchema.field.clientActions.number, 2);
  assert.equal(ResponseEventSchema.field.finished.number, 3);
  assert.equal(ResponseEvent_ClientActionsSchema.field.actions.number, 1);
  assert.equal(ClientActionSchema.field.createTask.number, 1);
  assert.equal(ClientActionSchema.field.addMessagesToTask.number, 3);
  assert.equal(ClientActionSchema.field.appendToMessageContent.number, 5);
  assert.equal(ClientAction_AppendToMessageContentSchema.field.message.number, 1);
  assert.equal(ClientAction_AppendToMessageContentSchema.field.mask.number, 2);
  assert.equal(ClientAction_AppendToMessageContentSchema.field.taskId.number, 3);
  assert.equal(MessageSchema.field.agentOutput.number, 3);
  const agentOutputMessage = MessageSchema.field.agentOutput.message;
  assert.ok(agentOutputMessage);
  assert.equal(agentOutputMessage.field.text.number, 1);
});

test("decodes the fields needed from a Warp request", () => {
  const task = hex("0a04726f6f74");
  const taskContext = Uint8Array.from([0x0a, task.length, ...task]);
  const userQuery = hex("0a0b68656c6c6f2077617270");
  const userInput = Uint8Array.from([0x0a, userQuery.length, ...userQuery]);
  const userInputs = Uint8Array.from([0x0a, userInput.length, ...userInput]);
  const input = Uint8Array.from([0x32, userInputs.length, ...userInputs]);
  const apiKeys = hex("120a736b2d74657374696e67");
  const settings = Uint8Array.from([0x92, 0x01, apiKeys.length, ...apiKeys]);
  const metadata = hex("0a0c636f6e766572736174696f6e");
  const request = Uint8Array.from([
    0x0a,
    taskContext.length,
    ...taskContext,
    0x12,
    input.length,
    ...input,
    0x1a,
    settings.length,
    ...settings,
    0x22,
    metadata.length,
    ...metadata,
  ]);

  assert.deepEqual(
    {
      ...decodeWarpRequest(request),
      requestId: "<generated>",
    },
    {
      conversationId: "conversation",
      requestId: "<generated>",
      rootTaskId: "root",
      shouldCreateRootTask: false,
      prompt: "hello warp",
      toolResults: [],
      mcpTools: [],
      openAiApiKey: "sk-testing",
      model: undefined,
    },
  );
});

test("extracts supported prompt input variants", () => {
  const cases = [
    {
      name: "deprecated user query",
      input: lengthDelimitedField(2, stringField(1, "legacy prompt")),
      expected: "legacy prompt",
    },
    {
      name: "query with canned response",
      input: lengthDelimitedField(4, stringField(1, "canned prompt")),
      expected: "canned prompt",
    },
    {
      name: "invoke skill user query",
      input: lengthDelimitedField(17, lengthDelimitedField(2, stringField(1, "skill prompt"))),
      expected: "skill prompt",
    },
    {
      name: "summarize conversation",
      input: lengthDelimitedField(13, stringField(1, "summarize prompt")),
      expected: "summarize prompt",
    },
    {
      name: "cli agent user query",
      input: lengthDelimitedField(6, lengthDelimitedField(1, lengthDelimitedField(3, lengthDelimitedField(1, stringField(1, "cli prompt"))))),
      expected: "cli prompt",
    },
    {
      name: "create environment",
      input: lengthDelimitedField(14, stringField(1, "/repo")),
      expected: "Create a development environment for:\n/repo",
    },
  ];

  for (const item of cases) {
    const request = lengthDelimitedField(2, item.input);
    assert.equal(decodeWarpRequest(request).prompt, item.expected, item.name);
  }

  const summarizeRequest = lengthDelimitedField(2, lengthDelimitedField(13, stringField(1, "keep decisions")));
  const summarizeDecoded = decodeWarpRequest(summarizeRequest);
  assert.equal(summarizeDecoded.isSummarizationRequest, true);
  assert.equal(summarizeDecoded.summarizationPrompt, "keep decisions");

  const emptySummarizeRequest = lengthDelimitedField(2, lengthDelimitedField(13, new Uint8Array()));
  const emptySummarizeDecoded = decodeWarpRequest(emptySummarizeRequest);
  assert.equal(emptySummarizeDecoded.isSummarizationRequest, true);
  assert.equal(emptySummarizeDecoded.prompt, "");
  assert.equal(emptySummarizeDecoded.summarizationPrompt, undefined);
});

test("decodes extended context fields and images", () => {
  const os = lengthDelimitedField(2, Buffer.concat([
    stringField(1, "Linux"),
    stringField(2, "Ubuntu"),
  ]));
  const shell = lengthDelimitedField(3, Buffer.concat([
    stringField(1, "zsh"),
    stringField(2, "5.9"),
  ]));
  const image = lengthDelimitedField(7, Buffer.concat([
    lengthDelimitedField(1, Uint8Array.from([1, 2, 3])),
    stringField(2, "image/png"),
  ]));
  const codebase = lengthDelimitedField(8, Buffer.concat([
    stringField(1, "warp"),
    stringField(2, "/Users/me/warp"),
  ]));
  const git = lengthDelimitedField(11, Buffer.concat([
    stringField(1, "abc123"),
    stringField(2, "byok"),
  ]));
  const skill = lengthDelimitedField(12, lengthDelimitedField(1, Buffer.concat([
    stringField(2, "local-skill"),
    stringField(3, "Do local work"),
  ])));
  const lsp = lengthDelimitedField(13, lengthDelimitedField(1, Buffer.concat([
    stringField(1, "/Users/me/warp"),
    stringField(2, "rust-analyzer"),
  ])));
  const input = lengthDelimitedField(1, Buffer.concat([os, shell, image, codebase, git, skill, lsp]));
  const request = lengthDelimitedField(2, input);

  const decoded = decodeWarpRequest(request);

  assert.match(decoded.contextText ?? "", /Operating system: Linux Ubuntu/);
  assert.match(decoded.contextText ?? "", /Shell: zsh 5\.9/);
  assert.match(decoded.contextText ?? "", /Indexed codebases:\nwarp: \/Users\/me\/warp/);
  assert.match(decoded.contextText ?? "", /Git: branch=byok, head=abc123/);
  assert.match(decoded.contextText ?? "", /Available skills:\nlocal-skill: Do local work/);
  assert.match(decoded.contextText ?? "", /Available LSP servers:\nrust-analyzer/);
  assert.deepEqual(decoded.contextImages, [{ data: "AQID", mimeType: "image/png" }]);
});

test("decodes attached selected text and command context", () => {
  const selectedText = lengthDelimitedField(6, stringField(1, "Filesystem Size\n/dev/md3 1.5T"));
  const executedShellCommand = lengthDelimitedField(5, Buffer.concat([
    stringField(1, "df -h"),
    stringField(2, "Filesystem Size\n/dev/md3 1.5T"),
    varintField(3, 0),
  ]));
  const directory = lengthDelimitedField(1, stringField(1, "/root"));
  const context = lengthDelimitedField(1, Buffer.concat([
    directory,
    selectedText,
    executedShellCommand,
  ]));
  const userInput = lengthDelimitedField(1, stringField(1, "explain this output"));
  const userInputs = lengthDelimitedField(6, lengthDelimitedField(1, userInput));
  const request = lengthDelimitedField(2, Buffer.concat([context, userInputs]));

  const decoded = decodeWarpRequest(request);

  assert.equal(decoded.prompt, "explain this output");
  assert.match(decoded.contextText ?? "", /Current directory: \/root/);
  assert.match(decoded.contextText ?? "", /Selected text:\nFilesystem Size/);
  assert.match(decoded.contextText ?? "", /Executed shell command:\nCommand: df -h/);
  assert.match(decoded.contextText ?? "", /Exit code: 0/);
});

test("marks requests without a task context as needing root task creation", () => {
  const userQuery = hex("0a0b68656c6c6f2077617270");
  const userInput = Uint8Array.from([0x0a, userQuery.length, ...userQuery]);
  const userInputs = Uint8Array.from([0x0a, userInput.length, ...userInput]);
  const input = Uint8Array.from([0x32, userInputs.length, ...userInputs]);
  const request = Uint8Array.from([0x12, input.length, ...input]);

  const decoded = decodeWarpRequest(request);

  assert.equal(decoded.shouldCreateRootTask, true);
  assert.notEqual(decoded.rootTaskId, "root");
});

test("decodes read_files tool call results from user inputs", () => {
  const fileContent = Buffer.concat([
    stringField(1, "src/main.ts"),
    stringField(2, "console.log('warp');"),
  ]);
  const textFilesSuccess = lengthDelimitedField(1, fileContent);
  const readFilesResult = lengthDelimitedField(1, textFilesSuccess);
  const toolCallResult = Buffer.concat([
    stringField(1, "call-read-files"),
    lengthDelimitedField(3, readFilesResult),
  ]);
  const userInput = lengthDelimitedField(2, toolCallResult);
  const userInputs = lengthDelimitedField(1, userInput);
  const input = lengthDelimitedField(6, userInputs);
  const request = lengthDelimitedField(2, input);

  const decoded = decodeWarpRequest(request);

  assert.deepEqual(decoded.toolResults, [
    {
      type: "read_files",
      toolCallId: "call-read-files",
      files: [{ filePath: "src/main.ts", content: "console.log('warp');" }],
      error: undefined,
    },
  ]);
  assert.match(decoded.prompt, /src\/main\.ts/);
  assert.match(decoded.prompt, /console\.log/);
});

test("decodes non-file tool call results from user inputs", () => {
  const shellFinished = Buffer.concat([
    stringField(1, "ok"),
    varintField(2, 0),
  ]);
  const runShellResult = Buffer.concat([
    stringField(1, "call-shell"),
    lengthDelimitedField(2, Buffer.concat([
      stringField(3, "pwd"),
      lengthDelimitedField(5, shellFinished),
    ])),
  ]);
  assert.deepEqual(decodeWarpRequest(toolResultRequest(runShellResult)).toolResults, [
    {
      type: "run_shell_command",
      toolCallId: "call-shell",
      command: "pwd",
      output: "ok",
      exitCode: 0,
    },
  ]);

  const grepMatch = Buffer.concat([
    stringField(1, "src/main.ts"),
    lengthDelimitedField(2, varintField(1, 12)),
  ]);
  const grepResult = Buffer.concat([
    stringField(1, "call-grep"),
    lengthDelimitedField(8, lengthDelimitedField(1, lengthDelimitedField(1, grepMatch))),
  ]);
  assert.deepEqual(decodeWarpRequest(toolResultRequest(grepResult)).toolResults, [
    {
      type: "grep",
      toolCallId: "call-grep",
      matchedFiles: [{ filePath: "src/main.ts", lineNumbers: [12] }],
      error: undefined,
    },
  ]);

  const globMatch = stringField(1, "src/main.ts");
  const globResult = Buffer.concat([
    stringField(1, "call-glob"),
    lengthDelimitedField(15, lengthDelimitedField(1, lengthDelimitedField(1, globMatch))),
  ]);
  assert.deepEqual(decodeWarpRequest(toolResultRequest(globResult)).toolResults, [
    {
      type: "file_glob",
      toolCallId: "call-glob",
      matchedFiles: ["src/main.ts"],
      warnings: undefined,
      error: undefined,
    },
  ]);
});

test("decodes additional tool call results as generic provider content", () => {
  const file = Buffer.concat([
    stringField(1, "src/lib.ts"),
    stringField(2, "export {};"),
  ]);
  const searchCodebaseResult = Buffer.concat([
    stringField(1, "call-search"),
    lengthDelimitedField(4, lengthDelimitedField(1, lengthDelimitedField(1, file))),
  ]);
  assert.deepEqual(decodeWarpRequest(toolResultRequest(searchCodebaseResult)).toolResults, [
    {
      type: "generic",
      toolCallId: "call-search",
      name: "search_codebase",
      content: "File: src/lib.ts\nexport {};",
      error: undefined,
    },
  ]);

  const readShellOutput = Buffer.concat([
    stringField(1, "call-read-shell"),
    lengthDelimitedField(22, Buffer.concat([
      stringField(1, "npm test"),
      lengthDelimitedField(3, Buffer.concat([
        stringField(1, "ok"),
        varintField(2, 0),
        stringField(3, "cmd-1"),
      ])),
    ])),
  ]);
  assert.deepEqual(decodeWarpRequest(toolResultRequest(readShellOutput)).toolResults, [
    {
      type: "generic",
      toolCallId: "call-read-shell",
      name: "read_shell_command_output",
      content: "Command: npm test\nCommand ID: cmd-1\nExit code: 0\nOutput:\nok",
      error: undefined,
    },
  ]);

  const mcpResourceContent = Buffer.concat([
    stringField(1, "mcp://local/resource"),
    lengthDelimitedField(2, stringField(1, "resource text")),
  ]);
  const readMcpResource = Buffer.concat([
    stringField(1, "call-mcp-resource"),
    lengthDelimitedField(11, lengthDelimitedField(1, lengthDelimitedField(1, mcpResourceContent))),
  ]);
  assert.deepEqual(decodeWarpRequest(toolResultRequest(readMcpResource)).toolResults, [
    {
      type: "generic",
      toolCallId: "call-mcp-resource",
      name: "read_mcp_resource",
      content: "Resource: mcp://local/resource\nresource text",
      error: undefined,
    },
  ]);

  const callMcpTool = Buffer.concat([
    stringField(1, "call-mcp-tool"),
    lengthDelimitedField(12, lengthDelimitedField(1, lengthDelimitedField(1, lengthDelimitedField(1, stringField(1, "tool text"))))),
  ]);
  assert.deepEqual(decodeWarpRequest(toolResultRequest(callMcpTool)).toolResults, [
    {
      type: "generic",
      toolCallId: "call-mcp-tool",
      name: "call_mcp_tool",
      content: "tool text",
      error: undefined,
    },
  ]);
});

test("encodes response events as protobuf payloads", () => {
  assert.equal(encodeBase64Url(encodeStreamInit("c", "r")), "CgYKAWMSAXI=");
  assert.equal(encodeBase64Url(encodeStreamFinishedDone()), "GgISAA==");
  assert.ok(encodeStreamFinishedInvalidApiKey("mock-model").length > 0);
  assert.ok(encodeStreamFinishedLlmUnavailable().length > 0);
  assert.ok(encodeStreamFinishedContextWindowExceeded().length > 0);
  assert.ok(encodeStreamFinishedQuotaLimit().length > 0);
  assert.ok(encodeAgentOutput({ taskId: "root", requestId: "req", text: "ok" }).length > 0);
  assert.ok(encodeAddConversationSummary({ messageId: "message", taskId: "root", requestId: "req", text: "summary" }).length > 0);
  assert.ok(encodeCreateTask({ taskId: "root", description: "hello" }).length > 0);
  const toolCall = encodeAddReadFilesToolCall({
    messageId: "message",
    taskId: "root",
    requestId: "request",
    toolCallId: "call-read-files",
    files: [{ name: "src/main.ts", lineRanges: [{ start: 1, end: 10 }] }],
  });
  assert.ok(Buffer.from(toolCall).includes("call-read-files"));
  assert.ok(Buffer.from(toolCall).includes("src/main.ts"));
  const otherToolCalls = [
    encodeAddToolCall({
      messageId: "message",
      taskId: "root",
      requestId: "request",
      toolCallId: "call-shell",
      tool: { type: "run_shell_command", command: "pwd", isReadOnly: true },
    }),
    encodeAddToolCall({
      messageId: "message",
      taskId: "root",
      requestId: "request",
      toolCallId: "call-grep",
      tool: { type: "grep", queries: ["TODO"], path: "." },
    }),
    encodeAddToolCall({
      messageId: "message",
      taskId: "root",
      requestId: "request",
      toolCallId: "call-search",
      tool: { type: "search_codebase", query: "auth flow", pathFilters: ["src"] },
    }),
    encodeAddToolCall({
      messageId: "message",
      taskId: "root",
      requestId: "request",
      toolCallId: "call-glob",
      tool: { type: "file_glob", patterns: ["*.ts"], searchDir: "src" },
    }),
    encodeAddToolCall({
      messageId: "message",
      taskId: "root",
      requestId: "request",
      toolCallId: "call-apply",
      tool: {
        type: "apply_file_diffs",
        summary: "edit file",
        diffs: [{ filePath: "src/main.ts", search: "old", replace: "new" }],
      },
    }),
    encodeAddToolCall({
      messageId: "message",
      taskId: "root",
      requestId: "request",
      toolCallId: "call-plan",
      tool: { type: "suggest_plan", summary: "plan", tasks: [{ description: "Do work" }] },
    }),
    encodeAddToolCall({
      messageId: "message",
      taskId: "root",
      requestId: "request",
      toolCallId: "call-mcp-resource",
      tool: { type: "read_mcp_resource", uri: "mcp://local/resource", serverId: "local-server" },
    }),
    encodeAddToolCall({
      messageId: "message",
      taskId: "root",
      requestId: "request",
      toolCallId: "call-mcp-tool",
      tool: { type: "call_mcp_tool", name: "list_items", serverId: "local-server", args: { limit: 2 } },
    }),
  ];
  assert.ok(otherToolCalls.every((encoded) => encoded.length > 0));
  assert.ok(Buffer.from(otherToolCalls.at(-2) ?? []).includes("mcp://local/resource"));
  assert.ok(Buffer.from(otherToolCalls.at(-1) ?? []).includes("list_items"));
});

test("encodes streaming agent output append events with the text field mask", () => {
  const initial = encodeAddAgentOutput({
    messageId: "message",
    taskId: "root",
    requestId: "request",
    text: "",
  });
  const append = encodeAppendAgentOutput({
    messageId: "message",
    taskId: "root",
    requestId: "request",
    text: "chunk",
  });

  assert.ok(initial.length > 0);
  assert.ok(Buffer.from(append).includes("agent_output.text"));
  assert.equal(Buffer.from(append).includes("message.agent_output.text"), false);
});
