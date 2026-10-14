/**
 * Tests for the conversations sub-extension (V10): native-first fork
 * ordering, honest refusal without a fork API, office-only rename, and
 * capability-declared handoff. The office side is a fake `viva` binary.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { tryNativeFork } from "../conversations/index.ts";
import vivaConversations from "../conversations/index.ts";
import { officeEnv, type OfficeEnv } from "../lib.ts";
import { callTool, createFakePi, textOf, toolNamed, type FakePi } from "./fake-pi.ts";

const BASE_ENV: NodeJS.ProcessEnv = {
  VIVA_OFFICE_MEMBER_ID: "mem-1",
  VIVA_OFFICE_MEMBER_NAME: "Samuel",
  VIVA_HOME: "/tmp/viva-home",
};

function withEnv(raw: NodeJS.ProcessEnv, fn: () => void | Promise<void>): Promise<void> | void {
  const saved = { ...process.env };
  process.env = { ...raw } as NodeJS.ProcessEnv;
  const done = () => {
    process.env = saved;
  };
  try {
    const result = fn();
    if (result instanceof Promise) return result.finally(done);
    done();
  } catch (err) {
    done();
    throw err;
  }
}

function fakeViva(script: string): string {
  const dir = mkdtempSync(join(tmpdir(), "viva-conv-test-"));
  const path = join(dir, "fake-viva");
  writeFileSync(path, script);
  chmodSync(path, 0o755);
  return path;
}

function harnessWith(vivaBin: string): OfficeEnv {
  return { home: "/tmp/viva-home", memberId: "mem-1", memberName: "Samuel", vivaBin };
}

function fakePiWith(env: OfficeEnv): FakePi {
  const fake = createFakePi();
  process.env.VIVA_BIN = env.vivaBin;
  vivaConversations(fake.host);
  return fake;
}

test("tryNativeFork feature-detects the documented entries and refuses honestly", async () => {
  // A ctx with a fork-capable session manager.
  const forked = await tryNativeFork({
    sessionManager: {
      fork: async () => ({ sessionId: "pi-native-9", nodeId: "node-9" }),
    },
  } as never);
  assert.equal(forked?.sessionId, "pi-native-9");
  assert.equal(forked?.nodeId, "node-9");

  // No session manager at all -> null (no fork API, no pretending).
  assert.equal(await tryNativeFork({} as never), null);
  assert.equal(await tryNativeFork(undefined), null);
});

test("fork records in the office only after the native fork succeeds", async () => {
  await withEnv(BASE_ENV, async () => {
    // The fake viva records what it was asked, then answers with JSON.
    const viva = fakeViva(
      '#!/bin/sh\necho "$@" >> /tmp/viva-conv-calls.log\necho \'{"node_id": "node-new"}\'\n',
    );
    const fake = fakePiWith(harnessWith(viva));
    const fork = toolNamed(fake, "viva_conversations_fork");
    const result = await callTool(
      fork,
      { display_name: "Theme C", parent_node_id: "node-1" },
      { sessionManager: { fork: async () => ({ sessionId: "pi-native-8" }) } },
    );
    assert.equal(result.isError, undefined, `fork + record succeeded: ${textOf(result)}`);
    assert.match(textOf(result), /node-new/);
    assert.deepEqual(result.details, { node_id: "node-new" }, "the CLI payload is kept as details");
  });
});

test("fork without a native entry point refuses and records nothing", async () => {
  await withEnv(BASE_ENV, async () => {
    // The fake viva appends every invocation to a call log. The refusal
    // must mean the office CLI was NEVER called: `calls=0` inside a shell
    // script can't be observed from JS, but the log file can.
    const callLog = join(tmpdir(), `viva-conv-calls-${process.pid}-${Date.now()}.log`);
    const viva = fakeViva(`#!/bin/sh\necho "$@" >> ${callLog}\necho "{}"\n`);
    const fake = fakePiWith(harnessWith(viva));
    const fork = toolNamed(fake, "viva_conversations_fork");
    const result = await callTool(
      fork,
      { display_name: "Theme D", parent_node_id: "node-1" },
      { sessionManager: {} },
    );
    assert.equal(result.isError, true);
    assert.match(textOf(result), /no native fork entry point/);
    assert.match(textOf(result), /nothing was recorded/);
    assert.equal(
      existsSync(callLog),
      false,
      "the office CLI must never be invoked when the native fork is refused",
    );
  });
});

test("rename is office-owned and handoff declares its capability", async () => {
  await withEnv(BASE_ENV, async () => {
    const viva = fakeViva('#!/bin/sh\necho \'{"ok": true, "argv": "\'"\'"$*"\'"\'"}\'\n');
    const fake = fakePiWith(harnessWith(viva));

    const rename = toolNamed(fake, "viva_conversations_rename");
    const renamed = await callTool(rename, { node_id: "n1", display_name: "New name" });
    assert.match(textOf(renamed), /New name/);

    const handoff = toolNamed(fake, "viva_conversations_handoff");
    const result = await callTool(handoff, {
      node_id: "n1",
      to_harness: "codex",
      capability: "handoff_only",
      brief: "context summary",
    });
    assert.match(textOf(result), /ok/);
  });
});

test("the sub-extension stays inert without office context", () => {
  withEnv({ VIVA_OFFICE_MEMBER_NAME: "Samuel" }, () => {
    const fake = createFakePi();
    vivaConversations(fake.host);
    assert.equal(fake.tools.size, 0);
  });
});

// officeEnv import used by consumers of this test file; keep referenced.
void officeEnv;
