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
}
