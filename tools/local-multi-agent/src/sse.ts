import { encodeBase64Url } from "./protobuf.js";

export function formatSseDataEvent(bytes: Uint8Array): string {
  return `data: ${encodeBase64Url(bytes)}\n\n`;
}
