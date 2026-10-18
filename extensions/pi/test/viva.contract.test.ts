/**
 * Contract test: the commands the Pi extension emits must be recognised by
 * the REAL `viva` binary (issue #86), and the names the Rust harness injects
 * must be the names the extension reads (issue #96).
 *
 * Why this exists: `viva.test.ts` drives the extension against a fake
 * `viva` script that returns fixed JSON for ANY arguments. It stayed green
 * while the extension kept calling a retired command namespace, which the
 * real binary rejects with "unknown or missing subcommand". A fake
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
 * The other half of the contract lives in Rust: the environment variable
 * names and the extension path are read straight out of
 * `crates/viva/src/harness/pi/mod.rs`, so renaming them on only one side
 * fails here. The extension is loaded from the path the harness hands to Pi.
 *
 * The binary comes from VIVA_BIN, else `target/debug/viva`:
 *   cargo build -p viva && npm test
 * A missing binary is a FAILURE, never a skip: a skipped contract test
 * proves nothing.
 */

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import type { AgentToolResult } from "@earendil-works/pi-coding-agent";

import type { PiHost } from "../lib.ts";
import {
  callTool,
  createFakePi,
  emit,
  textOf,
  toolNamed,
  type FakePi,
  type RegisteredTool,
} from "./fake-pi.ts";

type ToolResult = AgentToolResult<unknown>;

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const HARNESS_SOURCE = join(REPO_ROOT, "crates", "viva", "src", "harness", "pi", "mod.rs");

/** What the real binary says when it does not recognise a command. */
const UNRECOGNISED = /unknown or missing subcommand/;
/** What Viva says when no resident server is running: a business-layer refusal. */
const NO_ACTIVE_SERVER = /no active server/;

const GOAL = "contract test goal";

/** The contract, spelled out once. The Rust constants must equal these. */
const NAMES = {
  memberId: "VIVA_MEMBER_ID",
  memberName: "VIVA_MEMBER_NAME",
  taskId: "VIVA_TASK_ID",
  grantId: "VIVA_GRANT_ID",
  extension: "extensions/pi/viva.ts",
} as const;

/** The variable names the contract used before issue #96. They must be dead. */
const RETIRED = {
  memberId: "VIVA_OFFICE_MEMBER_ID",
  memberName: "VIVA_OFFICE_MEMBER_NAME",
  taskId: "VIVA_OFFICE_TASK_ID",
  grantId: "VIVA_OFFICE_GRANT_ID",
} as const;

const TOOLS = [
  "viva_computer_audit",
  "viva_dispatch",
  "viva_handoff",
  "viva_memory_remember",
  "viva_memory_search",
  "viva_status",
  "viva_task_brief",
];

interface Fixture {
  bin: string;
  root: string;
  home: string;
  taskId: string;
  extensionFactory: (pi: PiHost) => void;
  fake: FakePi;
}

let fx: Fixture | undefined;

/** `pub const NAME: &str = "value";`, read from the Rust harness source. */
function harnessConstant(name: string): string {
  const source = readFileSync(HARNESS_SOURCE, "utf8");
  const found = new RegExp(`pub const ${name}: &str = "([^"]+)";`).exec(source);
  assert.ok(
    found?.[1],
    `${HARNESS_SOURCE} no longer defines \`pub const ${name}: &str = "...";\`. ` +
      "This test reads the Rust side of the contract from that line; keep it a plain string constant.",
  );
  return found[1];
}

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
function loadExtension(raw: NodeJS.ProcessEnv, extensionFactory: (pi: PiHost) => void): FakePi {
  const fake = createFakePi();
  const saved = process.env;
  process.env = raw;
  try {
    extensionFactory(fake.host);
  } finally {
    process.env = saved;
  }
  return fake;
}

/** The harness-injected environment, under the names the Rust side defines. */
function harnessEnv(fixture: Pick<Fixture, "bin" | "home" | "taskId">, withTask = true, withGrant = true) {
  const env: Record<string, string> = {
    VIVA_BIN: fixture.bin,
    VIVA_HOME: fixture.home,
    [harnessConstant("ENV_MEMBER_ID")]: "mem-00000000000000000000000000000001",
    [harnessConstant("ENV_MEMBER_NAME")]: "Contract Member",
  };
  if (withTask) env[harnessConstant("ENV_TASK_ID")] = fixture.taskId;
  if (withGrant) env[harnessConstant("ENV_GRANT_ID")] = "grant-00000000000000000000000000000001";
  return cleanEnv(env);
}

before(async () => {
  const bin = resolveVivaBinary();
  const root = mkdtempSync(join(tmpdir(), "viva-contract-"));
  const home = join(root, "home");
  mkdirSync(home, { mode: 0o700 });

  const extensionPath = join(REPO_ROOT, harnessConstant("EXTENSION_RELATIVE_PATH"));
  assert.ok(existsSync(extensionPath), `the harness launches ${extensionPath}, which does not exist`);
  const loaded = (await import(pathToFileURL(extensionPath).href)) as { default: (pi: PiHost) => void };

  const init = runViva(bin, home, ["init"]);
  assert.equal(init.status, 0, `viva init failed: ${init.stderr}`);
  const task = runVivaJson(bin, home, ["create-task", "--goal", GOAL]);
  const taskId = String(task.task_id);

  const harnessEnvForInit = cleanEnv({
    VIVA_BIN: bin,
    VIVA_HOME: home,
    [harnessConstant("ENV_MEMBER_ID")]: "mem-00000000000000000000000000000001",
    [harnessConstant("ENV_MEMBER_NAME")]: "Contract Member",
    [harnessConstant("ENV_TASK_ID")]: taskId,
    [harnessConstant("ENV_GRANT_ID")]: "grant-00000000000000000000000000000001",
  });
  const fake = loadExtension(harnessEnvForInit, loaded.default);

  fx = { bin, root, home, taskId, extensionFactory: loaded.default, fake };
});

after(() => {
  if (fx) rmSync(fx.root, { recursive: true, force: true });
});

function tool(name: string): RegisteredTool {
  assert.ok(fx);
  return toolNamed(fx.fake, name);
}

/**
 * The command was understood by the CLI and refused by Viva, not by
 * the argument parser. (No resident server runs in this test.)
 */
function assertRecognisedButRefused(result: ToolResult, what: string): void {
  assert.equal(result.isError, true, `${what}: with no server running the call must be refused`);
  assert.doesNotMatch(textOf(result), UNRECOGNISED, `${what}: the real CLI did not recognise the command`);
  assert.match(textOf(result), NO_ACTIVE_SERVER, `${what}: expected Viva's own refusal`);
}

test("the Rust harness defines exactly the names this contract expects", () => {
  assert.equal(harnessConstant("ENV_MEMBER_ID"), NAMES.memberId);
  assert.equal(harnessConstant("ENV_MEMBER_NAME"), NAMES.memberName);
  assert.equal(harnessConstant("ENV_TASK_ID"), NAMES.taskId);
  assert.equal(harnessConstant("ENV_GRANT_ID"), NAMES.grantId);
  assert.equal(harnessConstant("EXTENSION_RELATIVE_PATH"), NAMES.extension);
});

test("the harness-injected variables activate the extension and register the documented tools", () => {
  assert.ok(fx);
  assert.deepEqual([...fx.fake.tools.keys()].sort(), TOOLS);
});

test("the retired VIVA_OFFICE_* variables no longer activate the extension", () => {
  assert.ok(fx);
  const fake = loadExtension(
    cleanEnv({
      VIVA_BIN: fx.bin,
      VIVA_HOME: fx.home,
      [RETIRED.memberId]: "mem-00000000000000000000000000000001",
      [RETIRED.memberName]: "Contract Member",
      [RETIRED.taskId]: fx.taskId,
      [RETIRED.grantId]: "grant-00000000000000000000000000000001",
    }),
    fx.extensionFactory,
  );
  assert.deepEqual([...fake.tools.keys()], [], "the old names must not be honoured, not even as a fallback");
});

test("a retired task or grant variable next to the new identity is ignored", async () => {
  assert.ok(fx);
  const oldTask = loadExtension(
    cleanEnv({
      VIVA_BIN: fx.bin,
      VIVA_HOME: fx.home,
      [NAMES.memberId]: "mem-00000000000000000000000000000001",
      [NAMES.memberName]: "Contract Member",
      [RETIRED.taskId]: fx.taskId,
    }),
    fx.extensionFactory,
  );
  const brief = await callTool(toolNamed(oldTask, "viva_task_brief"), {});
  assert.equal(brief.isError, true, `an old task variable must not attach a task: ${textOf(brief)}`);
  assert.match(textOf(brief), /no task is attached/);

  const oldGrant = loadExtension(
    cleanEnv({
      VIVA_BIN: fx.bin,
      VIVA_HOME: fx.home,
      [NAMES.memberId]: "mem-00000000000000000000000000000001",
      [NAMES.memberName]: "Contract Member",
      [NAMES.taskId]: fx.taskId,
      [RETIRED.grantId]: "grant-00000000000000000000000000000001",
    }),
    fx.extensionFactory,
  );
  const dispatch = await callTool(toolNamed(oldGrant, "viva_dispatch"), {
    argv: ["/bin/true"],
    cwd: fx.root,
    request_key: "contract-test-old-grant",
  });
  assert.equal(dispatch.isError, true, `an old grant variable must not authorise dispatch: ${textOf(dispatch)}`);
  assert.match(textOf(dispatch), /no live grant/);
});

test("the session notice and tool descriptions speak of Viva, not of an office", async () => {
  assert.ok(fx);
  const fake = loadExtension(harnessEnv(fx), fx.extensionFactory);
  const notices: string[] = [];
  await emit(fake, "session_start", {}, { ui: { notify: (text: string) => notices.push(text) } });
  const notice = notices.join("\n");
  assert.match(notice, /^Viva context loaded: you are Contract Member/);
  assert.match(notice, /comes from Viva's member records/);
  assert.match(notice, /viva_task_brief/);
  assert.doesNotMatch(notice, /office/i);

  for (const name of ["viva_status", "viva_task_brief", "viva_dispatch", "viva_handoff"]) {
    assert.doesNotMatch(toolNamed(fake, name).description ?? "", /office/i, `${name}: the description names an office`);
  }

  const plain = loadExtension(harnessEnv(fx, false, false), fx.extensionFactory);
  const plainNotices: string[] = [];
  await emit(plain, "session_start", {}, { ui: { notify: (text: string) => plainNotices.push(text) } });
  assert.match(plainNotices.join("\n"), /no Viva-wide authority/);
  assert.doesNotMatch(plainNotices.join("\n"), /office/i);
});

test("control: the real binary rejects an unknown command namespace", () => {
  assert.ok(fx);
  const out = runViva(fx.bin, fx.home, ["no-such-namespace", "status"]);
  assert.notEqual(out.status, 0, "`viva no-such-namespace status` must be rejected");
  assert.match(out.stderr, UNRECOGNISED);
  assert.match(out.stdout, /USAGE/, "the real binary prints its USAGE banner for an unknown command");
});

test("viva_status is accepted and returns the real status", async () => {
  assert.ok(fx);
  const result = await callTool(tool("viva_status"), {});
  assert.notEqual(result.isError, true, `status failed: ${textOf(result)}`);
  const payload = JSON.parse(textOf(result)) as { home?: string };
  assert.equal(typeof payload.home, "string", "status payload names its home");
  assert.equal(realpathSync(payload.home as string), realpathSync(fx.home));
});

test("viva_task_brief is accepted and returns the real task brief", async () => {
  const result = await callTool(tool("viva_task_brief"), {});
  assert.notEqual(result.isError, true, `brief failed: ${textOf(result)}`);
  const payload = JSON.parse(textOf(result)) as { goal?: string };
  assert.equal(payload.goal, GOAL);
  assert.deepEqual(result.details, payload, "the real CLI payload is kept as details");
});

test("viva_handoff is recognised by the real CLI", async () => {
  const result = await callTool(tool("viva_handoff"), { summary: "contract test handoff" });
  assertRecognisedButRefused(result, "handoff");
});

test("viva_dispatch is recognised by the real CLI", async () => {
  assert.ok(fx);
  const result = await callTool(tool("viva_dispatch"), {
    argv: ["/bin/true"],
    cwd: fx.root,
    request_key: "contract-test-request-key",
  });
  assertRecognisedButRefused(result, "dispatch");
});

test("Pi session notice and both harness projections show only an approved principle", async () => {
  assert.ok(fx);
  const home = mkdtempSync(join(tmpdir(), "viva-principle-pi-"));
  const out = join(home, "projection");
  const member = "mem-00000000000000000000000000000002";
  const statement = "Ask which repository before editing.";
  const sourceA = "exec-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const sourceB = "exec-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  try {
    const init = runViva(fx.bin, home, ["init"]);
    assert.equal(init.status, 0, init.stderr);
    const digest = runVivaJson(fx.bin, home, [
      "principle",
      "digest",
      "--member",
      member,
      "--statement",
      statement,
      "--execution",
      sourceA,
      "--execution",
      sourceB,
    ]);
    const proposed = digest.proposed as Array<{ candidate_id: string }>;
    assert.equal(proposed.length, 1);
    const candidateId = proposed[0].candidate_id;

    runVivaJson(fx.bin, home, ["principle", "project", "--member", member, "--out", out]);
    const piBefore = readFileSync(join(out, "pi", "principles.md"), "utf8");
    const claudeBefore = readFileSync(join(out, "claude", "principles.md"), "utf8");
    assert.doesNotMatch(piBefore, /Ask which repository/);
    assert.doesNotMatch(claudeBefore, /Ask which repository/);

    const env = cleanEnv({
      VIVA_BIN: fx.bin,
      VIVA_HOME: home,
      [harnessConstant("ENV_MEMBER_ID")]: member,
      [harnessConstant("ENV_MEMBER_NAME")]: "Contract Member",
    });
    const before = loadExtension(env, fx.extensionFactory);
    const beforeNotices: string[] = [];
    await emit(before, "session_start", {}, {
      ui: { notify: (text: string) => beforeNotices.push(text) },
    });
    assert.doesNotMatch(beforeNotices.join("\n"), /Ask which repository/);

    runVivaJson(fx.bin, home, ["principle", "approve", "--candidate", candidateId]);
    runVivaJson(fx.bin, home, ["principle", "project", "--member", member, "--out", out]);
    const piAfter = readFileSync(join(out, "pi", "principles.md"), "utf8");
    const claudeAfter = readFileSync(join(out, "claude", "principles.md"), "utf8");
    assert.match(piAfter, /Ask which repository before editing\./);
    assert.match(claudeAfter, /Ask which repository before editing\./);
    assert.equal(piAfter.split("\n").slice(1).join("\n"), claudeAfter.split("\n").slice(1).join("\n"));

    const after = loadExtension(env, fx.extensionFactory);
    const afterNotices: string[] = [];
    await emit(after, "session_start", {}, {
      ui: { notify: (text: string) => afterNotices.push(text) },
    });
    assert.match(afterNotices.join("\n"), /Ask which repository before editing\./);
  } finally {
    rmSync(home, { recursive: true, force: true });
  }
});
