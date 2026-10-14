/**
 * Computer operations extension module for the Pi coding agent
 * (F03, issue #25).
 *
 * What this module does — and deliberately does not:
 * - Registers ONE read-only tool: auditing the reused computer tools
 *   (`orca computer`, `osascript`) for what is really present and
 *   permissioned on this host. The audit runs through the viva CLI and
 *   records evidence rows there.
 * - It does NOT expose keyboard/mouse/focus input from chat. Input is a
 *   world-mutating action: it exists only on Viva's task-scoped
 *   grant path (`viva tools computer smoke`, or the library engine with a
 *   live `computer_input` grant). Chat gets an honest refusal plus the
 *   pointer to that path — never a "please authorize me" shortcut.
 * - Sessions without Viva context stay inert (the caller checks the
 *   Viva environment before registering).
 */

import { Type } from "typebox";
import { runEnvelope, toolFailure, toolSuccess, type OfficeEnv, type PiHost } from "../lib.ts";

export function registerComputerTools(pi: PiHost, env: OfficeEnv): void {
  pi.registerTool({
    name: "viva_computer_audit",
    label: "Viva computer audit",
    description:
      "Audit which reused computer tools (orca computer, osascript) are present and " +
      "permissioned on this host. Read-only; evidence is recorded in the store.",
    parameters: Type.Object({}, { additionalProperties: false }),
    execute: async () => {
      try {
        return toolSuccess(await runEnvelope(env, ["tools", "computer", "audit"]));
      } catch (err) {
        return toolFailure(`computer audit unavailable: ${String(err)}`);
      }
    },
  });
}

/// The honest answer to "drive my computer" from chat: input actions are
/// not a chat tool. Exported so the entry file and tests share one text.
export const COMPUTER_INPUT_REFUSAL =
  "Computer input (click/type/keys) is not available from chat. It runs only on the " +
  "Viva's task-scoped grant path: the owner issues a `computer_input` grant for a " +
  "specific task, then actions execute through the engine (locate → act → " +
  "verify, with recorded evidence). Use viva_computer_audit to see what this host offers.";
