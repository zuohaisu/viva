/**
 * Viva extension for the Pi coding agent (V09, issue #18).
 *
 * What this extension does — and honestly does not:
 * - Injects the member identity and current task context Viva's Rust
 *   harness placed in the environment. The member is configuration data;
 *   nothing here hard-codes a member name.
 * - Registers query tools (status, task brief), a CONTROLLED
 *   dispatch tool (refuses without a live grant — chat never self-
 *   authorizes) and an explicit handoff tool (a member report; it never
 *   marks a task complete).
 * - States plainly when no external memory system is connected. There is
 *   no fake Holographic, no "remembers", no "learned".
 * - Sessions without Viva context (plain `pi` runs) leave this extension
 *   inert: it registers nothing and claims nothing.
 *
 * Pi keeps its own chat UI and agent loop; Viva keeps its own facts.
 *
 * Tools use Pi's real `registerTool({ name, label, description, parameters,
 * execute })` form with TypeBox parameter schemas.
 * `test/real-pi-load.test.ts` loads this file in the real Pi and checks the
 * tools it registers.
 */

import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import {
  readEnv,
  planDispatch,
  runEnvelope,
  toolFailure,
  toolSuccess,
  type VivaEnv,
  type PiHost,
} from "./lib.ts";
import { registerComputerTools } from "./computer/computer.ts";
import { registerMemoryTools } from "./memory/memory.ts";

export default function viva(pi: PiHost): void {
  const env: VivaEnv | null = readEnv(process.env);
  // Not launched by Viva: stay silent and claim nothing.
  if (!env) return;

  pi.on("session_start", async (_event, ctx: ExtensionContext) => {
    const lines: string[] = [];
    const executionId = process.env.VIVA_EXECUTION_ID;
    if (executionId) {
      try {
        await runEnvelope(env, [
          "experience",
          "link",
          "--execution",
          executionId,
          "--harness",
          "pi",
          "--session",
          ctx.sessionManager.getSessionId(),
        ]);
      } catch (err) {
        lines.push(
          `Experience link was not recorded (${String(err)}). The session still runs; it stays unassigned until the execution is linked.`,
        );
      }
    }
    lines.push(
      `Viva context loaded: you are ${env.memberName} (member ${env.memberId}). ` +
        "This identity comes from Viva's member records, not from this chat.",
    );
    if (env.taskId) {
      lines.push(`Current task: ${env.taskId}. Use the viva_task_brief tool to read its real brief.`);
    } else {
      lines.push(
        "This is plain conversation: no task is attached, and chat carries no Viva-wide authority.",
      );
    }
    try {
      const status = await runEnvelope(env, ["memory", "status"]);
      const text = typeof status === "string" ? status : JSON.stringify(status);
      if (
        text.toLowerCase().includes("\"ok\": false") ||
        text.toLowerCase().includes("unavailable")
      ) {
        lines.push(
          "External memory: not ready on this machine — run `viva init` if this home is new.",
        );
      } else {
        lines.push(`External memory (native): ${text.trim()}`);
      }
    } catch {
      lines.push(
        "External memory: could not reach Viva memory status — recall/remember may be unavailable.",
      );
    }
    ctx.ui.notify(lines.join("\n"), "info");
  });

  pi.registerTool({
    name: "viva_status",
    label: "Viva status",
    description:
      "Read Viva's status: active host, terminals and execution counts (read-only).",
    parameters: Type.Object({}, { additionalProperties: false }),
    execute: async () => {
      try {
        return toolSuccess(await runEnvelope(env, ["status"]));
      } catch (err) {
        return toolFailure(`Viva status unavailable: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_task_brief",
    label: "Viva task brief",
    description:
      "Read the current task's brief from Viva's records (read-only). Uses the task attached to this session.",
    parameters: Type.Object(
      {
        task_id: Type.Optional(
          Type.String({ description: "Optional; defaults to this session's task." }),
        ),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      const taskId = params.task_id || env.taskId;
      if (!taskId) {
        return toolFailure("no task is attached to this session");
      }
      try {
        return toolSuccess(await runEnvelope(env, ["brief", taskId]));
      } catch (err) {
        return toolFailure(`task brief unavailable: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_dispatch",
    label: "Viva dispatch",
    description:
      "Dispatch a task execution through Viva under this session's grant. Refuses without a live grant: ask the user to authorize.",
    parameters: Type.Object(
      {
        argv: Type.Array(Type.String(), {
          description: "Explicit program + arguments (never a joined shell string).",
        }),
        cwd: Type.String({ description: "Absolute working directory (the task worktree)." }),
        request_key: Type.String({ description: "Idempotency key so retries never double-start." }),
        task_id: Type.Optional(
          Type.String({ description: "Optional; defaults to this session's task." }),
        ),
        grant_id: Type.Optional(
          Type.String({ description: "Optional; defaults to this session's grant." }),
        ),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      const plan = planDispatch(env, {
        argv: params.argv,
        cwd: params.cwd,
        requestKey: params.request_key,
        taskId: params.task_id,
        grantId: params.grant_id,
      });
      if (plan.kind === "refuse") {
        // A refusal is a proposal, not a silent failure: the user is told
        // what authorization is missing.
        return toolFailure(plan.reason);
      }
      try {
        return toolSuccess(await runEnvelope(env, plan.argv.slice(1)));
      } catch (err) {
        return toolFailure(`dispatch failed: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_handoff",
    label: "Viva handoff",
    description:
      "Record an explicit handoff summary for the current task (a member-reported fact; it never completes the task).",
    parameters: Type.Object(
      {
        summary: Type.String({
          description: "What was done, what remains, where the record lives.",
        }),
        task_id: Type.Optional(
          Type.String({ description: "Optional; defaults to this session's task." }),
        ),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      const taskId = params.task_id || env.taskId;
      if (!taskId) {
        return toolFailure("no task is attached to this session; nothing to hand off");
      }
      if (!params.summary.trim()) {
        return toolFailure("handoff summary must not be empty");
      }
      try {
        const payload = await runEnvelope(env, [
          "handoff",
          "--task",
          taskId,
          "--member",
          env.memberId,
          "--summary",
          params.summary,
        ]);
        return toolSuccess(payload);
      } catch (err) {
        return toolFailure(`handoff not recorded: ${String(err)}`);
      }
    },
  });

  // F03 (issue #25): the read-only computer audit tool. Input stays off
  // chat — it lives on Viva's task-scoped grant path only.
  registerComputerTools(pi, env);

  // F04 (issue #26): Viva-linked external memory tools (scoped recall
  // with provenance; exits stay Viva-side).
  registerMemoryTools(pi, env);
}
