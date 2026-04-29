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
  contextImages?: Array<{ data: string; mimeType: string }>;
  toolResults: WarpToolResult[];
  mcpTools: McpToolSummary[];
  openAiApiKey?: string;
  model?: string;
};

export type McpToolSummary = {
  name: string;
  serverId?: string;
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

export type SearchCodebaseToolCall = {
  type: "search_codebase";
  query: string;
  pathFilters?: string[];
  codebasePath?: string;
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

export type ReadMcpResourceToolCall = {
  type: "read_mcp_resource";
  uri: string;
  serverId?: string;
};

export type CallMcpToolToolCall = {
  type: "call_mcp_tool";
  name: string;
  args?: Record<string, unknown>;
  serverId?: string;
};

export type WarpToolCall =
  | ({ toolCallId: string } & {
      tool: RunShellCommandToolCall | { type: "read_files"; files: ReadFilesToolCallFile[] } | GrepToolCall | FileGlobToolCall | SearchCodebaseToolCall | ApplyFileDiffsToolCall | SuggestPlanToolCall | ReadMcpResourceToolCall | CallMcpToolToolCall;
    });

export type WarpToolResult =
  | ReadFilesToolResult
  | { type: "run_shell_command"; toolCallId: string; command?: string; output?: string; exitCode?: number; error?: string }
  | { type: "grep"; toolCallId: string; matchedFiles: Array<{ filePath: string; lineNumbers: number[] }>; error?: string }
  | { type: "file_glob"; toolCallId: string; matchedFiles: string[]; warnings?: string; error?: string }
  | { type: "apply_file_diffs"; toolCallId: string; updatedFiles: string[]; deletedFiles: string[]; error?: string }
  | { type: "suggest_plan"; toolCallId: string; status: "accepted" | "edited"; planText?: string }
  | { type: "generic"; toolCallId: string; name: string; content: string; error?: string };

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

function floatField(fieldNumber: number, value: number | undefined): Uint8Array {
  if (value == null) {
    return new Uint8Array();
  }

  const bytes = new Uint8Array(4);
  new DataView(bytes.buffer).setFloat32(0, value, true);
  return concat([tag(fieldNumber, 5), bytes]);
}

function doubleField(fieldNumber: number, value: number | undefined): Uint8Array {
  if (value == null) {
    return new Uint8Array();
  }

  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setFloat64(0, value, true);
  return concat([tag(fieldNumber, 1), bytes]);
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

function bytesValue(fieldValue: ProtoField | undefined): Uint8Array | undefined {
  if (!fieldValue || fieldValue.wireType !== 2) {
    return undefined;
  }

  return fieldValue.raw;
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
    const cliAgentUserQuery = message(field(userInput, 3));
    const cliUserQuery = message(field(cliAgentUserQuery, 1));
    const messagesReceived = message(field(userInput, 4));
    const query = firstNonEmpty(
      stringValue(field(userQuery, 1)),
      stringValue(field(cliUserQuery, 1)),
      formatReceivedMessages(messagesReceived),
    );
    if (query) {
      return query;
    }
  }

  const deprecatedUserQuery = message(field(input, 2));
  const cannedResponse = message(field(input, 4));
  const autoCodeDiff = message(field(input, 5));
  const resumeConversation = message(field(input, 7));
  const initProjectRules = message(field(input, 8));
  const generatePassiveSuggestions = message(field(input, 9));
  const createNewProject = message(field(input, 10));
  const cloneRepository = message(field(input, 11));
  const codeReview = message(field(input, 12));
  const summarizeConversation = message(field(input, 13));
  const createEnvironment = message(field(input, 14));
  const fetchReviewComments = message(field(input, 15));
  const startFromAmbientRunPrompt = message(field(input, 16));
  const invokeSkill = message(field(input, 17));
  const invokeSkillUserQuery = message(field(invokeSkill, 2));

  return firstNonEmpty(
    stringValue(field(deprecatedUserQuery, 1)),
    stringValue(field(cannedResponse, 1)),
    stringValue(field(autoCodeDiff, 1)),
    resumeConversation.length ? "Resume this conversation." : undefined,
    initProjectRules.length ? "Initialize project rules for this project." : undefined,
    formatPassiveSuggestionPrompt(generatePassiveSuggestions),
    stringValue(field(createNewProject, 1)),
    formatCloneRepositoryPrompt(cloneRepository),
    codeReview.length ? "Review the attached code review comments and diff context." : undefined,
    stringValue(field(summarizeConversation, 1)),
    formatCreateEnvironmentPrompt(createEnvironment),
    formatFetchReviewCommentsPrompt(fetchReviewComments),
    formatStartFromAmbientRunPrompt(startFromAmbientRunPrompt),
    stringValue(field(invokeSkillUserQuery, 1)),
  );
}

function formatReceivedMessages(messagesReceived: ProtoField[]): string | undefined {
  const messages = fields(messagesReceived, 1).flatMap((messageField): string[] => {
    const received = message(messageField);
    const sender = stringValue(field(received, 2));
    const subject = stringValue(field(received, 4));
    const body = stringValue(field(received, 5));
    if (!subject && !body) {
      return [];
    }
    return [[
      "Message received from another agent:",
      sender ? `Sender: ${sender}` : undefined,
      subject ? `Subject: ${subject}` : undefined,
      body ? `Body:\n${body}` : undefined,
    ].filter((line): line is string => line != null).join("\n")];
  });
  return messages.length ? messages.join("\n\n") : undefined;
}

function formatPassiveSuggestionPrompt(generatePassiveSuggestions: ProtoField[]): string | undefined {
  if (!generatePassiveSuggestions.length) {
    return undefined;
  }
  const shellCommandCompleted = message(field(generatePassiveSuggestions, 4));
  const executedCommand = formatExecutedShellCommand(message(field(shellCommandCompleted, 1)));
  return [
    "Generate passive suggestions for the current context.",
    executedCommand,
  ].filter((line): line is string => Boolean(line)).join("\n\n");
}

function formatCloneRepositoryPrompt(cloneRepository: ProtoField[]): string | undefined {
  const url = stringValue(field(cloneRepository, 1));
  return url ? `Clone repository: ${url}` : undefined;
}

function formatCreateEnvironmentPrompt(createEnvironment: ProtoField[]): string | undefined {
  const repoPaths = fields(createEnvironment, 1).flatMap((repoPath) => {
    const path = stringValue(repoPath);
    return path ? [path] : [];
  });
  return repoPaths.length ? `Create a development environment for:\n${repoPaths.join("\n")}` : undefined;
}

function formatFetchReviewCommentsPrompt(fetchReviewComments: ProtoField[]): string | undefined {
  const repoPath = stringValue(field(fetchReviewComments, 1));
  return repoPath ? `Fetch review comments for repository: ${repoPath}` : undefined;
}

function formatStartFromAmbientRunPrompt(startFromAmbientRunPrompt: ProtoField[]): string | undefined {
  const runtimeBasePrompt = stringValue(field(startFromAmbientRunPrompt, 2));
  const attachmentsDir = stringValue(field(startFromAmbientRunPrompt, 4));
  if (!runtimeBasePrompt && !attachmentsDir) {
    return undefined;
  }
  return [
    runtimeBasePrompt,
    attachmentsDir ? `Attachments directory: ${attachmentsDir}` : undefined,
  ].filter((line): line is string => line != null).join("\n\n");
}

function formatFileContent(content: ProtoField[]): string | undefined {
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

function decodeFileContext(fileFields: ProtoField[]): string | undefined {
  return formatFileContent(message(field(fileFields, 1)));
}

function formatExecutedShellCommand(command: ProtoField[]): string | undefined {
  const commandText = stringValue(field(command, 1));
  const output = stringValue(field(command, 2));
  const exitCode = intValue(field(command, 3));
  if (!commandText && !output) {
    return undefined;
  }

  return [
    "Executed shell command:",
    commandText ? `Command: ${commandText}` : undefined,
    exitCode != null ? `Exit code: ${exitCode}` : undefined,
    output ? `Output:\n${output}` : undefined,
  ].filter((line): line is string => line != null).join("\n");
}

function formatTimestamp(timestamp: ProtoField[]): string | undefined {
  const seconds = intValue(field(timestamp, 1));
  if (seconds == null) {
    return undefined;
  }
  return new Date(seconds * 1000).toISOString();
}

function decodeAttachedContext(requestFields: ProtoField[]): Pick<WarpRequestSummary, "contextText" | "contextImages"> {
  const input = message(field(requestFields, 2));
  const context = message(field(input, 1));
  if (!context.length) {
    return {};
  }

  const sections: string[] = [];
  const contextImages: Array<{ data: string; mimeType: string }> = [];

  const directory = message(field(context, 1));
  const pwd = stringValue(field(directory, 1));
  if (pwd) {
    sections.push(`Current directory: ${pwd}`);
  }

  const operatingSystem = message(field(context, 2));
  const platform = stringValue(field(operatingSystem, 1));
  const distribution = stringValue(field(operatingSystem, 2));
  if (platform || distribution) {
    sections.push(`Operating system: ${[platform, distribution].filter(Boolean).join(" ")}`);
  }

  const shell = message(field(context, 3));
  const shellName = stringValue(field(shell, 1));
  const shellVersion = stringValue(field(shell, 2));
  if (shellName || shellVersion) {
    sections.push(`Shell: ${[shellName, shellVersion].filter(Boolean).join(" ")}`);
  }

  const currentTime = formatTimestamp(message(field(context, 4)));
  if (currentTime) {
    sections.push(`Current time: ${currentTime}`);
  }

  for (const selectedTextField of fields(context, 6)) {
    const text = stringValue(field(message(selectedTextField), 1));
    if (text) {
      sections.push(`Selected text:\n${text}`);
    }
  }

  for (const commandField of fields(context, 5)) {
    const decoded = formatExecutedShellCommand(message(commandField));
    if (decoded) {
      sections.push(decoded);
    }
  }

  for (const imageField of fields(context, 7)) {
    const image = message(imageField);
    const data = bytesValue(field(image, 1));
    const mimeType = stringValue(field(image, 2)) ?? "image/png";
    if (data?.length) {
      contextImages.push({ data: Buffer.from(data).toString("base64"), mimeType });
    }
  }

  for (const fileField of fields(context, 9)) {
    const decoded = decodeFileContext(message(fileField));
    if (decoded) {
      sections.push(decoded);
    }
  }

  const codebases = fields(context, 8).flatMap((codebaseField): string[] => {
    const codebase = message(codebaseField);
    const name = stringValue(field(codebase, 1));
    const path = stringValue(field(codebase, 2));
    return name || path ? [`${name ?? "codebase"}: ${path ?? ""}`] : [];
  });
  if (codebases.length) {
    sections.push(`Indexed codebases:\n${codebases.join("\n")}`);
  }

  for (const projectRulesField of fields(context, 10)) {
    const projectRules = message(projectRulesField);
    const rootPath = stringValue(field(projectRules, 1));
    const activeRules = fields(projectRules, 2).flatMap((ruleField) => {
      const decoded = formatFileContent(message(ruleField));
      return decoded ? [decoded] : [];
    });
    const additionalRulePaths = fields(projectRules, 3).flatMap((rulePathField) => {
      const path = stringValue(rulePathField);
      return path ? [path] : [];
    });
    if (rootPath || activeRules.length || additionalRulePaths.length) {
      sections.push([
        `Project rules${rootPath ? ` for ${rootPath}` : ""}:`,
        ...activeRules,
        additionalRulePaths.length ? `Additional rule files:\n${additionalRulePaths.join("\n")}` : undefined,
      ].filter((line): line is string => line != null).join("\n"));
    }
  }

  const git = message(field(context, 11));
  const head = stringValue(field(git, 1));
  const branch = stringValue(field(git, 2));
  if (head || branch) {
    sections.push(`Git: ${[branch ? `branch=${branch}` : undefined, head ? `head=${head}` : undefined].filter(Boolean).join(", ")}`);
  }

  const skillsContext = message(field(context, 12));
  const skills = fields(skillsContext, 1).flatMap((skillField): string[] => {
    const skill = message(skillField);
    const name = stringValue(field(skill, 2));
    const description = stringValue(field(skill, 3));
    return name || description ? [`${name ?? "skill"}${description ? `: ${description}` : ""}`] : [];
  });
  if (skills.length) {
    sections.push(`Available skills:\n${skills.join("\n")}`);
  }

  const lspContext = message(field(context, 13));
  const lspServers = fields(lspContext, 1).flatMap((serverField): string[] => {
    const server = message(serverField);
    const root = stringValue(field(server, 1));
    const name = stringValue(field(server, 2));
    return root || name ? [`${name ?? "LSP"}${root ? ` (${root})` : ""}`] : [];
  });
  if (lspServers.length) {
    sections.push(`Available LSP servers:\n${lspServers.join("\n")}`);
  }

  return {
    contextText: sections.length ? sections.join("\n\n") : undefined,
    contextImages: contextImages.length ? contextImages : undefined,
  };
}

function formatAttachment(attachment: ProtoField[], label?: string): string | undefined {
  const plainText = stringValue(field(attachment, 1));
  const executedCommand = formatExecutedShellCommand(message(field(attachment, 2)));
  const runningCommand = formatRunningShellCommand(message(field(attachment, 3)));
  const documentContent = message(field(attachment, 7));
  const documentId = stringValue(field(documentContent, 1));
  const documentText = stringValue(field(documentContent, 2));
  const filePath = stringValue(field(message(field(attachment, 8)), 1));
  const body = firstNonEmpty(
    plainText,
    executedCommand,
    runningCommand,
    documentText ? [`Document: ${documentId ?? ""}`, documentText].join("\n") : undefined,
    filePath ? `File path: ${filePath}` : undefined,
  );
  if (!body) {
    return undefined;
  }
  return label ? `Referenced attachment ${label}:\n${body}` : `Referenced attachment:\n${body}`;
}

function decodeReferencedAttachments(requestFields: ProtoField[]): string[] {
  const input = message(field(requestFields, 2));
  const sections: string[] = [];
  const userQueries: ProtoField[][] = [];

  const userInputs = message(field(input, 6));
  for (const userInputField of fields(userInputs, 1)) {
    const userInput = message(userInputField);
    const direct = message(field(userInput, 1));
    const cli = message(field(message(field(userInput, 3)), 1));
    if (direct.length) {
      userQueries.push(direct);
    }
    if (cli.length) {
      userQueries.push(cli);
    }
  }

  const deprecatedUserQuery = message(field(input, 2));
  if (deprecatedUserQuery.length) {
    userQueries.push(deprecatedUserQuery);
  }

  for (const userQuery of userQueries) {
    for (const attachmentEntryField of fields(userQuery, 2)) {
      const attachmentEntry = message(attachmentEntryField);
      const key = stringValue(field(attachmentEntry, 1));
      const decoded = formatAttachment(message(field(attachmentEntry, 2)), key);
      if (decoded) {
        sections.push(decoded);
      }
    }
  }

  return sections;
}

function decodeMcpContext(requestFields: ProtoField[]): { sections: string[]; tools: McpToolSummary[] } {
  const mcpContext = message(field(requestFields, 6));
  if (!mcpContext.length) {
    return { sections: [], tools: [] };
  }

  const sections: string[] = [];
  const mcpTools: McpToolSummary[] = [];
  const legacyResources = fields(mcpContext, 1).flatMap((resourceField): string[] => {
    const resource = message(resourceField);
    const uri = stringValue(field(resource, 1));
    const name = stringValue(field(resource, 2));
    const description = stringValue(field(resource, 3));
    return uri || name || description ? [`${name ?? uri ?? "resource"}${description ? `: ${description}` : ""}${uri ? ` (${uri})` : ""}`] : [];
  });
  const legacyTools = fields(mcpContext, 2).flatMap((toolField): string[] => {
    const tool = message(toolField);
    const name = stringValue(field(tool, 1));
    const description = stringValue(field(tool, 2));
    if (name) {
      mcpTools.push({ name });
    }
    return name || description ? [`${name ?? "tool"}${description ? `: ${description}` : ""}`] : [];
  });

  if (legacyResources.length) {
    sections.push(`Available MCP resources:\n${legacyResources.join("\n")}`);
  }
  if (legacyTools.length) {
    sections.push(`Available MCP tools:\n${legacyTools.join("\n")}`);
  }

  for (const serverField of fields(mcpContext, 3)) {
    const server = message(serverField);
    const name = stringValue(field(server, 1));
    const description = stringValue(field(server, 2));
    const id = stringValue(field(server, 5));
    const resources = fields(server, 3).flatMap((resourceField) => {
      const resource = message(resourceField);
      const resourceName = stringValue(field(resource, 2));
      const uri = stringValue(field(resource, 1));
      return resourceName || uri ? [`${resourceName ?? uri}${uri ? ` (${uri})` : ""}`] : [];
    });
    const tools = fields(server, 4).flatMap((toolField) => {
      const tool = message(toolField);
      const toolName = stringValue(field(tool, 1));
      const toolDescription = stringValue(field(tool, 2));
      if (toolName) {
        mcpTools.push({
          name: toolName,
          ...(id ? { serverId: id } : {}),
        });
      }
      return toolName || toolDescription ? [`${toolName ?? "tool"}${toolDescription ? `: ${toolDescription}` : ""}`] : [];
    });
    if (name || description || id || resources.length || tools.length) {
      sections.push([
        `MCP server: ${name ?? id ?? "unnamed"}`,
        description ? `Description: ${description}` : undefined,
        resources.length ? `Resources:\n${resources.join("\n")}` : undefined,
        tools.length ? `Tools:\n${tools.join("\n")}` : undefined,
      ].filter((line): line is string => line != null).join("\n"));
    }
  }

  return { sections, tools: mcpTools };
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

function decodeSearchCodebaseResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  const searchCodebase = message(field(toolCallResultFields, 4));
  if (!toolCallId || !searchCodebase.length) {
    return undefined;
  }

  const error = stringValue(field(message(field(searchCodebase, 2)), 1));
  const files = fields(message(field(searchCodebase, 1)), 1).flatMap((fileField) => {
    const decoded = formatFileContent(message(fileField));
    return decoded ? [decoded] : [];
  });
  return { type: "generic", toolCallId, name: "search_codebase", content: files.join("\n\n"), error };
}

function formatShellSnapshot(snapshot: ProtoField[]): string | undefined {
  const output = stringValue(field(snapshot, 1));
  const cursor = stringValue(field(snapshot, 2));
  const commandId = stringValue(field(snapshot, 3));
  if (!output && !cursor && !commandId) {
    return undefined;
  }
  return [
    commandId ? `Command ID: ${commandId}` : undefined,
    cursor ? `Cursor: ${cursor}` : undefined,
    output ? `Output:\n${output}` : undefined,
  ].filter((line): line is string => line != null).join("\n");
}

function formatRunningShellCommand(runningCommand: ProtoField[]): string | undefined {
  const command = stringValue(field(runningCommand, 1));
  const snapshot = formatShellSnapshot(message(field(runningCommand, 2)));
  if (!command && !snapshot) {
    return undefined;
  }
  return [
    "Running shell command:",
    command ? `Command: ${command}` : undefined,
    snapshot,
  ].filter((line): line is string => line != null).join("\n");
}

function formatShellFinished(finished: ProtoField[]): string | undefined {
  const output = stringValue(field(finished, 1));
  const exitCode = intValue(field(finished, 2));
  const commandId = stringValue(field(finished, 3));
  if (!output && exitCode == null && !commandId) {
    return undefined;
  }
  return [
    commandId ? `Command ID: ${commandId}` : undefined,
    exitCode != null ? `Exit code: ${exitCode}` : undefined,
    output ? `Output:\n${output}` : undefined,
  ].filter((line): line is string => line != null).join("\n");
}

function decodeReadShellCommandOutputResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  const readShellOutput = message(field(toolCallResultFields, 22));
  if (!toolCallId || !readShellOutput.length) {
    return undefined;
  }

  const command = stringValue(field(readShellOutput, 1));
  const snapshot = formatShellSnapshot(message(field(readShellOutput, 2)));
  const finished = formatShellFinished(message(field(readShellOutput, 3)));
  const error = message(field(readShellOutput, 4)).length ? "Shell command not found." : undefined;
  return {
    type: "generic",
    toolCallId,
    name: "read_shell_command_output",
    content: [command ? `Command: ${command}` : undefined, snapshot, finished].filter(Boolean).join("\n"),
    error,
  };
}

function formatMcpResourceContent(resourceContent: ProtoField[]): string | undefined {
  const uri = stringValue(field(resourceContent, 1));
  const text = message(field(resourceContent, 2));
  const textContent = stringValue(field(text, 1));
  const textMime = stringValue(field(text, 2));
  const binary = message(field(resourceContent, 3));
  const binaryMime = stringValue(field(binary, 2));
  if (textContent) {
    return [`Resource: ${uri ?? ""}`, textMime ? `MIME: ${textMime}` : undefined, textContent].filter(Boolean).join("\n");
  }
  if (binary.length) {
    return [`Resource: ${uri ?? ""}`, `Binary content${binaryMime ? ` (${binaryMime})` : ""}`].join("\n");
  }
  return uri ? `Resource: ${uri}` : undefined;
}

function decodeReadMcpResourceResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  const readMcpResource = message(field(toolCallResultFields, 11));
  if (!toolCallId || !readMcpResource.length) {
    return undefined;
  }

  const error = stringValue(field(message(field(readMcpResource, 2)), 1));
  const contents = fields(message(field(readMcpResource, 1)), 1).flatMap((contentField) => {
    const decoded = formatMcpResourceContent(message(contentField));
    return decoded ? [decoded] : [];
  });
  return { type: "generic", toolCallId, name: "read_mcp_resource", content: contents.join("\n\n"), error };
}

function decodeCallMcpToolResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  const callMcpTool = message(field(toolCallResultFields, 12));
  if (!toolCallId || !callMcpTool.length) {
    return undefined;
  }

  const error = stringValue(field(message(field(callMcpTool, 2)), 1));
  const results = fields(message(field(callMcpTool, 1)), 1).flatMap((resultField): string[] => {
    const result = message(resultField);
    const text = stringValue(field(message(field(result, 1)), 1));
    if (text) {
      return [text];
    }
    const resource = formatMcpResourceContent(message(field(result, 3)));
    if (resource) {
      return [resource];
    }
    const image = message(field(result, 2));
    const mime = stringValue(field(image, 2));
    return image.length ? [`Image result${mime ? ` (${mime})` : ""}`] : [];
  });
  return { type: "generic", toolCallId, name: "call_mcp_tool", content: results.join("\n\n"), error };
}

function decodeReadSkillResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  const readSkill = message(field(toolCallResultFields, 26));
  if (!toolCallId || !readSkill.length) {
    return undefined;
  }

  const error = stringValue(field(message(field(readSkill, 2)), 1));
  const content = formatFileContent(message(field(message(field(readSkill, 1)), 1))) ?? "";
  return { type: "generic", toolCallId, name: "read_skill", content, error };
}

function decodeFetchConversationResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  const toolCallId = stringValue(field(toolCallResultFields, 1));
  const fetchConversation = message(field(toolCallResultFields, 27));
  if (!toolCallId || !fetchConversation.length) {
    return undefined;
  }

  const error = stringValue(field(message(field(fetchConversation, 2)), 1));
  const directoryPath = stringValue(field(message(field(fetchConversation, 1)), 2));
  return {
    type: "generic",
    toolCallId,
    name: "fetch_conversation",
    content: directoryPath ? `Conversation materialized at: ${directoryPath}` : "",
    error,
  };
}

function decodeToolResult(toolCallResultFields: ProtoField[]): WarpToolResult | undefined {
  return decodeReadFilesResult(toolCallResultFields)
    ?? decodeRunShellCommandResult(toolCallResultFields)
    ?? decodeSearchCodebaseResult(toolCallResultFields)
    ?? decodeGrepResult(toolCallResultFields)
    ?? decodeFileGlobResult(toolCallResultFields)
    ?? decodeApplyFileDiffsResult(toolCallResultFields)
    ?? decodeSuggestPlanResult(toolCallResultFields)
    ?? decodeReadMcpResourceResult(toolCallResultFields)
    ?? decodeCallMcpToolResult(toolCallResultFields)
    ?? decodeReadShellCommandOutputResult(toolCallResultFields)
    ?? decodeReadSkillResult(toolCallResultFields)
    ?? decodeFetchConversationResult(toolCallResultFields);
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
  const attachedContext = decodeAttachedContext(requestFields);
  const mcpContext = decodeMcpContext(requestFields);
  const contextSections = [
    attachedContext.contextText,
    ...decodeReferencedAttachments(requestFields),
    ...mcpContext.sections,
  ].filter((section): section is string => Boolean(section));
  const contextText = contextSections.length ? contextSections.join("\n\n") : undefined;

  return {
    conversationId,
    requestId: randomUUID(),
    rootTaskId: decodedRootTaskId ?? randomUUID(),
    shouldCreateRootTask: decodedRootTaskId == null,
    prompt: decodeInputPrompt(requestFields) ?? formatToolResultsPrompt(toolResults),
    ...(contextText ? { contextText } : {}),
    ...(attachedContext.contextImages ? { contextImages: attachedContext.contextImages } : {}),
    toolResults,
    mcpTools: mcpContext.tools,
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

function encodeSearchCodebaseToolCall(params: SearchCodebaseToolCall & { toolCallId: string }): Uint8Array {
  const searchCodebase = concat([
    stringField(1, params.query),
    ...((params.pathFilters ?? []).map((filter) => stringField(2, filter))),
    stringField(3, params.codebasePath),
  ]);

  return concat([
    stringField(1, params.toolCallId),
    messageField(3, searchCodebase),
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

function encodeJsonValue(value: unknown): Uint8Array {
  if (value == null) {
    return concat([tag(1, 0), encodeVarint(0)]);
  }
  if (typeof value === "number" && Number.isFinite(value)) {
    return doubleField(2, value);
  }
  if (typeof value === "string") {
    return stringField(3, value);
  }
  if (typeof value === "boolean") {
    return boolField(4, value);
  }
  if (Array.isArray(value)) {
    return messageField(6, concat(value.map((item) => messageField(1, encodeJsonValue(item)))));
  }
  if (typeof value === "object") {
    return messageField(5, encodeJsonStruct(value as Record<string, unknown>));
  }
  return stringField(3, String(value));
}

function encodeJsonStruct(value: Record<string, unknown>): Uint8Array {
  return concat(Object.entries(value).map(([key, fieldValue]) => messageField(1, concat([
    stringField(1, key),
    messageField(2, encodeJsonValue(fieldValue)),
  ]))));
}

function encodeReadMcpResourceToolCall(params: ReadMcpResourceToolCall & { toolCallId: string }): Uint8Array {
  const readMcpResource = concat([
    stringField(1, params.uri),
    stringField(2, params.serverId),
  ]);

  return concat([
    stringField(1, params.toolCallId),
    messageField(11, readMcpResource),
  ]);
}

function encodeCallMcpToolToolCall(params: CallMcpToolToolCall & { toolCallId: string }): Uint8Array {
  const callMcpTool = concat([
    stringField(1, params.name),
    messageField(2, encodeJsonStruct(params.args ?? {})),
    stringField(3, params.serverId),
  ]);

  return concat([
    stringField(1, params.toolCallId),
    messageField(12, callMcpTool),
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
    case "search_codebase":
      return encodeSearchCodebaseToolCall({ ...params.tool, toolCallId: params.toolCallId });
    case "apply_file_diffs":
      return encodeApplyFileDiffsToolCall({ ...params.tool, toolCallId: params.toolCallId });
    case "suggest_plan":
      return encodeSuggestPlanToolCall({ ...params.tool, toolCallId: params.toolCallId });
    case "read_mcp_resource":
      return encodeReadMcpResourceToolCall({ ...params.tool, toolCallId: params.toolCallId });
    case "call_mcp_tool":
      return encodeCallMcpToolToolCall({ ...params.tool, toolCallId: params.toolCallId });
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
    messageField(2, encodeFieldMask(["agent_output.text"])),
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

export function encodeStreamFinishedDone(params?: {
  contextWindowUsage?: number;
  summarized?: boolean;
  creditsSpent?: number;
}): Uint8Array {
  const conversationUsageMetadata = params?.contextWindowUsage == null
    ? new Uint8Array()
    : messageField(11, concat([
      floatField(1, params.contextWindowUsage),
      boolField(2, params.summarized ?? false),
      floatField(3, params.creditsSpent ?? 0),
    ]));
  const finished = concat([
    messageField(2, new Uint8Array()),
    conversationUsageMetadata,
  ]);
  return messageField(3, finished);
}

export function encodeStreamFinishedInternalError(message: string): Uint8Array {
  const internalError = stringField(1, message);
  const finished = messageField(7, internalError);
  return messageField(3, finished);
}

export function encodeStreamFinishedInvalidApiKey(modelName?: string): Uint8Array {
  const invalidApiKey = concat([
    int64Field(1, 2), // LLM_PROVIDER_OPENAI
    stringField(2, modelName),
  ]);
  const finished = messageField(12, invalidApiKey);
  return messageField(3, finished);
}

export function encodeStreamFinishedLlmUnavailable(): Uint8Array {
  const finished = messageField(6, new Uint8Array());
  return messageField(3, finished);
}

export function encodeStreamFinishedContextWindowExceeded(): Uint8Array {
  const finished = messageField(5, new Uint8Array());
  return messageField(3, finished);
}

export function encodeStreamFinishedQuotaLimit(): Uint8Array {
  const finished = messageField(4, new Uint8Array());
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
    case "generic":
      return `${result.name} result for tool_call_id=${result.toolCallId}\n${result.content}`;
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
