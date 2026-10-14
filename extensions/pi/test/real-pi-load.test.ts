/**
 * Real-Pi load smoke test (issue #101).
 *
 * Why this exists: every other test in this package drives the extension
 * with a fake Pi that this repo wrote. That fake accepted
 * `registerTool(name, tool)` for months while the real Pi rejects it, so the
 * suite stayed green while the extension failed to load in a real session.
 * A fake cannot catch a mismatch with the thing it fakes; only the real Pi
 * can.
 *
 * How it works: the extension entry files are loaded by the REAL Pi extension
 * loader (the same `DefaultResourceLoader` code path `pi --extension <file>`
 * uses), pinned by `package-lock.json`. The test then reads what real Pi
 * recorded: the load errors and the registered tool names.
 *
 * Deliberately NOT `pi -p ...`: without a model API key that command exits 1
 * whether or not the extension loaded, so its exit code cannot tell a good
 * load from a bad one. This test needs no network and no credentials.
 *
 * Guards against a false green:
 * - The harness-injected office variables are set. Without them the
 *   extension stays inert by design, loads trivially, and would pass while
 *   registering nothing; a control test below pins that behaviour.
 * - Tool names are compared for exact equality, not "at least one tool".
 * - A Pi package that cannot be imported fails this file; nothing here skips.
 * - Pi runs offline with its agent directory and HOME in a throwaway
 *   directory, so the developer's own ~/.pi configuration is never read.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  createAgentSession,
  DefaultResourceLoader,
  SessionManager,
  VERSION,
} from "@earendil-works/pi-coding-agent";

const EXTENSION_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** The entry the Rust harness really passes to `pi --extension` (harness/pi/mod.rs). */
const HARNESS_ENTRY = join(EXTENSION_ROOT, "viva-office.ts");
/** Not loaded by the harness today; checked so it cannot rot unnoticed. */
const CONVERSATIONS_ENTRY = join(EXTENSION_ROOT, "conversations", "index.ts");

const HARNESS_ENTRY_TOOLS = [
  "viva_computer_audit",
  "viva_memory_remember",
  "viva_memory_search",
  "viva_office_dispatch",
  "viva_office_handoff",
  "viva_office_status",
  "viva_office_task_brief",
];

const CONVERSATIONS_TOOLS = [
  "viva_conversations_fork",
  "viva_conversations_handoff",
  "viva_conversations_rename",
];

interface RealPiLoad {
  /** Errors the real Pi extension loader reported for the entry. */
  errors: string[];
  /** Tool names the loaded extension registered with real Pi. */
  registeredTools: string[];
  /** Tools of a real Pi session built on that loader (built-ins excluded). */
  sessionTools: { name: string; parameterType: unknown }[];
}

/** The environment a Viva-launched Pi session has (harness/pi/mod.rs contract). */
function officeEnvironment(home: string): Record<string, string> {
  return {
    VIVA_HOME: home,
    VIVA_OFFICE_MEMBER_ID: "mem-00000000000000000000000000000001",
    VIVA_OFFICE_MEMBER_NAME: "Smoke Member",
    VIVA_OFFICE_TASK_ID: "task-00000000000000000000000000000001",
    VIVA_OFFICE_GRANT_ID: "grant-00000000000000000000000000000001",
  };
}

/**
 * Load one extension entry in real Pi and report what Pi recorded.
 * `office` is the harness-injected environment, or null for a bare `pi` run.
 */
async function loadInRealPi(entry: string, office: boolean): Promise<RealPiLoad> {
  const sandbox = mkdtempSync(join(tmpdir(), "viva-real-pi-"));
  const agentDir = join(sandbox, "agent");
  const cwd = join(sandbox, "cwd");
  const vivaHome = join(sandbox, "viva-home");
  for (const dir of [agentDir, cwd, vivaHome]) mkdirSync(dir, { recursive: true });

  // Nothing from the surrounding shell (VIVA_*, PI_*) may leak in.
  const env: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (!key.startsWith("VIVA_") && !key.startsWith("PI_")) env[key] = value;
  }
  Object.assign(env, {
    HOME: sandbox,
    PI_OFFLINE: "1",
    PI_CODING_AGENT_DIR: agentDir,
    ...(office ? officeEnvironment(vivaHome) : {}),
  });

  const saved = process.env;
  process.env = env;
  try {
    // `noExtensions` turns off discovery (~/.pi, project .pi); only the entry
    // under test is loaded, through the same loader `pi --extension` uses.
    const loader = new DefaultResourceLoader({
      cwd,
      agentDir,
      additionalExtensionPaths: [entry],
      noExtensions: true,
      noSkills: true,
      noPromptTemplates: true,
      noThemes: true,
      noContextFiles: true,
    });
    await loader.reload();
    const loaded = loader.getExtensions();

    const { session } = await createAgentSession({
      cwd,
      agentDir,
      resourceLoader: loader,
      sessionManager: SessionManager.inMemory(cwd),
    });
    try {
      const registeredTools = loaded.extensions
        .flatMap((extension) => [...extension.tools.keys()])
        .sort();
      const registered = new Set(registeredTools);
      return {
        errors: loaded.errors.map((e) => `${e.path}: ${e.error}`),
        registeredTools,
        sessionTools: session
          .getAllTools()
          .filter((tool) => registered.has(tool.name))
          .map((tool) => ({
            name: tool.name,
            parameterType: (tool.parameters as { type?: unknown }).type,
          }))
          .sort((a, b) => a.name.localeCompare(b.name)),
      };
    } finally {
      session.dispose();
    }
  } finally {
    process.env = saved;
    rmSync(sandbox, { recursive: true, force: true });
  }
}

function assertLoadsCleanlyWithTools(load: RealPiLoad, expected: string[], what: string): void {
  assert.deepEqual(
    load.errors,
    [],
    `${what}: real Pi ${VERSION} failed to load the extension:\n${load.errors.join("\n")}`,
  );
  assert.deepEqual(load.registeredTools, expected, `${what}: tools registered with real Pi`);
  assert.deepEqual(
    load.sessionTools.map((tool) => tool.name),
    expected,
    `${what}: tools visible in a real Pi session`,
  );
  for (const tool of load.sessionTools) {
    assert.equal(
      tool.parameterType,
      "object",
      `${what}: ${tool.name} must declare an object parameter schema`,
    );
  }
}

test("control: a bare Pi run loads the extension inertly (why the office env is injected)", async (t) => {
  t.diagnostic(`real Pi ${VERSION}`);
  const load = await loadInRealPi(HARNESS_ENTRY, false);
  assert.deepEqual(load.errors, [], "an inert extension must load without errors");
  assert.deepEqual(
    load.registeredTools,
    [],
    "without the office environment the extension registers nothing, so a load " +
      "check without it would pass vacuously",
  );
});

test("real Pi loads the harness entry (viva-office.ts) and registers its 7 tools", async (t) => {
  t.diagnostic(`real Pi ${VERSION}`);
  const load = await loadInRealPi(HARNESS_ENTRY, true);
  assertLoadsCleanlyWithTools(load, HARNESS_ENTRY_TOOLS, "viva-office.ts");
});

test("real Pi loads the conversations sub-extension and registers its 3 tools", async (t) => {
  t.diagnostic(`real Pi ${VERSION}`);
  const load = await loadInRealPi(CONVERSATIONS_ENTRY, true);
  assertLoadsCleanlyWithTools(load, CONVERSATIONS_TOOLS, "conversations/index.ts");
});
