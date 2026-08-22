#!/usr/bin/env node

/**
 * Web command contract check.
 *
 * The command inventory is generated from static `invoke(...)` calls in the
 * shared frontend. Web backend support is read from Rust's explicit public
 * registry, rather than inferred from Rust match/control flow.
 */
import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";

const root = process.cwd();
const frontendRoot = join(root, "src");
const registryPath = join(root, "src-web/src/command/mod.rs");
const noopPath = join(root, "src/lib/platform/webNoop.ts");
const webTransportPath = join(root, "src/lib/platform/web.ts");

function walk(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return entry.name === "lib" && dir === frontendRoot ? walk(path) : walk(path);
    return /\.(?:ts|tsx)$/.test(entry.name) ? [path] : [];
  });
}

function lineAt(source, offset) {
  return source.slice(0, offset).split("\n").length;
}

function skipWhitespace(source, offset) {
  while (/\s/.test(source[offset] ?? "")) offset += 1;
  return offset;
}

function skipTypeArguments(source, offset) {
  if (source[offset] !== "<") return offset;
  let depth = 0;
  for (let i = offset; i < source.length; i += 1) {
    if (source[i] === "<") depth += 1;
    if (source[i] === ">" && --depth === 0) return i + 1;
  }
  return offset;
}

function readStringLiteral(source, offset) {
  const quote = source[offset];
  if (quote !== "'" && quote !== '"') return null;
  let value = "";
  for (let i = offset + 1; i < source.length; i += 1) {
    if (source[i] === "\\") {
      value += source[i + 1] ?? "";
      i += 1;
      continue;
    }
    if (source[i] === quote) return { value, end: i + 1 };
    value += source[i];
  }
  return null;
}

function scanInvokes(path) {
  const source = readFileSync(path, "utf8");
  const commands = [];
  const dynamic = [];
  const matcher = /\binvoke\b/g;
  for (const match of source.matchAll(matcher)) {
    let offset = skipWhitespace(source, match.index + match[0].length);
    offset = skipWhitespace(source, skipTypeArguments(source, offset));
    if (source[offset] !== "(") continue;
    const argumentOffset = skipWhitespace(source, offset + 1);
    const literal = readStringLiteral(source, argumentOffset);
    const entry = { file: relative(root, path), line: lineAt(source, match.index) };
    if (literal) commands.push({ ...entry, command: literal.value });
    else dynamic.push(entry);
  }
  return { commands, dynamic };
}

function quotedItemsInConst(source, constantName) {
  const match = source.match(new RegExp(`pub const ${constantName}:\\s*&\\[&str\\]\\s*=\\s*&\\[([\\s\\S]*?)\\];`));
  if (!match) throw new Error(`Cannot find Rust ${constantName} registry`);
  return [...match[1].matchAll(/"([^"\\]*(?:\\.[^"\\]*)*)"/g)].map((item) => item[1]);
}

function objectKeys(source, objectName) {
  const start = source.indexOf(`export const ${objectName}`);
  if (start < 0) throw new Error(`Cannot find ${objectName}`);
  const body = source.slice(start, source.indexOf("};", start));
  return [...body.matchAll(/^\s*([A-Za-z0-9_]+):/gm)].map((item) => item[1]);
}

function webTransportCommands(source) {
  return [...source.matchAll(/command\s*===\s*["']([^"']+)["']/g)].map((item) => item[1]);
}

const frontend = walk(frontendRoot)
  .filter((path) => !path.includes("/src/lib/platform/"))
  .map(scanInvokes)
  .reduce(
    (all, scanned) => ({
      commands: [...all.commands, ...scanned.commands],
      dynamic: [...all.dynamic, ...scanned.dynamic],
    }),
    { commands: [], dynamic: [] },
  );

const sourcesByCommand = new Map();
for (const entry of frontend.commands) {
  const sources = sourcesByCommand.get(entry.command) ?? [];
  sources.push(`${entry.file}:${entry.line}`);
  sourcesByCommand.set(entry.command, sources);
}

function report(title, commands) {
  console.log(`${title} (${commands.length})`);
  for (const command of commands) {
    console.log(`  ${command} <- ${(sourcesByCommand.get(command) ?? []).join(", ")}`);
  }
}

const frontendCommands = [...new Set(frontend.commands.map((entry) => entry.command))].sort();
const backend = new Set(quotedItemsInConst(readFileSync(registryPath, "utf8"), "SUPPORTED_COMMANDS"));
const client = new Set(webTransportCommands(readFileSync(webTransportPath, "utf8")));
const noop = new Set(objectKeys(readFileSync(noopPath, "utf8"), "WEB_NOOP_COMMANDS"));
// Fill this table only for deliberate, temporary stage-7.1 gaps. The final
// acceptance command enables WEB_CONTRACT_FINAL=1, which rejects all entries.
const unsupported = new Set([]);

const statuses = new Map();
for (const command of frontendCommands) {
  const groups = [
    backend.has(command) || client.has(command) ? "SUPPORTED" : null,
    noop.has(command) ? "NOOP" : null,
    unsupported.has(command) ? "UNSUPPORTED" : null,
  ].filter(Boolean);
  statuses.set(command, groups);
}

const supported = frontendCommands.filter((command) => statuses.get(command).includes("SUPPORTED"));
const noops = frontendCommands.filter((command) => statuses.get(command).includes("NOOP"));
const unsupportedCommands = frontendCommands.filter((command) => statuses.get(command).includes("UNSUPPORTED"));
const missing = frontendCommands.filter((command) => statuses.get(command).length === 0);
const duplicates = frontendCommands.filter((command) => statuses.get(command).length > 1);

const finalUnsupported = process.env.WEB_CONTRACT_FINAL === "1" && unsupportedCommands.length > 0;
export const contractReport = {
  supported,
  noops,
  unsupported: unsupportedCommands,
  missing,
  duplicates,
  dynamic: frontend.dynamic,
  finalUnsupported,
};

if (!process.env.VITEST) {
  report("SUPPORTED", supported);
  report("NOOP", noops);
  report("UNSUPPORTED", unsupportedCommands);
  report("MISSING", missing);
  report("DUPLICATE", duplicates);
  console.log(`DYNAMIC (${frontend.dynamic.length})`);
  for (const entry of frontend.dynamic) console.log(`  ${entry.file}:${entry.line}`);
  if (missing.length || duplicates.length || frontend.dynamic.length || finalUnsupported) process.exitCode = 1;
}
