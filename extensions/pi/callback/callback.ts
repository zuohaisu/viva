/**
 * Member callback tools (issue #117): recall, dialog state, and skill
 * reads. Each call is a `viva` CLI command. The process environment
 * carries the execution and grant; these tools do not pass a member id.
 */

import { Type } from "typebox";
import { runEnvelope, toolFailure, toolSuccess, type PiHost, type VivaEnv } from "../lib.ts";

export function registerCallbackTools(pi: PiHost, env: VivaEnv): void {
  pi.registerTool({
    name: "viva_memory_recall",
    label: "Viva memory recall",
    description:
      "Recall knowledge this execution may see. not_ready, no_match, and backend_error stay distinct. " +
      "Does not pass a member id.",
    parameters: Type.Object(
      {
        query: Type.String({ description: "What to recall." }),
        project_id: Type.Optional(
          Type.String({ description: "Must be this execution's project, if set." }),
        ),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      const query = params.query.trim();
      if (!query) return toolFailure("memory recall needs a non-empty query");
      const cliArgs = ["memory", "recall", "--query", query];
      if (params.project_id) cliArgs.push("--project", params.project_id);
      try {
        return toolSuccess(await runEnvelope(env, cliArgs));
      } catch (err) {
        return toolFailure(`memory recall failed: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_dialog_state_get",
    label: "Viva dialog state",
    description: "Read the dialog state explicitly stored on this execution. Does not infer one.",
    parameters: Type.Object({}, { additionalProperties: false }),
    execute: async () => {
      try {
        return toolSuccess(await runEnvelope(env, ["memory", "state", "get"]));
      } catch (err) {
        return toolFailure(`dialog state read failed: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_dialog_state_set",
    label: "Set Viva dialog state",
    description:
      "Store an explicit dialog state on this execution. work or personal only; " +
      "this cannot widen visibility past the caller's own personal knowledge.",
    parameters: Type.Object(
      {
        domain: Type.String({ description: "work or personal." }),
        project_id: Type.Optional(Type.String()),
        interlocutor: Type.Optional(Type.String()),
        activity: Type.Optional(Type.String()),
        by: Type.Optional(Type.String({ description: "command or haisu. Defaults to command." })),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      const domain = params.domain.trim();
      if (domain !== "work" && domain !== "personal") {
        return toolFailure("dialog state domain must be work or personal");
      }
      const cliArgs = ["memory", "state", "set", "--domain", domain];
      if (params.project_id) cliArgs.push("--project", params.project_id);
      if (params.interlocutor) cliArgs.push("--interlocutor", params.interlocutor);
      if (params.activity) cliArgs.push("--activity", params.activity);
      if (params.by) cliArgs.push("--by", params.by);
      try {
        return toolSuccess(await runEnvelope(env, cliArgs));
      } catch (err) {
        return toolFailure(`dialog state write failed: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_skill_list",
    label: "List Viva skills",
    description: "List skills in this execution's member set.",
    parameters: Type.Object({}, { additionalProperties: false }),
    execute: async () => {
      try {
        return toolSuccess(await runEnvelope(env, ["skill", "list"]));
      } catch (err) {
        return toolFailure(`skill list failed: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_skill_search",
    label: "Search Viva skills",
    description:
      "Search this member's skill set. Set team to also search the team skill library.",
    parameters: Type.Object(
      {
        query: Type.String(),
        team: Type.Optional(Type.Boolean({ description: "Also search the team library." })),
      },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      const query = params.query.trim();
      if (!query) return toolFailure("skill search needs a non-empty query");
      const cliArgs = ["skill", "search", "--query", query];
      if (params.team) cliArgs.push("--team", "yes");
      try {
        return toolSuccess(await runEnvelope(env, cliArgs));
      } catch (err) {
        return toolFailure(`skill search failed: ${String(err)}`);
      }
    },
  });

  pi.registerTool({
    name: "viva_skill_get",
    label: "Read Viva skill",
    description: "Read one skill that is in this member's set.",
    parameters: Type.Object(
      { skill: Type.String({ description: "Skill id." }) },
      { additionalProperties: false },
    ),
    execute: async (_toolCallId, params) => {
      const skill = params.skill.trim();
      if (!skill) return toolFailure("skill get needs a skill id");
      try {
        return toolSuccess(await runEnvelope(env, ["skill", "get", "--skill", skill]));
      } catch (err) {
        return toolFailure(`skill get failed: ${String(err)}`);
      }
    },
  });
}
