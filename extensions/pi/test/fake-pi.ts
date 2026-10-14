/**
 * A fake Pi for unit tests, typed against the REAL Pi SDK (issue #101).
 *
 * The previous per-file fakes were hand-written to match a hand-written
 * `pi-api.d.ts`, and both disagreed with the real Pi: the fakes accepted
 * `registerTool(name, tool)` and an `execute(args)` that returned a bare
 * string, while the real Pi takes one tool object and an
 * `execute(toolCallId, params, signal, onUpdate, ctx)` that returns
 * `{ content: [{ type: "text", text }], details }`.
 *
 * This fake implements only `on` and `registerTool`, with the signatures of
 * the real `ExtensionAPI` (`PiHost`, see `lib.ts`), and tools are called
 * through the real `ToolDefinition.execute` signature. If either drifts from
 * the real SDK, `npm run typecheck` fails; nothing here is cast to `never`.
 *
 * It is still a fake: it proves the extension's own logic. That real Pi
 * accepts the extension is checked by `real-pi-load.test.ts`.
 */

import assert from "node:assert/strict";

import type {
  AgentToolResult,
  ExtensionToolContext,
  ToolDefinition,
} from "@earendil-works/pi-coding-agent";

import type { PiHost } from "../lib.ts";

/** A registry holds tools of every parameter shape, hence `any`. */
export type RegisteredTool = ToolDefinition<any, any, any>;

/** Any Pi event handler: assignable from every `ExtensionHandler<E>`. */
export type Handler = (event: never, ctx: never) => unknown;

export interface FakePi {
  /** Pass this to an extension factory. */
  host: PiHost;
  /** Tools registered so far, by name. */
  tools: Map<string, RegisteredTool>;
  /** Event handlers registered so far, by event name. */
  handlers: Map<string, Handler[]>;
}

export function createFakePi(): FakePi {
  const tools = new Map<string, RegisteredTool>();
  const handlers = new Map<string, Handler[]>();
  const host: PiHost = {
    registerTool(tool) {
      assert.ok(!tools.has(tool.name), `tool ${tool.name} registered twice`);
      tools.set(tool.name, tool);
    },
    on(event: string, handler: Handler) {
      handlers.set(event, [...(handlers.get(event) ?? []), handler]);
      return () => {};
    },
  };
  return { host, tools, handlers };
}

/**
 * Call a registered tool the way Pi does. `ctx` is the tool context Pi
 * passes; most tools ignore it, so the default is an empty object.
 */
export function callTool(
  tool: RegisteredTool,
  params: Record<string, unknown>,
  ctx: unknown = {},
): Promise<AgentToolResult<unknown>> {
  return tool.execute("test-call-id", params, undefined, undefined, ctx as ExtensionToolContext);
}

/** Fire an event at every handler registered for it, as Pi would. */
export async function emit(fake: FakePi, event: string, payload: unknown, ctx: unknown): Promise<void> {
  const registered = fake.handlers.get(event) ?? [];
  assert.ok(registered.length > 0, `no handler registered for ${event}`);
  for (const handler of registered) {
    await (handler as (e: unknown, c: unknown) => unknown)(payload, ctx);
  }
}

/** The text the model would read: the single text block of a tool result. */
export function textOf(result: AgentToolResult<unknown>): string {
  assert.equal(result.content.length, 1, "a Viva tool result carries exactly one content block");
  const block = result.content[0];
  assert.ok(block && block.type === "text", "the content block is text");
  return block.text;
}

/** Look a registered tool up, failing with the names that do exist. */
export function toolNamed(fake: FakePi, name: string): RegisteredTool {
  const tool = fake.tools.get(name);
  assert.ok(tool, `tool ${name} was not registered; registered: ${[...fake.tools.keys()].join(", ")}`);
  return tool;
}
