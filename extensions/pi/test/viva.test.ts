/**
 * Tests for the Viva Pi extension (V09). Run with:
 *   npm test   (node --experimental-strip-types --test test/)
 *
 * The subprocess envelope is tested against a fake `viva` binary so the
 * whole chain (plan -> spawn -> parse -> refusal) is exercised for real,
 * without a live server. Pi itself is `fake-pi.ts`, typed against the real
 * SDK.
 *
 * Two fakes, two blind spots, each covered by a real-environment test:
 * - A fake `viva` accepts ANY arguments, so nothing in this file proves the
 *   real CLI understands the commands the extension emits:
 *   `viva.contract.test.ts` runs them against the real `viva` binary
 *   (build it first: `cargo build -p viva`).
 * - A fake Pi proves only the extension's own logic: `real-pi-load.test.ts`
 *   loads the extension in the real Pi.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, chmodSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { readEnv, planDispatch, runEnvelope } from "../lib.ts";
import viva, { approvedPrincipleBodies } from "../viva.ts";
import { callTool, createFakePi, emit, textOf, toolNamed } from "./fake-pi.ts";

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
  VIVA_MEMBER_ID: "mem-1",
  VIVA_MEMBER_NAME: "Samuel",
  VIVA_HOME: "/tmp/viva-home",
  VIVA_TASK_ID: "task-1",
  VIVA_GRANT_ID: "grant-1",
};

test("readEnv parses the harness context and stays inert without identity", () => {
  withEnv(BASE_ENV, () => {
    const env = readEnv(process.env);
    assert.ok(env, "Viva launch must produce context");
    assert.equal(env.memberId, "mem-1");
    assert.equal(env.taskId, "task-1");
    assert.equal(env.grantId, "grant-1");
    assert.equal(env.vivaBin, "viva");
  });

  withEnv({ VIVA_MEMBER_NAME: "Samuel" }, () => {
    assert.equal(readEnv(process.env), null, "no member id -> no Viva context");
  });

  withEnv({ VIVA_MEMBER_ID: "mem-1" }, () => {
    assert.equal(readEnv(process.env), null, "no member name -> no Viva context");
  });
});

test("planDispatch refuses without a grant and proposes asking the user", () => {
  withEnv({ ...BASE_ENV, VIVA_GRANT_ID: "" }, () => {
    const env = readEnv(process.env)!;
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
  withEnv({ ...BASE_ENV, VIVA_TASK_ID: "", VIVA_GRANT_ID: "" }, () => {
    const env = readEnv(process.env)!;
    const noTask = planDispatch(env, { argv: ["/bin/true"], cwd: "/tmp", requestKey: "rk" });
    assert.equal(noTask.kind, "refuse");
  });
  withEnv(BASE_ENV, () => {
    const env = readEnv(process.env)!;
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
    const env = readEnv(process.env)!;
    const plan = planDispatch(env, {
      argv: ["/bin/echo", "hi"],
      cwd: "/wt/one",
      requestKey: "rk-9",
    });
    assert.equal(plan.kind, "run");
    if (plan.kind === "run") {
      assert.deepEqual(plan.argv, [
        "viva",
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
    const env = readEnv(process.env)!;

    const good = fakeViva('#!/bin/sh\necho \'{"ok": true}\'\n');
    const payload = await runEnvelope({ ...env, vivaBin: good }, ["status"]);
    assert.ok((payload as { ok: boolean }).ok);

    const failing = fakeViva('#!/bin/sh\necho "no active server" >&2\nexit 3\n');
    await assert.rejects(
      runEnvelope({ ...env, vivaBin: failing }, ["dispatch"]),
      /exit 3[\s\S]*no active server/,
    );

    const malformed = fakeViva('#!/bin/sh\necho "not json"\n');
    await assert.rejects(
      runEnvelope({ ...env, vivaBin: malformed }, ["status"]),
      /malformed JSON/,
    );

    await assert.rejects(
      runEnvelope({ ...env, vivaBin: "/nonexistent/viva" }, ["status"]),
      /not usable/,
    );
  });
});

test("the extension registers its tools and refuses unauthorized dispatch", async () => {
  // No grant in the environment from the start: the factory captures the
  // Viva context at load time, so the authorization must already be
  // absent when it runs.
  await withEnv({ ...BASE_ENV, VIVA_GRANT_ID: "" }, async () => {
    const fake = createFakePi();
    viva(fake.host);
    assert.deepEqual(
      [...fake.tools.keys()].sort(),
      [
        "viva_computer_audit",
        "viva_dispatch",
        "viva_handoff",
        "viva_memory_remember",
        "viva_memory_search",
        "viva_status",
        "viva_task_brief",
      ],
    );

    const refusalResult = await callTool(toolNamed(fake, "viva_dispatch"), {
      argv: ["/bin/true"],
      cwd: "/tmp",
      request_key: "rk-1",
    });
    assert.equal(refusalResult.isError, true);
    assert.match(textOf(refusalResult), /grant/);
    assert.match(textOf(refusalResult), /ask the user/);
  });
});

test("tools answer in Pi's result shape: text content, structured details, isError on failure", async () => {
  // The factory captures VIVA_BIN at load time, so set it before loading.
  const load = (vivaBin: string) => {
    process.env.VIVA_BIN = vivaBin;
    const fake = createFakePi();
    viva(fake.host);
    return fake;
  };
  await withEnv(BASE_ENV, async () => {
    const good = load(fakeViva('#!/bin/sh\necho \'{"home": "/h", "terminals": []}\'\n'));
    const okResult = await callTool(toolNamed(good, "viva_status"), {});
    assert.equal(okResult.isError, undefined);
    assert.deepEqual(JSON.parse(textOf(okResult)), { home: "/h", terminals: [] });
    assert.deepEqual(okResult.details, { home: "/h", terminals: [] }, "the CLI payload is kept as details");

    const bad = load(fakeViva('#!/bin/sh\necho "no active server" >&2\nexit 3\n'));
    const badResult = await callTool(toolNamed(bad, "viva_status"), {});
    assert.equal(badResult.isError, true);
    assert.match(textOf(badResult), /Viva status unavailable/);
    assert.match(textOf(badResult), /no active server/);
    assert.deepEqual(badResult.details, {});
  });
});

test("the session_start handler tells the user who the member is, at the real 'info' level", async () => {
  await withEnv(BASE_ENV, async () => {
    const fake = createFakePi();
    viva(fake.host);
    const notices: { message: string; level?: string }[] = [];
    await emit(fake, "session_start", { type: "session_start", reason: "startup" }, {
      ui: {
        notify(message: string, level?: string) {
          notices.push({ message, level });
        },
      },
    });
    assert.equal(notices.length, 1);
    assert.equal(notices[0]?.level, "info");
    assert.match(notices[0]?.message ?? "", /you are Samuel \(member mem-1\)/);
    assert.match(notices[0]?.message ?? "", /Current task: task-1/);
  });
});

test("approvedPrincipleBodies keeps only current approved text", () => {
  assert.deepEqual(
    approvedPrincipleBodies({
      principles: [{ body: "  Ask which repository before editing.  " }],
      candidates: [{ statement: "Ship without a recorded execution." }],
    }),
    ["Ask which repository before editing."],
  );
  assert.deepEqual(approvedPrincipleBodies({ principles: [] }), []);
});

test("the session notice shows approved principles and not other payload fields", async () => {
  const bin = fakeViva(`#!/bin/sh
case "$1 $2" in
  "principle approved")
    echo '{"principles":[{"body":"Ask which repository before editing."}],"candidates":[{"statement":"Ship without a recorded execution."}]}'
    ;;
  *)
    echo '{"ok":true}'
    ;;
esac
`);
  await withEnv({ ...BASE_ENV, VIVA_BIN: bin }, async () => {
    const fake = createFakePi();
    viva(fake.host);
    const notices: string[] = [];
    await emit(fake, "session_start", { type: "session_start", reason: "startup" }, {
      ui: { notify(message: string) { notices.push(message); } },
    });
    const notice = notices.join("\n");
    assert.match(notice, /Ask which repository before editing\./);
    assert.doesNotMatch(notice, /Ship without a recorded execution/);
  });
});

test("the extension stays inert without Viva context", () => {
  withEnv({ VIVA_MEMBER_NAME: "Samuel" }, () => {
    const fake = createFakePi();
    viva(fake.host);
    assert.equal(fake.tools.size, 0, "no Viva context -> no tools, no claims");
    assert.equal(fake.handlers.size, 0, "no Viva context -> no event handlers either");
  });
});
