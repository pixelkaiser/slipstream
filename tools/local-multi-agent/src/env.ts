import { readFileSync } from "node:fs";
import { resolve } from "node:path";

function unquoteEnvValue(value: string): string {
  const trimmed = value.trim();
  if (trimmed.length < 2) {
    return trimmed;
  }

  const quote = trimmed[0];
  if ((quote !== "\"" && quote !== "'") || trimmed.at(-1) !== quote) {
    return trimmed;
  }

  const inner = trimmed.slice(1, -1);
  return quote === "\"" ? inner.replace(/\\n/g, "\n").replace(/\\"/g, "\"") : inner;
}

export function loadDotEnv(path = ".env"): void {
  let contents: string;
  try {
    contents = readFileSync(resolve(path), "utf8");
  } catch (error) {
    if (error && typeof error === "object" && "code" in error && error.code === "ENOENT") {
      return;
    }
    throw error;
  }

  for (const rawLine of contents.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) {
      continue;
    }

    const withoutExport = line.startsWith("export ") ? line.slice("export ".length).trimStart() : line;
    const separator = withoutExport.indexOf("=");
    if (separator <= 0) {
      continue;
    }

    const key = withoutExport.slice(0, separator).trim();
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key) || Object.hasOwn(process.env, key)) {
      continue;
    }

    process.env[key] = unquoteEnvValue(withoutExport.slice(separator + 1));
  }
}
