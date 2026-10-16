/**
 * Tests for the external memory extension module (F04, issue #26).
 * Run with: npm test
 *
 * Both tools are exercised against a fake `viva` binary, so the real
 * chain (tool -> viva CLI argv -> payload) is verified without a live
 * server or any memory store.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { readEnv } from "../lib.ts";
import { MEMORY_EXIT_REFUSAL, registerMemoryTools } from "../memory/memory.ts";
import viva from "../viva.ts";
import { callTool, createFakePi, textOf, toolNamed } from "./fake-pi.ts";

const BASE_ENV: NodeJS.ProcessEnv = {
  VIVA_MEMBER_ID: "mem-1",
  VIVA_MEMBER_NAME: "Samuel",
  VIVA_HOME: "/tmp/viva-home",
  VIVA_TASK_ID: "task-1",
  VIVA_GRANT_ID: "grant-1",
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
  // assert the real CLI envelope Viva will see (runEnvelope
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

test("the main entry registers the memory tools", () => {
  withEnv(BASE_ENV, () => {
    const fake = createFakePi();
    viva(fake.host);
    assert.ok(fake.tools.has("viva_memory_search"));
    assert.ok(fake.tools.has("viva_memory_remember"));
  });
});

test("memory search is member-scoped and read-only", async () => {
  await withEnv(BASE_ENV, async () => {
    const env = readEnv(process.env)!;
    env.vivaBin = fakeViva();
    const fake = createFakePi();
    registerMemoryTools(fake.host, env);

    const result = await callTool(toolNamed(fake, "viva_memory_search"), {
      query: "release train",
      project_id: "proj-9",
    });
    assert.equal(result.isError, undefined);
    const argv = JSON.parse(textOf(result)) as string[];
    assert.deepEqual(result.details, argv, "the CLI payload is kept as details");
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
    const env = readEnv(process.env)!;
    env.vivaBin = fakeViva();
    const fake = createFakePi();
    registerMemoryTools(fake.host, env);

    const result = await callTool(toolNamed(fake, "viva_memory_remember"), {
      content: "The demo is on Fridays",
    });
    assert.equal(result.isError, undefined);
    const argv = JSON.parse(textOf(result)) as string[];
    assert.deepEqual(argv, [
      "memory",
      "remember",
      "--member",
      "mem-1",
      "--content",
      "The demo is on Fridays",
      "--source",
      "chat by Samuel (task task-1)",
      "--emphasis",
    ]);
  });
});

test("empty content and queries are refused before any call", async () => {
  await withEnv(BASE_ENV, async () => {
    const env = readEnv(process.env)!;
    // /bin/false would make any real CLI call fail as "unavailable"/"refused",
    // so the exact refusal text below proves nothing was spawned.
    env.vivaBin = "/bin/false";
    const fake = createFakePi();
    registerMemoryTools(fake.host, env);

    const search = await callTool(toolNamed(fake, "viva_memory_search"), { query: "   " });
    assert.equal(search.isError, true);
    assert.equal(textOf(search), "memory search needs a non-empty query");

    const write = await callTool(toolNamed(fake, "viva_memory_remember"), { content: "   " });
    assert.equal(write.isError, true);
    assert.equal(textOf(write), "memory write needs non-empty content");

    // The exit path is not a chat tool; the refusal text is pinned instead.
    assert.match(MEMORY_EXIT_REFUSAL, /archive/);
    assert.match(MEMORY_EXIT_REFUSAL, /recoverable/);
    assert.match(MEMORY_EXIT_REFUSAL, /never/);
  });
});
