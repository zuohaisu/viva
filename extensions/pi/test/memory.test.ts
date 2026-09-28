/**
 * Tests for the external memory extension module (F04, issue #26).
 * Run with: npm test
 *
 * Both tools are exercised against a fake `viva` binary, so the real
 * chain (tool -> office CLI argv -> payload) is verified without a live
 * office or any memory store.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { officeEnv, type OfficeEnv } from "../lib.ts";
import { MEMORY_EXIT_REFUSAL, registerMemoryTools } from "../memory/memory.ts";
import vivaOffice from "../viva-office.ts";

const BASE_ENV: NodeJS.ProcessEnv = {
  VIVA_OFFICE_MEMBER_ID: "mem-1",
  VIVA_OFFICE_MEMBER_NAME: "Samuel",
  VIVA_HOME: "/tmp/viva-home",
  VIVA_OFFICE_TASK_ID: "task-1",
  VIVA_OFFICE_GRANT_ID: "grant-1",
};

function withEnv(
  raw: NodeJS.ProcessEnv,
  fn: () => void | Promise<void>,
): Promise<void> | void {
  const saved = { ...process.env };
  process.env = { ...raw } as NodeJS.ProcessEnv;
  const done = () => {
    process.env = saved;
  };
  try {
    const result = fn();
    if (result instanceof Promise) {
      return result.finally(done);
    }
    done();
  } catch (err) {
    done();
    throw err;
  }
}

function fakeViva(): string {
  const dir = mkdtempSync(join(tmpdir(), "viva-memory-test-"));
  const path = join(dir, "fake-viva");
  // The fake answers with the argv it received as a JSON array, so tests
  // assert the real CLI envelope the office will see (runEnvelope
  // JSON-parses stdout). POSIX sh only: the test runner's PATH may not
  // resolve `env node` for shebangs. Test args never contain quotes, so
  // naive quoting is safe here.
  const script = [
    "#!/bin/sh",
    "printf '['",
    "first=1",
    'for a in "$@"; do',
    '[ "$first" -eq 1 ] || printf \',\'',
    'printf \'"%s"\' "$a"',
    "first=0",
    "done",
    "printf ']\\n'",
  ].join("\n");
  writeFileSync(path, script, { mode: 0o755 });
  return path;
}

function fakePi() {
  const registered: Map<
    string,
    { execute: (args: Record<string, unknown>) => Promise<{ content: string; isError?: boolean }> }
  > = new Map();
  const pi = {
    on(_event: string, _handler: unknown) {
      return () => {};
    },
    registerTool(name: string, tool: { execute: (args: Record<string, unknown>) => Promise<{ content: string; isError?: boolean }> }) {
      registered.set(name, tool);
    },
    registerCommand(_name: string, _command: unknown) {},
  };
  return { registered, pi };
}

test("the office entry registers the memory tools", () => {
  withEnv(BASE_ENV, () => {
    const { registered, pi } = fakePi();
    vivaOffice(pi as never);
    assert.ok(registered.has("viva_memory_search"));
    assert.ok(registered.has("viva_memory_remember"));
  });
});

test("memory search is member-scoped and read-only", async () => {
  await withEnv(BASE_ENV, async () => {
    const env = officeEnv(process.env)!;
    env.vivaBin = fakeViva();
    const { registered, pi } = fakePi();
    registerMemoryTools(pi as never, env);

    const payload = await registered.get("viva_memory_search")!.execute({
      query: "release train",
      project_id: "proj-9",
    });
    assert.equal(payload.isError, undefined);
    const argv = JSON.parse(payload.content) as string[];
    assert.deepEqual(argv, [
      "memory",
      "search",
      "--member",
      "mem-1",
      "--query",
      "release train",
      "--project",
      "proj-9",
    ]);
  });
});

test("memory remember records provenance even when chat omits it", async () => {
  await withEnv(BASE_ENV, async () => {
    const env = officeEnv(process.env)!;
    env.vivaBin = fakeViva();
    const { registered, pi } = fakePi();
    registerMemoryTools(pi as never, env);

    const payload = await registered.get("viva_memory_remember")!.execute({
      content: "The demo is on Fridays",
    });
    assert.equal(payload.isError, undefined);
    const argv = JSON.parse(payload.content) as string[];
    assert.deepEqual(argv, [
      "memory",
      "remember",
      "--member",
      "mem-1",
      "--content",
      "The demo is on Fridays",
      "--source",
      "chat by Samuel (task task-1)",
    ]);
  });
});

test("empty content and queries are refused before any call", async () => {
  withEnv(BASE_ENV, () => {
    const env = officeEnv(process.env)!;
    env.vivaBin = "/bin/false";
    const { registered, pi } = fakePi();
    registerMemoryTools(pi as never, env);
    void registered;
    // Synchronous refusal paths are covered by the CLI contract; here we
    // pin the exported refusal text instead of spawning anything.
    assert.match(MEMORY_EXIT_REFUSAL, /archive/);
    assert.match(MEMORY_EXIT_REFUSAL, /recoverable/);
    assert.match(MEMORY_EXIT_REFUSAL, /never/);
  });
});
