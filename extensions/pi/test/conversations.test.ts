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

type Tool = {
  execute: (args: Record<string, unknown>, ctx?: unknown) => Promise<{ content: string; isError?: boolean }>;
};

function fakePiWith(env: OfficeEnv) {
  const registered = new Map<string, Tool>();
  const fakePi = {
    on() {
      return () => {};
    },
    registerTool(name: string, tool: Tool) {
      registered.set(name, tool);
    },
    registerCommand() {},
  };
  process.env.VIVA_BIN = env.vivaBin;
  vivaConversations(fakePi as never);
  return registered;
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
    const registered = fakePiWith(harnessWith(viva));
    const fork = registered.get("viva_conversations_fork")!;
    const result = await fork.execute(
      { display_name: "Theme C", parent_node_id: "node-1" },
      { sessionManager: { fork: async () => ({ sessionId: "pi-native-8" }) } },
    );
    assert.equal(result.isError, undefined, "fork + record succeeded: {content}");
    assert.match(result.content, /node-new/);
  });
});

test("fork without a native entry point refuses and records nothing", async () => {
  await withEnv(BASE_ENV, async () => {
    // The fake viva appends every invocation to a call log. The refusal
    // must mean the office CLI was NEVER called: `calls=0` inside a shell
    // script can't be observed from JS, but the log file can.
    const callLog = join(tmpdir(), `viva-conv-calls-${process.pid}-${Date.now()}.log`);
    const viva = fakeViva(`#!/bin/sh\necho "$@" >> ${callLog}\necho "{}"\n`);
    const registered = fakePiWith(harnessWith(viva));
    const fork = registered.get("viva_conversations_fork")!;
    const result = await fork.execute(
      { display_name: "Theme D", parent_node_id: "node-1" },
      { sessionManager: {} },
    );
    assert.equal(result.isError, true);
    assert.match(result.content, /no native fork entry point/);
    assert.match(result.content, /nothing was recorded/);
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
    const registered = fakePiWith(harnessWith(viva));

    const rename = registered.get("viva_conversations_rename")!;
    const renamed = await rename.execute({ node_id: "n1", display_name: "New name" });
    assert.match(renamed.content, /New name/);

    const handoff = registered.get("viva_conversations_handoff")!;
    const result = await handoff.execute({
      node_id: "n1",
      to_harness: "codex",
      capability: "handoff_only",
      brief: "context summary",
    });
    assert.match(result.content, /ok/);
  });
});

test("the sub-extension stays inert without office context", () => {
  withEnv({ VIVA_OFFICE_MEMBER_NAME: "Samuel" }, () => {
    let tools = 0;
    vivaConversations({
      on() {
        return () => {};
      },
      registerTool() {
        tools += 1;
      },
      registerCommand() {},
    } as never);
    assert.equal(tools, 0);
  });
});

// officeEnv import used by consumers of this test file; keep referenced.
void officeEnv;
