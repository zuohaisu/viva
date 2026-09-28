/**
 * Viva office extension for the Pi coding agent (V09, issue #18).
 *
 * What this extension does — and honestly does not:
 * - Injects the member identity and current task context the office's Rust
 *   harness placed in the environment. The member is configuration data;
 *   nothing here hard-codes a member name.
 * - Registers office query tools (status, task brief), a CONTROLLED
 *   dispatch tool (refuses without a live grant — chat never self-
 *   authorizes) and an explicit handoff tool (a member report; it never
 *   marks a task complete).
 * - States plainly when no external memory system is connected. There is
 *   no fake Holographic, no "remembers", no "learned".
 * - Sessions without office context (plain `pi` runs) leave this extension
 *   inert: it registers nothing and claims nothing.
 *
 * Pi keeps its own chat UI and agent loop; the office keeps its own facts.
 */

import type { ExtensionAPI, ExtensionContext } from "./pi-api.ts";
import { officeEnv, planDispatch, runEnvelope, toolText, type OfficeEnv } from "./lib.ts";
import { registerComputerTools } from "./computer/computer.ts";
import { registerMemoryTools } from "./memory/memory.ts";

export default function vivaOffice(pi: ExtensionAPI): void {
  const env: OfficeEnv | null = officeEnv(process.env);
  // Not launched by the office: stay silent and claim nothing.
  if (!env) return;

  pi.on("session_start", async (_event: unknown, ctx: ExtensionContext) => {
    const lines: string[] = [];
    lines.push(
      `Viva office context loaded: you are ${env.memberName} (member ${env.memberId}). ` +
        "This identity comes from the office's member records, not from this chat.",
    );
    if (env.taskId) {
      lines.push(`Current task: ${env.taskId}. Use the viva_office_task_brief tool to read its real brief.`);
    } else {
      lines.push(
        "This is plain conversation: no task is attached, and chat carries no office-wide authority.",
      );
    }
    lines.push(
      "No external memory system is connected: nothing here claims to remember beyond what the office has recorded.",
    );
    ctx.ui.notify(lines.join("\n"), "info");
  });

  pi.registerTool("viva_office_status", {
    description:
      "Read the Viva office status: active host, terminals and execution counts (read-only).",
    parameters: { type: "object", properties: {}, additionalProperties: false },
    execute: async () => {
      try {
        const payload = await runEnvelope(env, ["office", "status"]);
        return { content: toolText(payload) };
      } catch (err) {
        return { content: `office status unavailable: ${String(err)}`, isError: true };
      }
    },
  });

  pi.registerTool("viva_office_task_brief", {
    description:
      "Read the current task's brief from the office records (read-only). Uses the task attached to this session.",
    parameters: {
      type: "object",
      properties: {
        task_id: { type: "string", description: "Optional; defaults to this session's task." },
      },
      additionalProperties: false,
    },
    execute: async (args) => {
      const taskId = typeof args.task_id === "string" && args.task_id ? args.task_id : env.taskId;
      if (!taskId) {
        return { content: "no task is attached to this session", isError: true };
      }
      try {
        const payload = await runEnvelope(env, ["office", "brief", taskId]);
        return { content: toolText(payload) };
      } catch (err) {
        return { content: `task brief unavailable: ${String(err)}`, isError: true };
      }
    },
  });

  pi.registerTool("viva_office_dispatch", {
    description:
      "Dispatch a task execution through the office under this session's grant. Refuses without a live grant: ask the user to authorize.",
    parameters: {
      type: "object",
      properties: {
        argv: {
          type: "array",
          items: { type: "string" },
          description: "Explicit program + arguments (never a joined shell string).",
        },
        cwd: { type: "string", description: "Absolute working directory (the task worktree)." },
        request_key: { type: "string", description: "Idempotency key so retries never double-start." },
        task_id: { type: "string", description: "Optional; defaults to this session's task." },
        grant_id: { type: "string", description: "Optional; defaults to this session's grant." },
      },
      required: ["argv", "cwd", "request_key"],
      additionalProperties: false,
    },
    execute: async (args) => {
      const argv = Array.isArray(args.argv)
        ? args.argv.filter((a): a is string => typeof a === "string")
        : [];
      const plan = planDispatch(env, {
        argv,
        cwd: typeof args.cwd === "string" ? args.cwd : "",
        requestKey: typeof args.request_key === "string" ? args.request_key : "",
        taskId: typeof args.task_id === "string" ? args.task_id : undefined,
        grantId: typeof args.grant_id === "string" ? args.grant_id : undefined,
      });
      if (plan.kind === "refuse") {
        // A refusal is a proposal, not a silent failure: the user is told
        // what authorization is missing.
        return { content: plan.reason, isError: true };
      }
      try {
        const payload = await runEnvelope(env, plan.argv.slice(1));
        return { content: toolText(payload) };
      } catch (err) {
        return { content: `dispatch failed: ${String(err)}`, isError: true };
      }
    },
  });

  pi.registerTool("viva_office_handoff", {
    description:
      "Record an explicit handoff summary for the current task (a member-reported fact; it never completes the task).",
    parameters: {
      type: "object",
      properties: {
        summary: { type: "string", description: "What was done, what remains, where the record lives." },
        task_id: { type: "string", description: "Optional; defaults to this session's task." },
      },
      required: ["summary"],
      additionalProperties: false,
    },
    execute: async (args) => {
      const taskId = typeof args.task_id === "string" && args.task_id ? args.task_id : env.taskId;
      if (!taskId) {
        return { content: "no task is attached to this session; nothing to hand off", isError: true };
      }
      const summary = typeof args.summary === "string" ? args.summary : "";
      if (!summary.trim()) {
        return { content: "handoff summary must not be empty", isError: true };
      }
      try {
        const payload = await runEnvelope(env, [
          "office",
          "handoff",
          "--task",
          taskId,
          "--member",
          env.memberId,
          "--summary",
          summary,
        ]);
        return { content: toolText(payload) };
      } catch (err) {
        return { content: `handoff not recorded: ${String(err)}`, isError: true };
      }
    },
  });


  // F03 (issue #25): the read-only computer audit tool. Input stays off
  // chat — it lives on the office's task-scoped grant path only.
  registerComputerTools(pi, env);

  // F04 (issue #26): office-linked external memory tools (scoped recall
  // with provenance; exits stay office-side).
  registerMemoryTools(pi, env);
}
