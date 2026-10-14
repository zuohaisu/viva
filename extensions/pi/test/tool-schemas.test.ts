/**
 * Parameter schemas of every Viva tool (issue #101).
 *
 * Real Pi takes a TypeBox schema per tool and validates the model's call
 * against it before `execute` runs. The schemas used to be hand-written
 * JSON-schema-like objects that nothing checked. This pins, for all 10 tools,
 * the contract the model sees: which fields are required, and that unknown
 * fields are rejected. Validation uses TypeBox itself (the library Pi
 * validates with), so a schema that only looks right does not pass.
 *
 * `real-pi-load.test.ts` separately proves real Pi accepts each schema.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { Value } from "typebox/value";

import vivaConversations from "../conversations/index.ts";
import viva from "../viva.ts";
import { createFakePi, type RegisteredTool } from "./fake-pi.ts";

const OFFICE_ENV: NodeJS.ProcessEnv = {
  VIVA_MEMBER_ID: "mem-1",
  VIVA_MEMBER_NAME: "Samuel",
  VIVA_HOME: "/tmp/viva-home",
};

/** Every tool and its required parameters (the contract the model sees). */
const REQUIRED: Record<string, string[]> = {
  viva_status: [],
  viva_task_brief: [],
  viva_dispatch: ["argv", "cwd", "request_key"],
  viva_handoff: ["summary"],
  viva_computer_audit: [],
  viva_memory_search: ["query"],
  viva_memory_remember: ["content"],
  viva_conversations_fork: ["display_name", "parent_node_id"],
  viva_conversations_rename: ["node_id", "display_name"],
  viva_conversations_handoff: ["node_id", "to_harness"],
};

/** A call that satisfies each tool's required fields. */
const VALID_CALL: Record<string, Record<string, unknown>> = {
  viva_status: {},
  viva_task_brief: { task_id: "task-1" },
  viva_dispatch: { argv: ["/bin/true"], cwd: "/tmp", request_key: "rk-1", grant_id: "g-1" },
  viva_handoff: { summary: "done", task_id: "task-1" },
  viva_computer_audit: {},
  viva_memory_search: { query: "q", project_id: "p-1" },
  viva_memory_remember: { content: "c", source: "s", project_id: "p-1" },
  viva_conversations_fork: { display_name: "n", parent_node_id: "node-1" },
  viva_conversations_rename: { node_id: "node-1", display_name: "n" },
  viva_conversations_handoff: {
    node_id: "node-1",
    to_harness: "codex",
    capability: "native_fork",
    brief: "b",
    task_id: "task-1",
  },
};

function allTools(): Map<string, RegisteredTool> {
  const saved = process.env;
  process.env = { ...OFFICE_ENV };
  try {
    const fake = createFakePi();
    viva(fake.host);
    vivaConversations(fake.host);
    return fake.tools;
  } finally {
    process.env = saved;
  }
}

test("all 10 tools are registered, each with a label and description", () => {
  const tools = allTools();
  assert.deepEqual([...tools.keys()].sort(), Object.keys(REQUIRED).sort());
  for (const [name, tool] of tools) {
    assert.ok(tool.label.trim(), `${name} needs a human-readable label`);
    assert.ok(tool.description.trim(), `${name} needs a description for the model`);
  }
});

test("every tool declares an object schema with its required fields and no extras", () => {
  for (const [name, tool] of allTools()) {
    const schema = tool.parameters as {
      type?: unknown;
      required?: string[];
      additionalProperties?: unknown;
    };
    assert.equal(schema.type, "object", `${name}: parameters must be an object schema`);
    assert.equal(schema.additionalProperties, false, `${name}: unknown fields must be rejected`);
    assert.deepEqual([...(schema.required ?? [])].sort(), [...REQUIRED[name]!].sort(), `${name}: required fields`);
  }
});

test("TypeBox accepts a complete call and rejects missing, extra and mistyped fields", () => {
  for (const [name, tool] of allTools()) {
    const valid = VALID_CALL[name]!;
    assert.equal(Value.Check(tool.parameters, valid), true, `${name}: a complete call is accepted`);
    assert.equal(
      Value.Check(tool.parameters, { ...valid, not_a_field: 1 }),
      false,
      `${name}: an unknown field is rejected`,
    );
    for (const field of REQUIRED[name]!) {
      const { [field]: _omitted, ...without } = valid;
      assert.equal(Value.Check(tool.parameters, without), false, `${name}: missing ${field} is rejected`);
    }
  }
});

test("typed fields reject the wrong type", () => {
  const tools = allTools();
  const dispatch = tools.get("viva_dispatch")!;
  assert.equal(
    Value.Check(dispatch.parameters, { ...VALID_CALL.viva_dispatch, argv: "/bin/true" }),
    false,
    "argv must be an array, never a joined shell string",
  );
  const handoff = tools.get("viva_conversations_handoff")!;
  assert.equal(
    Value.Check(handoff.parameters, { ...VALID_CALL.viva_conversations_handoff, capability: "teleport" }),
    false,
    "capability is limited to the declared values",
  );
});
