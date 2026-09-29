/**
 * Tests for the computer operations extension module (F03, issue #25).
 * Run with: npm test
 *
 * The audit tool is exercised against a fake `viva` binary: the real
 * chain (tool -> office CLI -> payload) runs for real, without needing a
 * live office or any computer tool.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { officeEnv, type OfficeEnv } from "../lib.ts";
import { COMPUTER_INPUT_REFUSAL, registerComputerTools } from "../computer/computer.ts";
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

function fakeViva(script: string): string {
  const dir = mkdtempSync(join(tmpdir(), "viva-computer-test-"));
  const path = join(dir, "fake-viva");
  writeFileSync(path, script);
  chmodSync(path, 0o755);
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

test("the office entry registers the read-only computer audit tool", () => {
  withEnv(BASE_ENV, () => {
    const env = officeEnv(process.env);
    assert.ok(env, "office context must parse");
    const { registered, pi } = fakePi();
    vivaOffice(pi as never);
    assert.ok(
      registered.has("viva_computer_audit"),
      "the audit tool is registered alongside the office tools",
    );
    assert.ok(
      ![...registered.keys()].some((name) => name.includes("type") || name.includes("click")),
      "no input tool is ever registered from chat",
    );
  });
});

test("the audit tool runs the office CLI and reports its payload", () =>
  withEnv(BASE_ENV, async () => {
    const env = officeEnv(process.env)!;
    env.vivaBin = fakeViva('#!/bin/sh\necho \'{ "audited": ["orca-computer"] }\'\n');
    const { registered, pi } = fakePi();
    registerComputerTools(pi as never, env);

    const payload = await registered.get("viva_computer_audit")!.execute({});
    assert.equal(payload.isError, undefined);
    assert.match(payload.content, /orca-computer/);
  }));

test("an unavailable audit is an honest error, never a fake pass", () =>
  withEnv(BASE_ENV, async () => {
    const env = officeEnv(process.env)!;
    env.vivaBin = fakeViva('#!/bin/sh\necho "viva: store missing" >&2\nexit 1\n');
    const { registered, pi } = fakePi();
    registerComputerTools(pi as never, env);

    const payload = await registered.get("viva_computer_audit")!.execute({});
    assert.equal(payload.isError, true);
    assert.match(payload.content, /unavailable/);
  }));

test("computer input stays behind the task-scoped grant path, off chat", () => {
  assert.match(COMPUTER_INPUT_REFUSAL, /computer_input/);
  assert.match(COMPUTER_INPUT_REFUSAL, /task-scoped/);
  assert.match(COMPUTER_INPUT_REFUSAL, /evidence/);
});
