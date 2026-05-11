#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const DEFAULT_SERVER = "ws://127.0.0.1:4500";
const DEFAULT_OUT = "/tmp/slipstream-codex-live-capture.ndjson";
const DEFAULT_TIMEOUT_MS = 180_000;

function usage() {
  console.error(`Usage:
  node script/capture_codex_approval.mjs [options]

Options:
  --server URL       Codex app-server WebSocket URL. Default: ${DEFAULT_SERVER}
  --cwd PATH         Thread cwd. Default: current working directory.
  --prompt TEXT      Prompt to send after starting a thread.
  --plan             Send the prompt in Codex plan collaboration mode.
  --approval-policy POLICY
                    Codex approval policy. Default: on-request.
  --out PATH         NDJSON capture output path. Default: ${DEFAULT_OUT}
  --timeout-ms N     Stop after N milliseconds. Default: ${DEFAULT_TIMEOUT_MS}

The script writes every inbound and outbound JSON-RPC packet to --out and exits
with status 0 only after it sees an approval/user-input request.`);
}

function parseArgs(argv) {
  const args = {
    server: DEFAULT_SERVER,
    cwd: process.cwd(),
    prompt:
      "Use the shell to run this exact command with escalated approval: /bin/zsh -lc \"printf slipstream-codex-approval-capture\\\\n\"",
    plan: false,
    approvalPolicy: "on-request",
    out: DEFAULT_OUT,
    timeoutMs: DEFAULT_TIMEOUT_MS,
  };

  for (let index = 2; index < argv.length; index += 1) {
    const arg = argv[index];
    switch (arg) {
      case "--server":
        args.server = requireValue(argv, ++index, arg);
        break;
      case "--cwd":
        args.cwd = requireValue(argv, ++index, arg);
        break;
      case "--prompt":
        args.prompt = requireValue(argv, ++index, arg);
        break;
      case "--plan":
        args.plan = true;
        break;
      case "--approval-policy":
        args.approvalPolicy = requireValue(argv, ++index, arg);
        break;
      case "--out":
        args.out = requireValue(argv, ++index, arg);
        break;
      case "--timeout-ms":
        args.timeoutMs = Number(requireValue(argv, ++index, arg));
        if (!Number.isFinite(args.timeoutMs) || args.timeoutMs <= 0) {
          throw new Error("--timeout-ms must be a positive number");
        }
        break;
      case "--help":
      case "-h":
        usage();
        process.exit(0);
      default:
        throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return args;
}

function requireValue(argv, index, name) {
  const value = argv[index];
  if (!value) {
    throw new Error(`${name} requires a value`);
  }
  return value;
}

function writeRecord(out, record) {
  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.appendFileSync(out, `${JSON.stringify(record)}\n`);
}

function isApprovalMessage(message) {
  const method = String(message?.method ?? "").replace(/[^a-zA-Z0-9]/g, "").toLowerCase();
  return (
    method.includes("requestapproval") ||
    method.includes("execapprovalrequest") ||
    method.includes("applypatchapprovalrequest") ||
    method.includes("requestpermissions") ||
    method.includes("requestuserinput")
  );
}

function waitForOpen(socket) {
  return new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener(
      "error",
      (event) => reject(new Error(`WebSocket error before open: ${event.message ?? "unknown"}`)),
      { once: true },
    );
  });
}

async function main() {
  const args = parseArgs(process.argv);
  fs.rmSync(args.out, { force: true });

  const socket = new WebSocket(args.server);
  await waitForOpen(socket);

  let nextId = 1;
  let threadId = null;
  let model = null;
  let foundApproval = null;

  const send = (message) => {
    socket.send(JSON.stringify(message));
    writeRecord(args.out, {
      at: new Date().toISOString(),
      direction: "out",
      message,
    });
  };
  const request = (method, params) => {
    const message = { id: nextId, method, params };
    nextId += 1;
    send(message);
    return message.id;
  };

  const done = new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`Timed out after ${args.timeoutMs}ms without an approval request`));
    }, args.timeoutMs);

    socket.addEventListener("message", (event) => {
      const raw = String(event.data);
      let message;
      try {
        message = JSON.parse(raw);
      } catch {
        message = raw;
      }
      writeRecord(args.out, {
        at: new Date().toISOString(),
        direction: "in",
        message,
      });

      if (message?.id === 2 && message?.result) {
        threadId =
          message.result?.thread?.id ??
          message.result?.threadId ??
          message.result?.id ??
          threadId;
        model = message.result?.model ?? message.result?.thread?.model ?? model;
        if (!threadId) {
          return;
        }
        const params = {
          threadId,
          approvalPolicy: args.approvalPolicy,
          approvalsReviewer: "user",
          input: [{ type: "text", text: args.prompt }],
        };
        if (args.plan) {
          params.collaborationMode = {
            mode: "plan",
            settings: {
              model: model ?? "gpt-5.5",
              developer_instructions: null,
              reasoning_effort: null,
            },
          };
        }
        request("turn/start", params);
      }

      if (message && typeof message === "object" && isApprovalMessage(message)) {
        foundApproval = message;
        clearTimeout(timer);
        resolve();
      }
    });

    socket.addEventListener("close", () => {
      clearTimeout(timer);
      reject(new Error("WebSocket closed before an approval request arrived"));
    });
    socket.addEventListener("error", (event) => {
      clearTimeout(timer);
      reject(new Error(`WebSocket error: ${event.message ?? "unknown"}`));
    });
  });

  request("initialize", {
    clientInfo: { name: "Slipstream approval capture", version: "0.0.0" },
    capabilities: { experimentalApi: true },
  });
  send({ method: "initialized", params: {} });
  request("thread/start", {
    cwd: args.cwd,
    approvalPolicy: args.approvalPolicy,
    approvalsReviewer: "user",
  });

  await done;
  socket.close();
  console.log(JSON.stringify({ out: args.out, approval: foundApproval }, null, 2));
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
