import { randomUUID } from "node:crypto";

const textDecoder = new TextDecoder();
const textEncoder = new TextEncoder();

type ProtoField = {
  number: number;
  wireType: number;
  raw: Uint8Array;
};

export type WarpRequestSummary = {
  conversationId: string;
  requestId: string;
  rootTaskId: string;
  shouldCreateRootTask: boolean;
  prompt: string;
  contextText?: string;
  toolResults: WarpToolResult[];
  openAiApiKey?: string;
  model?: string;
};

export type ReadFilesToolCallFile = {
  name: string;
  lineRanges?: Array<{ start: number; end: number }>;
};

export type ReadFilesToolResult = {
  type: "read_files";
  toolCallId: string;
  files: Array<{ filePath: string; content: string }>;
  error?: string;
};

export type RunShellCommandToolCall = {
  type: "run_shell_command";
  command: string;
  isReadOnly?: boolean;
  isRisky?: boolean;
  usesPager?: boolean;
  waitUntilComplete?: boolean;
};

export type GrepToolCall = {
  type: "grep";
  queries: string[];
  path?: string;
};

export type FileGlobToolCall = {
  type: "file_glob";
  patterns: string[];
  searchDir?: string;
  maxMatches?: number;
  maxDepth?: number;
  minDepth?: number;
};

export type ApplyFileDiffsToolCall = {
  type: "apply_file_diffs";
  summary: string;
  diffs?: Array<{ filePath: string; search?: string; replace?: string }>;
  newFiles?: Array<{ filePath: string; content: string }>;
  deletedFiles?: Array<{ filePath: string }>;
  v4aUpdates?: Array<{
    filePath: string;
    moveTo?: string;
    hunks: Array<{
      changeContext?: string[];
      preContext?: string;
      old?: string;
      new?: string;
      postContext?: string;
    }>;
  }>;
};

export type SuggestPlanToolCall = {
  type: "suggest_plan";
  summary: string;
  tasks: Array<{ description: string }>;
};

export type WarpToolCall =
  | ({ toolCallId: string } & {
      tool: RunShellCommandToolCall | { type: "read_files"; files: ReadFilesToolCallFile[] } | GrepToolCall | FileGlobToolCall | ApplyFileDiffsToolCall | SuggestPlanToolCall;
    });

export type WarpToolResult =
  | ReadFilesToolResult
  | { type: "run_shell_command"; toolCallId: string; command?: string; output?: string; exitCode?: number; error?: string }
  | { type: "grep"; toolCallId: string; matchedFiles: Array<{ filePath: string; lineNumbers: number[] }>; error?: string }
  | { type: "file_glob"; toolCallId: string; matchedFiles: string[]; warnings?: string; error?: string }
  | { type: "apply_file_diffs"; toolCallId: string; updatedFiles: string[]; deletedFiles: string[]; error?: string }
  | { type: "suggest_plan"; toolCallId: string; status: "accepted" | "edited"; planText?: string };

function concat(parts: Uint8Array[]): Uint8Array {
  const length = parts.reduce((sum, part) => sum + part.length, 0);
  const output = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    output.set(part, offset);
    offset += part.length;
  }
  return output;
}

function encodeVarint(value: number | bigint): Uint8Array {
  let remaining = BigInt(value);
  const bytes: number[] = [];
  while (remaining >= 0x80n) {
    bytes.push(Number((remaining & 0x7fn) | 0x80n));
    remaining >>= 7n;
  }
  bytes.push(Number(remaining));
  return Uint8Array.from(bytes);
}

function readVarint(bytes: Uint8Array, offset: number): [number, number] {
  let result = 0n;
  let shift = 0n;
  let cursor = offset;

  while (cursor < bytes.length) {
    const byte = bytes[cursor++];
    result |= BigInt(byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) {
      return [Number(result), cursor];
    }
    shift += 7n;
  }

  throw new Error("Malformed protobuf varint.");
}

function tag(fieldNumber: number, wireType: number): Uint8Array {
  return encodeVarint((fieldNumber << 3) | wireType);
}

function stringField(fieldNumber: number, value: string | undefined): Uint8Array {
  if (!value) {
    return new Uint8Array();
  }

  const encoded = textEncoder.encode(value);
  return concat([tag(fieldNumber, 2), encodeVarint(encoded.length), encoded]);
}

function int64Field(fieldNumber: number, value: number): Uint8Array {
  return concat([tag(fieldNumber, 0), encodeVarint(value)]);
}

function boolField(fieldNumber: number, value: boolean | undefined): Uint8Array {
  if (value == null) {
    return new Uint8Array();
  }

  return concat([tag(fieldNumber, 0), encodeVarint(value ? 1 : 0)]);
}

function messageField(fieldNumber: number, value: Uint8Array): Uint8Array {
  return concat([tag(fieldNumber, 2), encodeVarint(value.length), value]);
}

function decodeFields(bytes: Uint8Array): ProtoField[] {
  const fields: ProtoField[] = [];
  let cursor = 0;

  while (cursor < bytes.length) {
    const [fieldTag, afterTag] = readVarint(bytes, cursor);
    cursor = afterTag;
    const number = fieldTag >> 3;
    const wireType = fieldTag & 0x7;

    if (wireType === 0) {
      const start = cursor;
      const [, next] = readVarint(bytes, cursor);
      cursor = next;
      fields.push({ number, wireType, raw: bytes.subarray(start, cursor) });
    } else if (wireType === 2) {
      const [length, afterLength] = readVarint(bytes, cursor);
      cursor = afterLength;
      const end = cursor + length;
      fields.push({ number, wireType, raw: bytes.subarray(cursor, end) });
      cursor = end;
    } else if (wireType === 5) {
      fields.push({ number, wireType, raw: bytes.subarray(cursor, cursor + 4) });
      cursor += 4;
    } else if (wireType === 1) {
      fields.push({ number, wireType, raw: bytes.subarray(cursor, cursor + 8) });
      cursor += 8;
    } else {
      throw new Error(`Unsupported protobuf wire type ${wireType}.`);
    }
  }

  return fields;
}

function field(fields: ProtoField[], number: number): ProtoField | undefined {
  return fields.find((item) => item.number === number);
}

function fields(fields: ProtoField[], number: number): ProtoField[] {
  return fields.filter((item) => item.number === number);
}

function message(fieldValue: ProtoField | undefined): ProtoField[] {
  if (!fieldValue || fieldValue.wireType !== 2) {
    return [];
  }

  return decodeFields(fieldValue.raw);
}

function stringValue(fieldValue: ProtoField | undefined): string | undefined {
  if (!fieldValue || fieldValue.wireType !== 2) {
    return undefined;
  }

  return textDecoder.decode(fieldValue.raw);
}

function intValue(fieldValue: ProtoField | undefined): number | undefined {
  if (!fieldValue || fieldValue.wireType !== 0) {
    return undefined;
  }

  return readVarint(fieldValue.raw, 0)[0];
}

function firstNonEmpty(...values: Array<string | undefined>): string | undefined {
  return values.find((value) => value != null && value.trim().length > 0)?.trim();
}

function decodeRootTaskId(requestFields: ProtoField[]): string | undefined {
  const taskContext = message(field(requestFields, 1));
  const firstTask = fields(taskContext, 1)[0];
  return stringValue(field(message(firstTask), 1));
}

function decodeMetadata(requestFields: ProtoField[]): Pick<WarpRequestSummary, "conversationId"> {
  const metadata = message(field(requestFields, 4));
  return {
    conversationId: stringValue(field(metadata, 1)) ?? randomUUID(),
  };
}

function decodeSettings(requestFields: ProtoField[]): Pick<WarpRequestSummary, "openAiApiKey" | "model"> {
  const settings = message(field(requestFields, 3));
  const modelConfig = message(field(settings, 1));
  const apiKeys = message(field(settings, 18));

  return {
    model: firstNonEmpty(
      stringValue(field(modelConfig, 1)),
      stringValue(field(modelConfig, 3)),
      stringValue(field(modelConfig, 4)),
    ),
    openAiApiKey: firstNonEmpty(stringValue(field(apiKeys, 2))),
  };
}

function decodeInputPrompt(requestFields: ProtoField[]): string | undefined {
  const input = message(field(requestFields, 2));
  const userInputs = message(field(input, 6));
  for (const userInputField of fields(userInputs, 1)) {
    const userInput = message(userInputField);
    const userQuery = message(field(userInput, 1));
    const query = stringValue(field(userQuery, 1));
    if (query) {
      return query;
    }
  }

  const deprecatedUserQuery = message(field(input, 2));
  const cannedResponse = message(field(input, 4));
  const autoCodeDiff = message(field(input, 5));
  const createNewProject = message(field(input, 10));
  const cloneRepository = message(field(input, 11));
  const summarizeConversation = message(field(input, 13));
  const invokeSkill = message(field(input, 17));
  const invokeSkillUserQuery = message(field(invokeSkill, 2));

  return firstNonEmpty(
    stringValue(field(deprecatedUserQuery, 1)),
    stringValue(field(cannedResponse, 1)),
    stringValue(field(autoCodeDiff, 1)),
    stringValue(field(createNewProject, 1)),
    stringValue(field(cloneRepository, 1)),
    stringValue(field(summarizeConversation, 1)),
    stringValue(field(invokeSkillUserQuery, 1)),
  );
}

function decodeFileContext(fileFields: ProtoField[]): string | undefined {
  const content = message(field(fileFields, 1));
  const filePath = stringValue(field(content, 1));
  const text = stringValue(field(content, 2));
  if (!filePath || !text) {
    return undefined;
  }

  const lineRange = message(field(content, 3));
  const start = intValue(field(lineRange, 1));
  const end = intValue(field(lineRange, 2));
  const range = start != null && end != null ? `:${start}-${end}` : "";
  return [`File: ${filePath}${range}`, text].join("\n");
}

function decodeAttachedContext(requestFields: ProtoField[]): string | undefined {
  const input = message(field(requestFields, 2));
  const context = message(field(input, 1));
  if (!context.length) {
    return undefined;
  }

  const sections: string[] = [];

  const directory = message(field(context, 1));
  const pwd = stringValue(field(directory, 1));
  if (pwd) {
    sections.push(`Current directory: ${pwd}`);
  }

  for (const selectedTextField of fields(context, 6)) {
    const text = stringValue(field(message(selectedTextField), 1));
    if (text) {
      sections.push(`Selected text:\n${text}`);
    }
  }

  for (const commandField of fields(context, 5)) {
    const command = message(commandField);
    const commandText = stringValue(field(command, 1));
    const output = stringValue(field(command, 2));
    const exitCode = intValue(field(command, 3));
    if (commandText || output) {
      sections.push([
        "Executed shell command:",
        commandText ? `Command: ${commandText}` : undefined,
        exitCode != null ? `Exit code: ${exitCode}` : undefined,
        output ? `Output:\n${output}` : undefined,
      ].filter((line): line is string => line != null).join("\n"));
    }
  }

  for (const fileField of fields(context, 9)) {
    const decoded = decodeFileContext(message(fileField));
    if (decoded) {
      sections.push(decoded);
    }
  }

  return sections.length ? sections.join("\n\n") : undefined;
}

function decodeFileContent(fileContentFields: ProtoField[]): { filePath: string; content: string } | undefined {
  const filePath = stringValue(field(fileContentFields, 1));
  const content = stringValue(field(fileContentFields, 2));
  if (!filePath || content == null) {
    return undefined;
  }

  return { filePath, content };
}

function decodeReadFilesResult(toolCallResultFields: ProtoField[]): ReadFilesToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  if (!toolCallId) {
    return undefined;
  }

  const readFiles = message(field(toolCallResultFields, 3));
  if (!readFiles.length) {
    return undefined;
  }

  const error = stringValue(field(message(field(readFiles, 2)), 1));
  const files: Array<{ filePath: string; content: string }> = [];

  const textFilesSuccess = message(field(readFiles, 1));
  for (const fileContentField of fields(textFilesSuccess, 1)) {
    const decoded = decodeFileContent(message(fileContentField));
    if (decoded) {
      files.push(decoded);
    }
  }

  const anyFilesSuccess = message(field(readFiles, 3));
  for (const anyFileContentField of fields(anyFilesSuccess, 1)) {
    const textContent = message(field(message(anyFileContentField), 2));
    const decoded = decodeFileContent(textContent);
    if (decoded) {
      files.push(decoded);
    }
  }

  return { type: "read_files", toolCallId, files, error };
}

function decodeRunShellCommandResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  if (!toolCallId) {
    return undefined;
  }

  const runShellCommand = message(field(toolCallResultFields, 2));
  if (!runShellCommand.length) {
    return undefined;
  }

  const command = stringValue(field(runShellCommand, 3));
  const commandFinished = message(field(runShellCommand, 5));
  const permissionDenied = message(field(runShellCommand, 6));
  if (commandFinished.length) {
    return {
      type: "run_shell_command",
      toolCallId,
      command,
      output: stringValue(field(commandFinished, 1)) ?? "",
      exitCode: intValue(field(commandFinished, 2)) ?? 0,
    };
  }
  if (permissionDenied.length) {
    return {
      type: "run_shell_command",
      toolCallId,
      command,
      error: "Permission denied.",
    };
  }

  return {
    type: "run_shell_command",
    toolCallId,
    command,
    output: stringValue(field(runShellCommand, 1)) ?? "",
    exitCode: intValue(field(runShellCommand, 2)),
  };
}

function decodeGrepResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  if (!toolCallId) {
    return undefined;
  }

  const grep = message(field(toolCallResultFields, 8));
  if (!grep.length) {
    return undefined;
  }

  const error = stringValue(field(message(field(grep, 2)), 1));
  const success = message(field(grep, 1));
  const matchedFiles = fields(success, 1).flatMap((matchField) => {
    const match = message(matchField);
    const filePath = stringValue(field(match, 1));
    if (!filePath) {
      return [];
    }

    return [{
      filePath,
      lineNumbers: fields(match, 2).flatMap((lineField) => {
        const lineNumber = intValue(field(message(lineField), 1));
        return lineNumber == null ? [] : [lineNumber];
      }),
    }];
  });

  return { type: "grep", toolCallId, matchedFiles, error };
}

function decodeFileGlobResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  if (!toolCallId) {
    return undefined;
  }

  const fileGlob = message(field(toolCallResultFields, 9));
  if (fileGlob.length) {
    const error = stringValue(field(message(field(fileGlob, 2)), 1));
    const matchedFiles = (stringValue(field(message(field(fileGlob, 1)), 1)) ?? "")
      .split(/\r?\n/)
      .map((path) => path.trim())
      .filter(Boolean);
    return { type: "file_glob", toolCallId, matchedFiles, error };
  }

  const fileGlobV2 = message(field(toolCallResultFields, 15));
  if (!fileGlobV2.length) {
    return undefined;
  }

  const error = stringValue(field(message(field(fileGlobV2, 2)), 1));
  const success = message(field(fileGlobV2, 1));
  const matchedFiles = fields(success, 1).flatMap((matchField) => {
    const filePath = stringValue(field(message(matchField), 1));
    return filePath ? [filePath] : [];
  });
  const warnings = stringValue(field(success, 2));
  return { type: "file_glob", toolCallId, matchedFiles, warnings, error };
}

function decodeApplyFileDiffsResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  if (!toolCallId) {
    return undefined;
  }

  const applyFileDiffs = message(field(toolCallResultFields, 5));
  if (!applyFileDiffs.length) {
    return undefined;
  }

  const error = stringValue(field(message(field(applyFileDiffs, 2)), 1));
  const success = message(field(applyFileDiffs, 1));
  const updatedFiles = [
    ...fields(success, 1).flatMap((fileField) => {
      const filePath = stringValue(field(message(fileField), 1));
      return filePath ? [filePath] : [];
    }),
    ...fields(success, 2).flatMap((updatedFileField) => {
      const filePath = stringValue(field(message(field(message(updatedFileField), 1)), 1));
      return filePath ? [filePath] : [];
    }),
  ];
  const deletedFiles = fields(success, 3).flatMap((deletedFileField) => {
    const filePath = stringValue(field(message(deletedFileField), 1));
    return filePath ? [filePath] : [];
  });

  return { type: "apply_file_diffs", toolCallId, updatedFiles, deletedFiles, error };
}

function decodeSuggestPlanResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  if (!toolCallId) {
    return undefined;
  }

  const suggestPlan = message(field(toolCallResultFields, 6));
  if (!suggestPlan.length) {
    return undefined;
  }

  const editedPlan = message(field(suggestPlan, 2));
  if (editedPlan.length) {
    return {
      type: "suggest_plan",
      toolCallId,
      status: "edited",
      planText: stringValue(field(editedPlan, 1)) ?? "",
    };
  }

  return { type: "suggest_plan", toolCallId, status: "accepted" };
}

function decodeToolResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  return decodeReadFilesResult(toolCallResultFields)
    ?? decodeRunShellCommandResult(toolCallResultFields)
    ?? decodeGrepResult(toolCallResultFields)
    ?? decodeFileGlobResult(toolCallResultFields)
    ?? decodeApplyFileDiffsResult(toolCallResultFields)
    ?? decodeSuggestPlanResult(toolCallResultFields);
}

function decodeToolResults(requestFields: ProtoField[]): WarpToolResult[] {
  const input = message(field(requestFields, 2));
  const results: WarpToolResult[] = [];

  const userInputs = message(field(input, 6));
  for (const userInputField of fields(userInputs, 1)) {
    const userInput = message(userInputField);
    const decoded = decodeToolResult(message(field(userInput, 2)));
    if (decoded) {
      results.push(decoded);
    }
  }

  const deprecatedToolCallResult = decodeToolResult(message(field(input, 3)));
  if (deprecatedToolCallResult) {
    results.push(deprecatedToolCallResult);
  }

  return results;
}

export function decodeWarpRequest(bytes: Uint8Array): WarpRequestSummary {
  const requestFields = decodeFields(bytes);
  const { conversationId } = decodeMetadata(requestFields);
  const { openAiApiKey, model } = decodeSettings(requestFields);
  const decodedRootTaskId = decodeRootTaskId(requestFields);
  const toolResults = decodeToolResults(requestFields);

  return {
    conversationId,
    requestId: randomUUID(),
    rootTaskId: decodedRootTaskId ?? randomUUID(),
    shouldCreateRootTask: decodedRootTaskId == null,
    prompt: decodeInputPrompt(requestFields) ?? formatToolResultsPrompt(toolResults),
    contextText: decodeAttachedContext(requestFields),
    toolResults,
    openAiApiKey,
    model,
  };
}

export function encodeStreamInit(conversationId: string, requestId: string): Uint8Array {
  const init = concat([
    stringField(1, conversationId),
    stringField(2, requestId),
  ]);

  return messageField(1, init);
}

function encodeTask(params: {
  taskId: string;
  description: string;
}): Uint8Array {
  return concat([
    stringField(1, params.taskId),
    stringField(2, params.description),
  ]);
}

export function encodeCreateTask(params: {
  taskId: string;
  description: string;
}): Uint8Array {
  const createTask = messageField(1, encodeTask(params));
  const clientAction = messageField(1, createTask);
  const clientActions = messageField(1, clientAction);

  return messageField(2, clientActions);
}

function encodeTimestamp(date: Date): Uint8Array {
  return int64Field(1, Math.floor(date.getTime() / 1000));
}

function encodeLineRange(range: { start: number; end: number }): Uint8Array {
  return concat([
    int64Field(1, range.start),
    int64Field(2, range.end),
  ]);
}

function optionalIntField(fieldNumber: number, value: number | undefined): Uint8Array {
  return value == null ? new Uint8Array() : int64Field(fieldNumber, value);
}

function encodeFieldMask(paths: string[]): Uint8Array {
  return concat(paths.map((path) => stringField(1, path)));
}

function encodeAgentOutputMessage(params: {
  messageId: string;
  taskId: string;
  requestId: string;
  text: string;
}): Uint8Array {
  const agentOutput = stringField(1, params.text);

  return concat([
    stringField(1, params.messageId),
    messageField(3, agentOutput),
    stringField(11, params.taskId),
    stringField(13, params.requestId),
    messageField(14, encodeTimestamp(new Date())),
  ]);
}

export function encodeAddAgentOutput(params: {
  messageId: string;
  taskId: string;
  requestId: string;
  text: string;
}): Uint8Array {
  const outputMessage = encodeAgentOutputMessage(params);
  const addMessagesToTask = concat([
    stringField(1, params.taskId),
    messageField(2, outputMessage),
  ]);
  const clientAction = messageField(3, addMessagesToTask);
  const clientActions = messageField(1, clientAction);

  return messageField(2, clientActions);
}

function encodeReadFilesToolCall(params: {
  toolCallId: string;
  files: ReadFilesToolCallFile[];
}): Uint8Array {
  const readFiles = concat(params.files.map((file) => messageField(1, concat([
    stringField(1, file.name),
    ...((file.lineRanges ?? []).map((range) => messageField(2, encodeLineRange(range)))),
  ]))));

  return concat([
    stringField(1, params.toolCallId),
    messageField(5, readFiles),
  ]);
}

function encodeRunShellCommandToolCall(params: RunShellCommandToolCall & { toolCallId: string }): Uint8Array {
  const runShellCommand = concat([
    stringField(1, params.command),
    boolField(2, params.isReadOnly),
    boolField(3, params.usesPager),
    boolField(5, params.isRisky),
    boolField(6, params.waitUntilComplete),
  ]);

  return concat([
    stringField(1, params.toolCallId),
    messageField(2, runShellCommand),
  ]);
}

function encodeGrepToolCall(params: GrepToolCall & { toolCallId: string }): Uint8Array {
  const grep = concat([
    ...params.queries.map((query) => stringField(1, query)),
    stringField(2, params.path ?? "."),
  ]);

  return concat([
    stringField(1, params.toolCallId),
    messageField(9, grep),
  ]);
}

function encodeFileGlobToolCall(params: FileGlobToolCall & { toolCallId: string }): Uint8Array {
  const fileGlob = concat([
    ...params.patterns.map((pattern) => stringField(1, pattern)),
    stringField(2, params.searchDir ?? "."),
    optionalIntField(3, params.maxMatches),
    optionalIntField(4, params.maxDepth),
    optionalIntField(5, params.minDepth),
  ]);

  return concat([
    stringField(1, params.toolCallId),
    messageField(15, fileGlob),
  ]);
}

function encodeApplyFileDiffsToolCall(params: ApplyFileDiffsToolCall & { toolCallId: string }): Uint8Array {
  const applyFileDiffs = concat([
    stringField(1, params.summary),
    ...((params.diffs ?? []).map((diff) => messageField(2, concat([
      stringField(1, diff.filePath),
      stringField(2, diff.search),
      stringField(3, diff.replace),
    ])))),
    ...((params.newFiles ?? []).map((file) => messageField(3, concat([
      stringField(1, file.filePath),
      stringField(2, file.content),
    ])))),
    ...((params.deletedFiles ?? []).map((file) => messageField(4, stringField(1, file.filePath)))),
    ...((params.v4aUpdates ?? []).map((update) => messageField(5, concat([
      stringField(1, update.filePath),
      stringField(2, update.moveTo),
      ...update.hunks.map((hunk) => messageField(3, concat([
        ...((hunk.changeContext ?? []).map((context) => stringField(1, context))),
        stringField(2, hunk.preContext),
        stringField(3, hunk.old),
        stringField(4, hunk.new),
        stringField(5, hunk.postContext),
      ]))),
    ])))),
  ]);

  return concat([
    stringField(1, params.toolCallId),
    messageField(6, applyFileDiffs),
  ]);
}

function encodeSuggestPlanToolCall(params: SuggestPlanToolCall & { toolCallId: string }): Uint8Array {
  const suggestPlan = concat([
    stringField(1, params.summary),
    ...params.tasks.map((task) => messageField(2, encodeTask({
      taskId: randomUUID(),
      description: task.description,
    }))),
  ]);

  return concat([
    stringField(1, params.toolCallId),
    messageField(7, suggestPlan),
  ]);
}

function encodeToolCall(params: WarpToolCall): Uint8Array {
  switch (params.tool.type) {
    case "run_shell_command":
      return encodeRunShellCommandToolCall({ ...params.tool, toolCallId: params.toolCallId });
    case "read_files":
      return encodeReadFilesToolCall({ toolCallId: params.toolCallId, files: params.tool.files });
    case "grep":
      return encodeGrepToolCall({ ...params.tool, toolCallId: params.toolCallId });
    case "file_glob":
      return encodeFileGlobToolCall({ ...params.tool, toolCallId: params.toolCallId });
    case "apply_file_diffs":
      return encodeApplyFileDiffsToolCall({ ...params.tool, toolCallId: params.toolCallId });
    case "suggest_plan":
      return encodeSuggestPlanToolCall({ ...params.tool, toolCallId: params.toolCallId });
  }
}

function encodeToolCallMessage(params: {
  messageId: string;
  taskId: string;
  requestId: string;
} & WarpToolCall): Uint8Array {
  return concat([
    stringField(1, params.messageId),
    messageField(4, encodeToolCall(params)),
    stringField(11, params.taskId),
    stringField(13, params.requestId),
    messageField(14, encodeTimestamp(new Date())),
  ]);
}

export function encodeAddToolCall(params: {
  messageId: string;
  taskId: string;
  requestId: string;
} & WarpToolCall): Uint8Array {
  const toolCallMessage = encodeToolCallMessage(params);
  const addMessagesToTask = concat([
    stringField(1, params.taskId),
    messageField(2, toolCallMessage),
  ]);
  const clientAction = messageField(3, addMessagesToTask);
  const clientActions = messageField(1, clientAction);

  return messageField(2, clientActions);
}

export function encodeAddReadFilesToolCall(params: {
  messageId: string;
  taskId: string;
  requestId: string;
  toolCallId: string;
  files: ReadFilesToolCallFile[];
}): Uint8Array {
  return encodeAddToolCall({
    ...params,
    tool: { type: "read_files", files: params.files },
  });
}

export function encodeAppendAgentOutput(params: {
  messageId: string;
  taskId: string;
  requestId: string;
  text: string;
}): Uint8Array {
  const outputMessage = encodeAgentOutputMessage(params);
  const appendToMessageContent = concat([
    messageField(1, outputMessage),
    messageField(2, encodeFieldMask(["message.agent_output.text"])),
    stringField(3, params.taskId),
  ]);
  const clientAction = messageField(5, appendToMessageContent);
  const clientActions = messageField(1, clientAction);

  return messageField(2, clientActions);
}

export function encodeAgentOutput(params: {
  taskId: string;
  requestId: string;
  text: string;
}): Uint8Array {
  return encodeAddAgentOutput({
    ...params,
    messageId: randomUUID(),
  });
}

export function encodeStreamFinishedDone(): Uint8Array {
  const finished = messageField(2, new Uint8Array());
  return messageField(3, finished);
}

export function encodeStreamFinishedInternalError(message: string): Uint8Array {
  const internalError = stringField(1, message);
  const finished = messageField(7, internalError);
  return messageField(3, finished);
}

export function encodeBase64Url(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("base64").replace(/\+/g, "-").replace(/\//g, "_");
}

function formatToolResult(result: WarpToolResult): string {
  const header = `${result.type} result for tool_call_id=${result.toolCallId}`;
  if ("error" in result && result.error) {
    return `${header}\nError: ${result.error}`;
  }

  switch (result.type) {
    case "read_files":
      return `${header}\n${result.files.map((file) => `File: ${file.filePath}\n${file.content}`).join("\n\n")}`;
    case "run_shell_command":
      return `${header}\nCommand: ${result.command ?? ""}\nExit code: ${result.exitCode ?? ""}\nOutput:\n${result.output ?? ""}`;
    case "grep":
      return `${header}\n${result.matchedFiles.map((file) => {
        const lines = file.lineNumbers.length ? ` lines ${file.lineNumbers.join(", ")}` : "";
        return `${file.filePath}${lines}`;
      }).join("\n")}`;
    case "file_glob":
      return `${header}\n${result.matchedFiles.join("\n")}${result.warnings ? `\nWarnings:\n${result.warnings}` : ""}`;
    case "apply_file_diffs":
      return `${header}\nUpdated files:\n${result.updatedFiles.join("\n")}\nDeleted files:\n${result.deletedFiles.join("\n")}`;
    case "suggest_plan":
      return `${header}\nStatus: ${result.status}${result.planText ? `\nPlan:\n${result.planText}` : ""}`;
  }
}

function formatToolResultsPrompt(toolResults: WarpToolResult[]): string {
  if (!toolResults.length) {
    return "";
  }

  return [
    "Tool results are available. Use them to continue answering the user's original request.",
    ...toolResults.map(formatToolResult),
  ].join("\n\n");
}
