/**
 * Envelope plumbing for the Viva Pi extension (V09, issue #18).
 *
 * The fixed CLI envelope: the extension calls the `viva` binary's
 * top-level control commands — `status`, `brief`, `dispatch`, `handoff`
 * (JSON on stdout, non-zero exit on rejection) — and never speaks for
 * Viva itself. There is no separate namespace to type through, and
 * `test/viva.contract.test.ts` runs these commands against the real
 * binary. Everything here is deliberately boring and side-effect-free
 * unless it is actually spawning that CLI.
 *
 * Authority rules this module enforces structurally:
 * - Without a member identity in the environment the extension has no
 *   Viva context and stays inert (`readEnv` returns null).
 * - Without a live grant reference there is no dispatch: `planDispatch`
 *   returns a refusal that proposes asking the user for authorization.
 *   A chat session never receives Viva-wide authority.
 */

import { spawnSync } from "node:child_process";

import type { AgentToolResult, ExtensionAPI } from "@earendil-works/pi-coding-agent";

/**
 * The part of Pi's extension API this package uses. Extension factories take
 * this narrower type, so a test double only has to supply these two members,
 * and `npm run typecheck` fails if their signatures stop matching the real
 * `ExtensionAPI`. Real Pi's `ExtensionAPI` is assignable to it.
 */
export type PiHost = Pick<ExtensionAPI, "on" | "registerTool">;

export interface VivaEnv {
  home: string;
  memberId: string;
  memberName: string;
  taskId?: string;
  grantId?: string;
  vivaBin: string;
}

/**
 * Parse the Viva context the Rust harness injected into the environment
 * (see `crates/viva/src/harness/pi/mod.rs` for the contract). Returns null
 * when the session was not launched by Viva — the extension must
 * then claim nothing about membership or tasks.
 */
export function readEnv(raw: NodeJS.ProcessEnv): VivaEnv | null {
  const memberId = raw.VIVA_MEMBER_ID;
  const memberName = raw.VIVA_MEMBER_NAME;
  if (!memberId || !memberName) return null;
  return {
    home: raw.VIVA_HOME ?? "",
    memberId,
    memberName,
    taskId: raw.VIVA_TASK_ID || undefined,
    grantId: raw.VIVA_GRANT_ID || undefined,
    vivaBin: raw.VIVA_BIN || "viva",
  };
}

export type DispatchPlan =
  | { kind: "run"; argv: string[] }
  | { kind: "refuse"; reason: string };

export interface DispatchRequest {
  taskId?: string;
  grantId?: string;
  /** Explicit program + arguments for the execution; never a shell string. */
  argv: string[];
  /** Absolute working location (the task worktree). */
  cwd: string;
  /** Caller-provided idempotency key; retries reuse the same key. */
  requestKey: string;
}

/**
 * Decide how a dispatch request may proceed. Missing grant or missing task
 * context is a REFUSAL with a proposal — Viva's dispatch endpoint
 * enforces the grant again server-side; this refusal is the extension-side
 * guarantee that a chat without authorization cannot even attempt it.
 */
export function planDispatch(env: VivaEnv, req: DispatchRequest): DispatchPlan {
  const taskId = req.taskId ?? env.taskId;
  const grantId = req.grantId ?? env.grantId;
  if (!taskId) {
    return {
      kind: "refuse",
      reason:
        "no task is attached to this session; create a task in Viva and dispatch from there",
    };
  }
  if (!grantId) {
    return {
      kind: "refuse",
      reason:
        "no live grant covers this dispatch; ask the user to authorize one for this task " +
        "(Viva issues grants; chat cannot self-authorize)",
    };
  }
  if (!req.argv || req.argv.length === 0 || !req.argv[0] || req.argv[0].trim() === "") {
    return { kind: "refuse", reason: "dispatch needs explicit argv naming a program" };
  }
  if (!req.cwd || !req.cwd.startsWith("/")) {
    return { kind: "refuse", reason: "dispatch needs an absolute working directory" };
  }
  if (!req.requestKey || req.requestKey.trim() === "") {
    return { kind: "refuse", reason: "dispatch needs a request key so retries cannot double-start" };
  }
  return {
    kind: "run",
    argv: [
      env.vivaBin,
      "dispatch",
      "--task",
      taskId,
      "--member",
      env.memberId,
      "--grant",
      grantId,
      "--request-key",
      req.requestKey,
      "--cwd",
      req.cwd,
      "--",
      ...req.argv,
    ],
  };
}

export interface EnvelopeOutcome {
  ok: boolean;
  payload: unknown;
  error?: string;
}

/**
 * Run one viva CLI call through the fixed envelope: JSON on stdout, a
 * non-zero exit meaning rejection. Network/host problems surface as thrown
 * errors with the CLI's own words — the extension never invents an outcome.
 */
export function runEnvelope(env: VivaEnv, args: string[]): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const result = spawnSync(env.vivaBin, args, {
      env: { ...process.env, VIVA_HOME: env.home },
      encoding: "utf8",
      timeout: 30_000,
    });
    if (result.error) {
      reject(new Error(`viva CLI not usable (${env.vivaBin}): ${result.error.message}`));
      return;
    }
    if (result.status !== 0) {
      reject(
        new Error(
          `Viva rejected the request (exit ${result.status}): ${result.stderr.trim() || "no reason given"}`,
        ),
      );
      return;
    }
    try {
      resolve(JSON.parse(result.stdout));
    } catch {
      reject(
        new Error(
          `viva CLI returned malformed JSON: ${result.stdout.slice(0, 200) || "(empty)"}`,
        ),
      );
    }
  });
}

/** Stringify a tool result honestly: real payload text or the real error. */
export function toolText(value: unknown): string {
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

/**
 * What a Viva tool hands back to Pi: `content` is what the model reads,
 * `details` carries the CLI's structured payload for logs and the UI, and
 * `isError` marks a failure without throwing (Pi's `AgentToolResult`).
 */
export type ToolOutcome = AgentToolResult<unknown>;

/**
 * A successful tool result: the CLI payload as text for the model, and, when
 * it is structured (an object or array), as `details`; otherwise `{}`.
 */
export function toolSuccess(payload: unknown): ToolOutcome {
  return {
    content: [{ type: "text", text: toolText(payload) }],
    details: typeof payload === "object" && payload !== null ? payload : {},
  };
}

/** A failed tool result: the real reason as text; the model sees an error. */
export function toolFailure(message: string): ToolOutcome {
  return { content: [{ type: "text", text: message }], details: {}, isError: true };
}
