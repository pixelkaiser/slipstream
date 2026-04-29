#!/usr/bin/env node
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const toolRoot = resolve(scriptDir, "..");
const repoRoot = resolve(toolRoot, "../..");
const cacheRoot = resolve(toolRoot, ".proto-cache");
const generatedDir = resolve(toolRoot, "src/generated/warp_multi_agent/v1");
const sanitizedProtoDir = resolve(cacheRoot, "sanitized/multi_agent/v1");
const protoApiDir = resolve(cacheRoot, "warp-proto-apis");

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? toolRoot,
    stdio: "inherit",
    env: process.env,
  });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed with exit code ${result.status ?? "unknown"}`);
  }
}

function parsePinnedProtoDependency() {
  const cargoToml = readFileSync(resolve(repoRoot, "Cargo.toml"), "utf8");
  const match = cargoToml.match(/warp_multi_agent_api\s*=\s*\{[^}]*git\s*=\s*"([^"]+)"[^}]*rev\s*=\s*"([^"]+)"/s);
  if (!match) {
    throw new Error("Could not find pinned warp_multi_agent_api git dependency in Cargo.toml.");
  }

  return { gitUrl: match[1], rev: match[2] };
}

function ensureProtoRepo(gitUrl, rev) {
  const override = process.env.WARP_PROTO_APIS_DIR?.trim();
  if (override) {
    return resolve(override);
  }

  mkdirSync(cacheRoot, { recursive: true });
  if (!existsSync(resolve(protoApiDir, ".git"))) {
    rmSync(protoApiDir, { recursive: true, force: true });
    run("git", ["clone", "--filter=blob:none", "--no-checkout", gitUrl, protoApiDir]);
  }

  run("git", ["fetch", "--depth=1", "origin", rev], { cwd: protoApiDir });
  run("git", ["checkout", "--detach", rev], { cwd: protoApiDir });
  return protoApiDir;
}

function sanitizeProto(content) {
  return content
    .replaceAll('edition = "2023";', 'syntax = "proto3";')
    .replace(/^import "google\/protobuf\/go_features\.proto";\r?\n/gm, "")
    .replace(/^option features\..*;\r?\n/gm, "")
    .replace(/reserved\s+([A-Za-z_][A-Za-z0-9_]*)\s*;/g, 'reserved "$1";');
}

function prepareProtos(sourceDir) {
  rmSync(sanitizedProtoDir, { recursive: true, force: true });
  mkdirSync(sanitizedProtoDir, { recursive: true });

  const protoFiles = readdirSync(sourceDir)
    .filter((fileName) => fileName.endsWith(".proto"))
    .sort();

  for (const fileName of protoFiles) {
    const source = readFileSync(resolve(sourceDir, fileName), "utf8");
    writeFileSync(resolve(sanitizedProtoDir, fileName), sanitizeProto(source));
  }

  return protoFiles;
}

function generateTypeScript(protoFiles) {
  rmSync(generatedDir, { recursive: true, force: true });
  mkdirSync(generatedDir, { recursive: true });

  const protoc = process.env.PROTOC?.trim() || "protoc";
  const plugin = resolve(toolRoot, "node_modules/.bin/protoc-gen-es");
  run(protoc, [
    `--plugin=protoc-gen-es=${plugin}`,
    `--es_out=${generatedDir}`,
    "--es_opt=target=ts,import_extension=js",
    `--proto_path=${sanitizedProtoDir}`,
    ...protoFiles.map((fileName) => resolve(sanitizedProtoDir, fileName)),
  ]);
}

const { gitUrl, rev } = parsePinnedProtoDependency();
const protoRepo = ensureProtoRepo(gitUrl, rev);
const sourceProtoDir = resolve(protoRepo, "apis/multi_agent/v1");
const protoFiles = prepareProtos(sourceProtoDir);
generateTypeScript(protoFiles);

console.log(`Generated ${protoFiles.length} multi-agent proto files from ${rev}.`);
