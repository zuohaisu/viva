/**
 * Tests for the Viva office extension (V09). Run with:
 *   npm test   (node --experimental-strip-types --test test/)
 *
 * The subprocess envelope is tested against a fake `viva` binary so the
 * whole chain (plan -> spawn -> parse -> refusal) is exercised for real,
 * without needing the Rust binary or a live office.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, chmodSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { officeEnv, planDispatch, runEnvelope, type OfficeEnv } from "../lib.ts";
import vivaOffice from "../viva-office.ts";

function withEnv(raw: NodeJS.ProcessEnv, fn: () => void | Promise<void>): Promise<void> | void {
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
  const dir = mkdtempSync(join(tmpdir(), "viva-ext-test-"));
  const path = join(dir, "fake-viva");
  writeFileSync(path, script);
  chmodSync(path, 0o755);
  return path;
}

const BASE_ENV: NodeJS.ProcessEnv = {
  VIVA_OFFICE_MEMBER_ID: "mem-1",
  VIVA_OFFICE_MEMBER_NAME: "Samuel",
  VIVA_HOME: "/tmp/viva-home",
  VIVA_OFFICE_TASK_ID: "task-1",
  VIVA_OFFICE_GRANT_ID: "grant-1",
};

test("officeEnv parses the harness context and stays inert without identity", () => {
  withEnv(BASE_ENV, () => {
    const env = officeEnv(process.env);
    assert.ok(env, "office launch must produce context");
    assert.equal(env.memberId, "mem-1");
    assert.equal(env.taskId, "task-1");
    assert.equal(env.grantId, "grant-1");
    assert.equal(env.vivaBin, "viva");
  });

  withEnv({ VIVA_OFFICE_MEMBER_NAME: "Samuel" }, () => {
    assert.equal(officeEnv(process.env), null, "no member id -> no office context");
  });

  withEnv({ VIVA_OFFICE_MEMBER_ID: "mem-1" }, () => {
    assert.equal(officeEnv(process.env), null, "no member name -> no office context");
  });
});

test("planDispatch refuses without a grant and proposes asking the user", () => {
  withEnv({ ...BASE_ENV, VIVA_OFFICE_GRANT_ID: "" }, () => {
    const env = officeEnv(process.env)!;
    const noGrant = planDispatch(env, {
      argv: ["/bin/true"],
      cwd: "/tmp",
      requestKey: "rk-1",
    });
    assert.equal(noGrant.kind, "refuse");
    if (noGrant.kind === "refuse") {
      assert.match(noGrant.reason, /grant/);
      assert.match(noGrant.reason, /ask the user/);
    }
  });
});

test("planDispatch refuses without task, argv, absolute cwd or request key", () => {
  withEnv({ ...BASE_ENV, VIVA_OFFICE_TASK_ID: "", VIVA_OFFICE_GRANT_ID: "" }, () => {
    const env = officeEnv(process.env)!;
    const noTask = planDispatch(env, { argv: ["/bin/true"], cwd: "/tmp", requestKey: "rk" });
    assert.equal(noTask.kind, "refuse");
  });
  withEnv(BASE_ENV, () => {
    const env = officeEnv(process.env)!;
    assert.equal(
      planDispatch(env, { argv: [], cwd: "/tmp", requestKey: "rk" }).kind,
      "refuse",
      "empty argv",
    );
    assert.equal(
      planDispatch(env, { argv: ["/bin/true"], cwd: "relative", requestKey: "rk" }).kind,
      "refuse",
      "relative cwd",
    );
    assert.equal(
      planDispatch(env, { argv: ["/bin/true"], cwd: "/tmp", requestKey: "" }).kind,
      "refuse",
      "blank request key",
    );
  });
});

test("planDispatch builds the fixed CLI envelope argv", () => {
  withEnv(BASE_ENV, () => {
    const env = officeEnv(process.env)!;
    const plan = planDispatch(env, {
      argv: ["/bin/echo", "hi"],
      cwd: "/wt/one",
      requestKey: "rk-9",
    });
    assert.equal(plan.kind, "run");
    if (plan.kind === "run") {
      assert.deepEqual(plan.argv, [
        "viva",
        "office",
        "dispatch",
        "--task",
        "task-1",
        "--member",
        "mem-1",
        "--grant",
        "grant-1",
        "--request-key",
        "rk-9",
        "--cwd",
        "/wt/one",
        "--",
        "/bin/echo",
        "hi",
      ]);
    }
  });
});

test("runEnvelope parses real CLI JSON and surfaces real failures", async () => {
  await withEnv(BASE_ENV, async () => {
    const env = officeEnv(process.env)!;

    const good = fakeViva('#!/bin/sh\necho \'{"ok": true}\'\n');
    const payload = await runEnvelope({ ...env, vivaBin: good }, ["office", "status"]);
    assert.ok((payload as { ok: boolean }).ok);

    const failing = fakeViva('#!/bin/sh\necho "no active office" >&2\nexit 3\n');
    await assert.rejects(
      runEnvelope({ ...env, vivaBin: failing }, ["office", "dispatch"]),
      /exit 3[\s\S]*no active office/,
    );

    const malformed = fakeViva('#!/bin/sh\necho "not json"\n');
    await assert.rejects(
      runEnvelope({ ...env, vivaBin: malformed }, ["office", "status"]),
      /malformed JSON/,
    );

    await assert.rejects(
      runEnvelope({ ...env, vivaBin: "/nonexistent/viva" }, ["office", "status"]),
      /not usable/,
    );
  });
});

test("the extension registers office tools and refuses unauthorized dispatch", async () => {
  // No grant in the environment from the start: the factory captures the
  // office context at load time, so the authorization must already be
  // absent when it runs.
  await withEnv({ ...BASE_ENV, VIVA_OFFICE_GRANT_ID: "" }, async () => {
    const registered: Map<string, { execute: (args: Record<string, unknown>) => Promise<{ content: string; isError?: boolean }> }> =
      new Map();
    const notified: string[] = [];
    const fakePi = {
      on(_event: string, _handler: unknown) {
        return () => {};
      },
      registerTool(name: string, tool: { execute: (args: Record<string, unknown>) => Promise<{ content: string; isError?: boolean }> }) {
        registered.set(name, tool);
      },
      registerCommand(_name: string, _command: unknown) {},
    };

    vivaOffice(fakePi as never);
    assert.deepEqual(
      [...registered.keys()].sort(),
      [
        "viva_computer_audit",
        "viva_memory_remember",
        "viva_memory_search",
        "viva_office_dispatch",
        "viva_office_handoff",
        "viva_office_status",
        "viva_office_task_brief",
      ],
    );

    const refusalResult = await registered.get("viva_office_dispatch")!.execute({
      argv: ["/bin/true"],
      cwd: "/tmp",
      request_key: "rk-1",
    });
    assert.equal(refusalResult.isError, true);
    assert.match(refusalResult.content, /grant/);
    assert.match(refusalResult.content, /ask the user/);
  });
});

test("the extension stays inert without office context", () => {
  withEnv({ VIVA_OFFICE_MEMBER_NAME: "Samuel" }, () => {
    let tools = 0;
    const fakePi = {
      on() {
        return () => {};
      },
      registerTool() {
        tools += 1;
      },
      registerCommand() {},
    };
    vivaOffice(fakePi as never);
    assert.equal(tools, 0, "no office context -> no tools, no claims");
  });
});
