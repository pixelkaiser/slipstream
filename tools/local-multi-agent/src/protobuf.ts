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
  prompt: string;
  openAiApiKey?: string;
  model?: string;
};

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

export function decodeWarpRequest(bytes: Uint8Array): WarpRequestSummary {
  const requestFields = decodeFields(bytes);
  const { conversationId } = decodeMetadata(requestFields);
  const { openAiApiKey, model } = decodeSettings(requestFields);

  return {
    conversationId,
    requestId: randomUUID(),
    rootTaskId: decodeRootTaskId(requestFields) ?? "root",
    prompt: decodeInputPrompt(requestFields) ?? "",
    openAiApiKey,
    model,
  };
}

export function encodeStreamInit(conversationId: string, requestId: string): Uint8Array {
  const init = concat([
    stringField(1, conversationId),
    stringField(2, requestId),
    stringField(3, conversationId),
  ]);

  return messageField(1, init);
}

function encodeTimestamp(date: Date): Uint8Array {
  return int64Field(1, Math.floor(date.getTime() / 1000));
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
  return Buffer.from(bytes).toString("base64url");
}
