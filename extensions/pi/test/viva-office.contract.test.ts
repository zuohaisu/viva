/**
 * Contract test: the commands the Pi extension emits must be recognised by
 * the REAL `viva` binary (issue #86).
 *
 * Why this exists: `viva-office.test.ts` drives the extension against a fake
 * `viva` script that returns fixed JSON for ANY arguments. It stayed green
 * while the extension kept calling a retired command namespace,
 * which the real binary rejects with "unknown or missing subcommand". A fake
 * cannot catch a renamed command; only the real binary can.
 *
 * How it works: each tool the extension registers is executed exactly as Pi
 * would execute it, against the real binary and a throwaway VIVA_HOME. The
 * argv under test is therefore whatever the extension really emits, never a
 * copy of it written into this file. No `viva server` is started, so
 * `handoff` and `dispatch` are expected to be refused by Viva
 * ("no active server") — which proves the CLI recognised them. Their
 * success paths are deliberately not exercised here.
 *
 * The binary comes from VIVA_BIN, else `target/debug/viva`:
 *   cargo build -p viva && npm test
 * A missing binary is a FAILURE, never a skip: a skipped contract test
 * proves nothing.
 */

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import vivaOffice from "../viva-office.ts";

type ToolResult = { content: string; isError?: boolean };
type Tool = { execute: (args: Record<string, unknown>) => Promise<ToolResult> | ToolResult };

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

/** What the real binary says when it does not recognise a command. */
const UNRECOGNISED = /unknown or missing subcommand/;
/** What Viva says when no resident server is running: a business-layer refusal. */
const NO_ACTIVE_SERVER = /no active server/;

const GOAL = "contract test goal";

interface Fixture {
  bin: string;
  root: string;
  home: string;
  tools: Map<string, Tool>;
}

let fx: Fixture | undefined;

/** The real binary: VIVA_BIN, else the debug build in this checkout. */
function resolveVivaBinary(): string {
  const bin = process.env.VIVA_BIN
    ? resolve(process.env.VIVA_BIN)
    : join(REPO_ROOT, "target", "debug", "viva");
  assert.ok(
    existsSync(bin),
    `the real viva binary was not found at ${bin}. Build it with \`cargo build -p viva\` ` +
      "or point VIVA_BIN at one. This contract test never skips: a skipped test would " +
      "re-create the green-tests / broken-product failure it exists to catch.",
  );
  return bin;
}

/** A clean environment: nothing from a surrounding Viva session leaks in. */
function cleanEnv(extra: Record<string, string>): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (!key.startsWith("VIVA_")) env[key] = value;
  }
  return { ...env, ...extra };
}

function runViva(bin: string, home: string, args: string[]) {
  const result = spawnSync(bin, args, {
    env: cleanEnv({ VIVA_HOME: home }),
    encoding: "utf8",
    timeout: 30_000,
  });
  return { status: result.status, stdout: result.stdout, stderr: result.stderr, error: result.error };
}

function runVivaJson(bin: string, home: string, args: string[]): Record<string, unknown> {
  const out = runViva(bin, home, args);
  assert.equal(out.status, 0, `viva ${args.join(" ")} failed: ${out.stderr}${out.error ?? ""}`);
  return JSON.parse(out.stdout) as Record<string, unknown>;
}

/** Load the extension the way Pi does: factory call with the harness env. */
function loadExtensionTools(raw: NodeJS.ProcessEnv): Map<string, Tool> {
  const tools = new Map<string, Tool>();
  const fakePi = {
    on() {
      return () => {};
    },
    registerTool(name: string, tool: Tool) {
      tools.set(name, tool);
    },
    registerCommand() {},
  };
  const saved = process.env;
  process.env = raw;
  try {
    vivaOffice(fakePi as never);
  } finally {
    process.env = saved;
  }
  return tools;
}

before(() => {
  const bin = resolveVivaBinary();
  const root = mkdtempSync(join(tmpdir(), "viva-contract-"));
  const home = join(root, "home");
  mkdirSync(home, { mode: 0o700 });
  // Register the cleanup handle before anything below can throw.
  fx = { bin, root, home, tools: new Map() };

  // A real store with a real task: `brief` reads it back.
  const init = runViva(bin, home, ["init"]);
  assert.equal(init.status, 0, `viva init failed: ${init.stderr}`);
  const task = runVivaJson(bin, home, ["create-task", "--goal", GOAL]);

  fx.tools = loadExtensionTools(
    cleanEnv({
      VIVA_BIN: bin,
      VIVA_HOME: home,
      VIVA_OFFICE_MEMBER_ID: "mem-00000000000000000000000000000001",
      VIVA_OFFICE_MEMBER_NAME: "Contract Member",
      VIVA_OFFICE_TASK_ID: String(task.task_id),
      VIVA_OFFICE_GRANT_ID: "grant-00000000000000000000000000000001",
    }),
  );
});

after(() => {
  if (fx) rmSync(fx.root, { recursive: true, force: true });
});

function tool(name: string): Tool {
  const found = fx?.tools.get(name);
  assert.ok(found, `the extension did not register ${name}`);
  return found;
}

/**
 * The command was understood by the CLI and refused by Viva, not by
 * the argument parser. (No resident server runs in this test.)
 */
function assertRecognisedButRefused(result: ToolResult, what: string): void {
  assert.equal(result.isError, true, `${what}: with no server running the call must be refused`);
  assert.doesNotMatch(result.content, UNRECOGNISED, `${what}: the real CLI did not recognise the command`);
  assert.match(result.content, NO_ACTIVE_SERVER, `${what}: expected Viva's own refusal`);
}

test("control: the real binary rejects an unknown command namespace", () => {
  assert.ok(fx);
  const out = runViva(fx.bin, fx.home, ["no-such-namespace", "status"]);
  assert.notEqual(out.status, 0, "`viva no-such-namespace status` must be rejected");
  // Also proves UNRECOGNISED matches a real rejection, so the negative
  // assertions below are not vacuous.
  assert.match(out.stderr, UNRECOGNISED);
  assert.match(out.stdout, /USAGE/, "the real binary prints its USAGE banner for an unknown command");
});

test("viva_office_status is accepted and returns the real status", async () => {
  assert.ok(fx);
  const result = await tool("viva_office_status").execute({});
  assert.notEqual(result.isError, true, `status failed: ${result.content}`);
  const payload = JSON.parse(result.content) as { home?: string };
  assert.equal(typeof payload.home, "string", "status payload names its home");
  assert.equal(realpathSync(payload.home as string), realpathSync(fx.home));
});

test("viva_office_task_brief is accepted and returns the real task brief", async () => {
  const result = await tool("viva_office_task_brief").execute({});
  assert.notEqual(result.isError, true, `brief failed: ${result.content}`);
  const payload = JSON.parse(result.content) as { goal?: string };
  assert.equal(payload.goal, GOAL);
});

test("viva_office_handoff is recognised by the real CLI", async () => {
  const result = await tool("viva_office_handoff").execute({ summary: "contract test handoff" });
  assertRecognisedButRefused(result, "handoff");
});

test("viva_office_dispatch is recognised by the real CLI", async () => {
  assert.ok(fx);
  const result = await tool("viva_office_dispatch").execute({
    argv: ["/bin/true"],
    cwd: fx.root,
    request_key: "contract-test-request-key",
  });
  assertRecognisedButRefused(result, "dispatch");
});
