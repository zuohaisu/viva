/**
 * Tests for the computer operations extension module (F03, issue #25).
 * Run with: npm test
 *
 * The audit tool is exercised against a fake `viva` binary: the real
 * chain (tool -> viva CLI -> payload) runs for real, without needing a
 * live server or any computer tool.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { readEnv } from "../lib.ts";
import { COMPUTER_INPUT_REFUSAL, registerComputerTools } from "../computer/computer.ts";
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

function fakeViva(script: string): string {
  const dir = mkdtempSync(join(tmpdir(), "viva-computer-test-"));
  const path = join(dir, "fake-viva");
  writeFileSync(path, script);
  chmodSync(path, 0o755);
  return path;
}

test("the main entry registers the read-only computer audit tool", () => {
  withEnv(BASE_ENV, () => {
    const env = readEnv(process.env);
    assert.ok(env, "Viva context must parse");
    const fake = createFakePi();
    viva(fake.host);
    assert.ok(
      fake.tools.has("viva_computer_audit"),
      "the audit tool is registered alongside Viva tools",
    );
    assert.ok(
      ![...fake.tools.keys()].some((name) => name.includes("type") || name.includes("click")),
      "no input tool is ever registered from chat",
    );
  });
});

test("the audit tool runs the viva CLI and reports its payload", () =>
  withEnv(BASE_ENV, async () => {
    const env = readEnv(process.env)!;
    env.vivaBin = fakeViva('#!/bin/sh\necho \'{ "audited": ["orca-computer"] }\'\n');
    const fake = createFakePi();
    registerComputerTools(fake.host, env);

    const result = await callTool(toolNamed(fake, "viva_computer_audit"), {});
    assert.equal(result.isError, undefined);
    assert.match(textOf(result), /orca-computer/);
    assert.deepEqual(result.details, { audited: ["orca-computer"] }, "the CLI payload is kept as details");
  }));

test("an unavailable audit is an honest error, never a fake pass", () =>
  withEnv(BASE_ENV, async () => {
    const env = readEnv(process.env)!;
    env.vivaBin = fakeViva('#!/bin/sh\necho "viva: store missing" >&2\nexit 1\n');
    const fake = createFakePi();
    registerComputerTools(fake.host, env);

    const result = await callTool(toolNamed(fake, "viva_computer_audit"), {});
    assert.equal(result.isError, true);
    assert.match(textOf(result), /unavailable/);
  }));

test("computer input stays behind the task-scoped grant path, off chat", () => {
  assert.match(COMPUTER_INPUT_REFUSAL, /computer_input/);
  assert.match(COMPUTER_INPUT_REFUSAL, /task-scoped/);
  assert.match(COMPUTER_INPUT_REFUSAL, /evidence/);
});
