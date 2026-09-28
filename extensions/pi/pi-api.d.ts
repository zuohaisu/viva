/**
 * Minimal vendored types for the subset of the Pi coding-agent extension
 * API this package uses.
 *
 * Honest scope note: these declarations are pinned to the audited upstream
 * extension documentation (packages/coding-agent/docs/extensions.md,
 * reviewed 2026-09-28) and cover only what `viva-office.ts` touches. They
 * are NOT the full upstream SDK types. When this package gains the real
 * `@earendil-works/pi-coding-agent` dependency, delete this file, import
 * from the SDK and re-run `npm run typecheck` — the extension code should
 * not need to change.
 */

export interface ExtensionUI {
  notify(message: string, level?: "info" | "warn" | "error"): void;
}

export interface ExtensionContext {
  /** The working directory Pi is running in. */
  cwd?: string;
  ui: ExtensionUI;
  hasUI: boolean;
  mode?: "tui" | "headless" | string;
}

export interface SessionStartEvent {
  /** Present when Pi resumed an existing session. */
  resumed?: boolean;
}

export type ExtensionHandler<E> = (event: E, ctx: ExtensionContext) => void | Promise<void>;

export interface RegisteredTool {
  description: string;
  /** JSON-schema-ish parameter description; Pi renders it for the model. */
  parameters?: Record<string, unknown>;
  execute(
    args: Record<string, unknown>,
    ctx?: ExtensionContext,
  ): Promise<{ content: string; isError?: boolean }> | { content: string; isError?: boolean };
}

export interface ExtensionAPI {
  on<E>(
    event: "session_start" | "session_shutdown",
    handler: ExtensionHandler<E>,
  ): () => void;
  on(event: string, handler: (event: never, ctx: ExtensionContext) => void | Promise<void>): () => void;
  registerTool(name: string, tool: RegisteredTool): void;
  registerCommand(
    name: string,
    command: {
      description?: string;
      handler(args: string, ctx: ExtensionContext): void | Promise<void>;
    },
  ): void;
}
