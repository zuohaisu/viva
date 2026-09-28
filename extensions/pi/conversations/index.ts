/**
 * Viva conversations sub-extension for the Pi coding agent (V10, issue #19).
 *
 * Registers conversation-tree tools on top of the office conversation
 * metadata: fork (native-first, office-records-second), rename (office
 * display name only), and handoff (explicit, capability-declared).
 *
 * Ordering rule enforced here: a fork is recorded in the office only after
 * the harness's native fork succeeded. If this Pi build exposes no fork
 * API the tool refuses and proposes a handoff instead — it never records a
 * fork the harness did not confirm, and never pretends one happened.
 *
 * Sessions without office context stay inert, exactly like the main
 * extension.
 */

import type { ExtensionAPI, ExtensionContext } from "../pi-api.ts";
import { officeEnv, runEnvelope, toolText, type OfficeEnv } from "../lib.ts";

interface NativeForkResult {
  sessionId: string;
  nodeId?: string;
}

/**
 * Try the harness's native fork. The exact fork entry point varies between
 * Pi versions, so this feature-detects the documented candidates and
 * reports honestly when none exists. Returns null when no fork API is
 * available — the caller must refuse, not improvise.
 */
export async function tryNativeFork(ctx?: ExtensionContext): Promise<NativeForkResult | null> {
  const sm = (ctx as unknown as { sessionManager?: Record<string, unknown> })?.sessionManager;
  if (!sm) return null;
  for (const name of ["fork", "forkBranch", "branchSession"]) {
    const candidate = sm[name];
    if (typeof candidate === "function") {
      const result = await (candidate as () => Promise<unknown>)();
      const record = (result ?? {}) as { sessionId?: unknown; id?: unknown; nodeId?: unknown };
      const sessionId = record.sessionId ?? record.id;
      if (typeof sessionId === "string" && sessionId) {
        return {
          sessionId,
          nodeId: typeof record.nodeId === "string" ? record.nodeId : undefined,
        };
      }
    }
  }
  return null;
}

export default function vivaConversations(pi: ExtensionAPI): void {
  const env: OfficeEnv | null = officeEnv(process.env);
  if (!env) return;

  pi.registerTool("viva_conversations_fork", {
    description:
      "Fork the current conversation into a new branch: Pi forks natively first, then the office records the branch. Refuses when native fork is unavailable (use handoff).",
    parameters: {
      type: "object",
      properties: {
        display_name: { type: "string", description: "Office display name for the new branch." },
        parent_node_id: { type: "string", description: "Office node id to branch from." },
      },
      required: ["display_name", "parent_node_id"],
      additionalProperties: false,
    },
    execute: async (args, ctx) => {
      const displayName = typeof args.display_name === "string" ? args.display_name : "";
      const parentNode = typeof args.parent_node_id === "string" ? args.parent_node_id : "";
      if (!displayName.trim() || !parentNode.trim()) {
        return { content: "fork needs a display name and a parent node id", isError: true };
      }
      // 1) Native fork FIRST. A fork the harness did not confirm is never
      //    recorded as an office fact.
      const native = await tryNativeFork(ctx);
      if (!native) {
        return {
          content:
            "this Pi build exposes no native fork entry point; nothing was forked and nothing was recorded — use viva_conversations_handoff instead",
          isError: true,
        };
      }
      // 2) Office record SECOND.
      try {
        const payload = await runEnvelope(env, [
          "conversations",
          "fork",
          "--parent",
          parentNode,
          "--name",
          displayName,
          "--native-session",
          native.sessionId,
          ...(native.nodeId ? ["--native-node", native.nodeId] : []),
        ]);
        return { content: toolText(payload) };
      } catch (err) {
        // The fork happened natively but the office record failed: say so
        // plainly instead of hiding either fact.
        return {
          content: `native fork succeeded (${native.sessionId}) but the office record failed: ${String(err)}`,
          isError: true,
        };
      }
    },
  });

  pi.registerTool("viva_conversations_rename", {
    description:
      "Rename a branch in the office (display name is office-owned metadata; the harness's own title stays untouched).",
    parameters: {
      type: "object",
      properties: {
        node_id: { type: "string" },
        display_name: { type: "string" },
      },
      required: ["node_id", "display_name"],
      additionalProperties: false,
    },
    execute: async (args) => {
      const nodeId = typeof args.node_id === "string" ? args.node_id : "";
      const displayName = typeof args.display_name === "string" ? args.display_name : "";
      if (!nodeId.trim() || !displayName.trim()) {
        return { content: "rename needs a node id and a display name", isError: true };
      }
      try {
        await runEnvelope(env, ["conversations", "rename", "--node", nodeId, "--name", displayName]);
        return { content: `renamed to "${displayName}" in the office` };
      } catch (err) {
        return { content: `rename failed: ${String(err)}`, isError: true };
      }
    },
  });

  pi.registerTool("viva_conversations_handoff", {
    description:
      "Record a handoff of this conversation branch to another harness: identity, brief snapshot and a history reference travel; the original record stays traceable. No transcript conversion is promised.",
    parameters: {
      type: "object",
      properties: {
        node_id: { type: "string", description: "Office node id being handed off." },
        to_harness: { type: "string", description: "Target harness name (e.g. codex)." },
        capability: {
          type: "string",
          enum: ["native_fork", "handoff_only"],
          description: "The target harness's declared fork capability.",
        },
        brief: { type: "string", description: "Brief snapshot carried over." },
        task_id: { type: "string", description: "Optional task association." },
      },
      required: ["node_id", "to_harness"],
      additionalProperties: false,
    },
    execute: async (args) => {
      const nodeId = typeof args.node_id === "string" ? args.node_id : "";
      const toHarness = typeof args.to_harness === "string" ? args.to_harness : "";
      if (!nodeId.trim() || !toHarness.trim()) {
        return { content: "handoff needs a node id and a target harness", isError: true };
      }
      const capability = args.capability === "native_fork" ? "native_fork" : "handoff_only";
      const argv = [
        "conversations",
        "handoff",
        "--node",
        nodeId,
        "--to",
        toHarness,
        "--capability",
        capability,
      ];
      if (typeof args.brief === "string" && args.brief.trim()) argv.push("--brief", args.brief);
      if (typeof args.task_id === "string" && args.task_id.trim()) argv.push("--task", args.task_id);
      try {
        const payload = await runEnvelope(env, argv);
        return { content: toolText(payload) };
      } catch (err) {
        return { content: `handoff not recorded: ${String(err)}`, isError: true };
      }
    },
  });
}
