/**
 * Viva conversations sub-extension for the Pi coding agent (V10, issue #19).
 *
 * Registers conversation-tree tools on top of Viva's conversation
 * metadata: fork (native-first, Viva-records-second), rename (Viva
 * display name only), and handoff (explicit, capability-declared).
 *
 * Ordering rule enforced here: a fork is recorded in Viva only after
 * the harness's native fork succeeded. If this Pi build exposes no fork
 * API the tool refuses and proposes a handoff instead — it never records a
 * fork the harness did not confirm, and never pretends one happened.
 *
 * Sessions without Viva context stay inert, exactly like the main
 * extension.
 */

import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import {
  officeEnv,
  runEnvelope,
  toolFailure,
  toolSuccess,
  type OfficeEnv,
  type PiHost,
} from "../lib.ts";

interface NativeForkResult {
  sessionId: string;
  nodeId?: string;
}

/**
 * Try the harness's native fork. The exact fork entry point varies between
 * Pi versions, so this feature-detects the documented candidates and
 * reports honestly when none exists. Returns null when no fork API is
 * available — the caller must refuse, not improvise.
 *
 * In Pi 1.2.0 the tool context's `sessionManager` is read-only and has no
 * fork method, so this returns null there and the fork tool refuses and
 * proposes a handoff. That is the intended honest behaviour, not a gap to
 * paper over; the cast below is the feature detection.
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

export default function vivaConversations(pi: PiHost): void {
  const env: OfficeEnv | null = officeEnv(process.env);
  if (!env) return;

  pi.registerTool({
    name: "viva_conversations_fork",
    label: "Viva conversation fork",
    description:
      "Fork the current conversation into a new branch: Pi forks natively first, then Viva records the branch. Refuses when native fork is unavailable (use handoff).",
    parameters: Type.Object(
      {
        display_name: Type.String({ description: "Display name for the new branch." }),
        parent_node_id: Type.String({ description: "Node id to branch from." }),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params, _signal, _onUpdate, ctx) => {
      const displayName = params.display_name;
      const parentNode = params.parent_node_id;
      if (!displayName.trim() || !parentNode.trim()) {
        return toolFailure("fork needs a display name and a parent node id");
      }
      // 1) Native fork FIRST. A fork the harness did not confirm is never
      //    recorded as a Viva fact.
      const native = await tryNativeFork(ctx);
      if (!native) {
        return toolFailure(
          "this Pi build exposes no native fork entry point; nothing was forked and nothing was recorded — use viva_conversations_handoff instead",
        );
      }
      // 2) Viva record SECOND.
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
        return toolSuccess(payload);
      } catch (err) {
        // The fork happened natively but Viva's record failed: say so
        // plainly instead of hiding either fact.
        return toolFailure(
          `native fork succeeded (${native.sessionId}) but the Viva record failed: ${String(err)}`,
        );
      }
    },
  });

  pi.registerTool({
    name: "viva_conversations_rename",
    label: "Viva conversation rename",
    description:
      "Rename a branch in Viva (display name is Viva-owned metadata; the harness's own title stays untouched).",
    parameters: Type.Object(
      {
        node_id: Type.String(),
        display_name: Type.String(),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      if (!params.node_id.trim() || !params.display_name.trim()) {
        return toolFailure("rename needs a node id and a display name");
      }
      try {
        await runEnvelope(env, [
          "conversations",
          "rename",
          "--node",
          params.node_id,
          "--name",
          params.display_name,
        ]);
        return toolSuccess(`renamed to "${params.display_name}" in Viva`);
      } catch (err) {
        return toolFailure(`rename failed: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_conversations_handoff",
    label: "Viva conversation handoff",
    description:
      "Record a handoff of this conversation branch to another harness: identity, brief snapshot and a history reference travel; the original record stays traceable. No transcript conversion is promised.",
    parameters: Type.Object(
      {
        node_id: Type.String({ description: "Node id being handed off." }),
        to_harness: Type.String({ description: "Target harness name (e.g. codex)." }),
        capability: Type.Optional(
          Type.Unsafe<"native_fork" | "handoff_only">({
            type: "string",
            enum: ["native_fork", "handoff_only"],
            description: "The target harness's declared fork capability.",
          }),
        ),
        brief: Type.Optional(Type.String({ description: "Brief snapshot carried over." })),
        task_id: Type.Optional(Type.String({ description: "Optional task association." })),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      if (!params.node_id.trim() || !params.to_harness.trim()) {
        return toolFailure("handoff needs a node id and a target harness");
      }
      const capability = params.capability === "native_fork" ? "native_fork" : "handoff_only";
      const argv = [
        "conversations",
        "handoff",
        "--node",
        params.node_id,
        "--to",
        params.to_harness,
        "--capability",
        capability,
      ];
      if (params.brief?.trim()) argv.push("--brief", params.brief);
      if (params.task_id?.trim()) argv.push("--task", params.task_id);
      try {
        return toolSuccess(await runEnvelope(env, argv));
      } catch (err) {
        return toolFailure(`handoff not recorded: ${String(err)}`);
      }
    },
  });
}
