/**
 * External memory extension module for the Pi coding agent
 * (F04, issue #26).
 *
 * What this module does — and honestly does not:
 * - Registers two tools over the office's memory link layer:
 *   `viva_memory_search` (read-only recall scoped to this session's
 *   member, with a context budget) and `viva_memory_remember` (an
 *   explicit write that always records provenance). Both go through the
 *   office CLI, which filters by the member/project namespace and logs
 *   usage evidence.
 * - It does NOT expose memory exit as a chat tool: archive/restore are
 *   office-side commands with recorded reasons, and the physical delete
 *   of the underlying provider is never offered — archive is the exit,
 *   and it is recoverable.
 * - An unavailable store comes back as "unavailable": the agent is told
 *   the memory could not be reached instead of being handed an empty
 *   answer that looks like "nothing remembered".
 * - Sessions without office context stay inert.
 */

import type { ExtensionAPI } from "../pi-api.ts";
import { runEnvelope, toolText, type OfficeEnv } from "../lib.ts";

export function registerMemoryTools(pi: ExtensionAPI, env: OfficeEnv): void {
  pi.registerTool("viva_memory_search", {
    description:
      "Search the office-linked external memory (Holographic) for facts linked to this " +
      "session's member. Read-only; recalls carry their provenance, and an unreachable " +
      "store is reported as unavailable.",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: "What to recall. Exact terms recall best; " +
          "this provider's FTS does not paraphrase." },
        project_id: { type: "string", description: "Optional project scope." },
      },
      required: ["query"],
      additionalProperties: false,
    },
    execute: async (args) => {
      const query = typeof args.query === "string" ? args.query.trim() : "";
      if (!query) {
        return { content: "memory search needs a non-empty query", isError: true };
      }
      const cliArgs = ["memory", "search", "--member", env.memberId, "--query", query];
      if (typeof args.project_id === "string" && args.project_id) {
        cliArgs.push("--project", args.project_id);
      }
      try {
        const payload = await runEnvelope(env, cliArgs);
        return { content: toolText(payload) };
      } catch (err) {
        return { content: `memory search unavailable: ${String(err)}`, isError: true };
      }
    },
  });

  pi.registerTool("viva_memory_remember", {
    description:
      "Write one durable fact into the office-linked external memory with provenance. " +
      "The source records who wrote it and from where; without a source the office " +
      "refuses the write.",
    parameters: {
      type: "object",
      properties: {
        content: { type: "string", description: "The fact, stated once and concretely." },
        source: { type: "string", description: "Optional; defaults to a chat attribution " +
          "for this session's member and task." },
        project_id: { type: "string", description: "Optional project scope." },
      },
      required: ["content"],
      additionalProperties: false,
    },
    execute: async (args) => {
      const content = typeof args.content === "string" ? args.content.trim() : "";
      if (!content) {
        return { content: "memory write needs non-empty content", isError: true };
      }
      const source =
        typeof args.source === "string" && args.source.trim()
          ? args.source.trim()
          : `chat by ${env.memberName}` + (env.taskId ? ` (task ${env.taskId})` : "");
      const cliArgs = [
        "memory",
        "remember",
        "--member",
        env.memberId,
        "--content",
        content,
        "--source",
        source,
      ];
      if (typeof args.project_id === "string" && args.project_id) {
        cliArgs.push("--project", args.project_id);
      }
      try {
        const payload = await runEnvelope(env, cliArgs);
        return { content: toolText(payload) };
      } catch (err) {
        return { content: `memory write refused: ${String(err)}`, isError: true };
      }
    },
  });
}

/// The honest answer to "forget this" from chat: exits are office-side,
/// recorded, and recoverable; physical deletion is never offered.
export const MEMORY_EXIT_REFUSAL =
  "Memory exit is not a chat tool. Archiving a fact is an office command " +
  "(viva memory archive --fact <id> --reason <text>) that keeps the fact " +
  "recoverable; the underlying provider's physical delete is never " +
  "exposed, and archived facts stay resolvable by explicit request.";
