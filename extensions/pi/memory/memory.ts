/**
 * External memory extension module for the Pi coding agent
 * (F04, issue #26).
 *
 * What this module does — and honestly does not:
 * - Registers two tools over Viva's memory link layer:
 *   `viva_memory_search` (read-only recall scoped to this session's
 *   member, with a context budget) and `viva_memory_remember` (an
 *   explicit write that always records provenance). Both go through the
 *   viva CLI, which filters by the member/project namespace and logs
 *   usage evidence.
 * - It does NOT expose memory exit as a chat tool: archive/restore are
 *   Viva-side commands with recorded reasons, and the physical delete
 *   of the underlying provider is never offered — archive is the exit,
 *   and it is recoverable.
 * - An unavailable store comes back as "unavailable": the agent is told
 *   the memory could not be reached instead of being handed an empty
 *   answer that looks like "nothing remembered".
 * - Sessions without Viva context stay inert.
 */

import { Type } from "typebox";
import { runEnvelope, toolFailure, toolSuccess, type VivaEnv, type PiHost } from "../lib.ts";

export function registerMemoryTools(pi: PiHost, env: VivaEnv): void {
  pi.registerTool({
    name: "viva_memory_search",
    label: "Viva memory search",
    description:
      "Search the Viva-linked external memory (Holographic) for facts linked to this " +
      "session's member. Read-only; recalls carry their provenance, and an unreachable " +
      "store is reported as unavailable.",
    parameters: Type.Object(
      {
        query: Type.String({
          description:
            "What to recall. Exact terms recall best; this provider's FTS does not paraphrase.",
        }),
        project_id: Type.Optional(Type.String({ description: "Optional project scope." })),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      const query = params.query.trim();
      if (!query) {
        return toolFailure("memory search needs a non-empty query");
      }
      const cliArgs = ["memory", "search", "--member", env.memberId, "--query", query];
      if (params.project_id) {
        cliArgs.push("--project", params.project_id);
      }
      try {
        return toolSuccess(await runEnvelope(env, cliArgs));
      } catch (err) {
        return toolFailure(`memory search unavailable: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_memory_remember",
    label: "Viva memory remember",
    description:
      "Write one durable fact into the Viva-linked external memory with provenance. " +
      "The source records who wrote it and from where; without a source Viva " +
      "refuses the write.",
    parameters: Type.Object(
      {
        content: Type.String({ description: "The fact, stated once and concretely." }),
        source: Type.Optional(
          Type.String({
            description:
              "Optional; defaults to a chat attribution for this session's member and task.",
          }),
        ),
        project_id: Type.Optional(Type.String({ description: "Optional project scope." })),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      const content = params.content.trim();
      if (!content) {
        return toolFailure("memory write needs non-empty content");
      }
      const source = params.source?.trim()
        ? params.source.trim()
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
      if (params.project_id) {
        cliArgs.push("--project", params.project_id);
      }
      try {
        return toolSuccess(await runEnvelope(env, cliArgs));
      } catch (err) {
        return toolFailure(`memory write refused: ${String(err)}`);
      }
    },
  });
}

/// The honest answer to "forget this" from chat: exits are Viva-side,
/// recorded, and recoverable; physical deletion is never offered.
export const MEMORY_EXIT_REFUSAL =
  "Memory exit is not a chat tool. Archiving a fact is a Viva command " +
  "(viva memory archive --fact <id> --reason <text>) that keeps the fact " +
  "recoverable; the underlying provider's physical delete is never " +
  "exposed, and archived facts stay resolvable by explicit request.";
