import { appendFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

type LogLevel = "debug" | "info" | "warn" | "error";

const logLevelPriority: Record<LogLevel, number> = {
  debug: 10,
  info: 20,
  warn: 30,
  error: 40,
};

function configuredLogLevel(): LogLevel {
  const value = process.env.LOG_LEVEL?.trim().toLowerCase();
  return value === "debug" || value === "info" || value === "warn" || value === "error"
    ? value
    : "info";
}

function shouldLog(level: LogLevel): boolean {
  return logLevelPriority[level] >= logLevelPriority[configuredLogLevel()];
}

function sanitize(fields: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(fields).map(([key, value]) => {
      if (/api.?key|token|authorization|secret|password/i.test(key)) {
        return [key, value ? "[redacted]" : value];
      }

      return [key, value];
    }),
  );
}

export function logFilePath(): string | undefined {
  const configured = process.env.LOCAL_SERVICE_LOG_PATH?.trim();
  if (configured === "false" || configured === "off" || configured === "0") {
    return undefined;
  }

  return configured || fileURLToPath(new URL("../local-service.log", import.meta.url));
}

export function log(level: LogLevel, event: string, fields: Record<string, unknown> = {}): void {
  if (!shouldLog(level)) {
    return;
  }

  const payload = {
    ts: new Date().toISOString(),
    level,
    event,
    ...sanitize(fields),
  };
  const line = JSON.stringify(payload);

  if (level === "error") {
    console.error(line);
  } else if (level === "warn") {
    console.warn(line);
  } else {
    console.log(line);
  }

  const path = logFilePath();
  if (path) {
    try {
      mkdirSync(dirname(path), { recursive: true });
      appendFileSync(path, `${line}\n`, "utf8");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.error(JSON.stringify({
        ts: new Date().toISOString(),
        level: "error",
        event: "log_file_write_failed",
        path,
        message,
      }));
    }
  }
}
