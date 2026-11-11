import { invoke } from "@tauri-apps/api/core";
import type { CommandArgs, CommandName, CommandResult } from "./commands";

export const isTauri =
  typeof window !== "undefined" &&
  Boolean((window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);

/**
 * A command that failed. `code` is "error" unless the backend named the
 * failure (see `src-tauri/src/error.rs`). `String(error)` gives the message,
 * so code that only shows errors needs no changes.
 */
export class CommandError extends Error {
  readonly code: string;

  constructor(message: string, code = "error") {
    super(message);
    this.name = "CommandError";
    this.code = code;
  }

  override toString(): string {
    return this.message;
  }
}

/** Codes the backend uses for failures the frontend reacts to. */
export const ErrorCode = {
  fileChanged: "file_changed",
} as const;

/** Normalize what a rejected `invoke` carries: a string or `{ code, message }`. */
export function toCommandError(raw: unknown): CommandError {
  if (raw instanceof CommandError) return raw;
  if (typeof raw === "string") return new CommandError(raw);
  if (raw && typeof raw === "object" && typeof (raw as { message?: unknown }).message === "string") {
    const { message, code } = raw as { message: string; code?: unknown };
    return new CommandError(message, typeof code === "string" ? code : "error");
  }
  return new CommandError(String(raw));
}

export function hasErrorCode(error: unknown, code: string): boolean {
  return error instanceof CommandError && error.code === code;
}

/**
 * Call a backend command. Its name decides the argument and result types
 * (see `./commands`); commands without arguments take none.
 */
export async function command<K extends CommandName>(
  name: K,
  ...args: CommandArgs<K> extends Record<string, never> ? [] : [CommandArgs<K>]
): Promise<CommandResult<K>> {
  try {
    return await invoke<CommandResult<K>>(name, args[0]);
  } catch (raw) {
    throw toCommandError(raw);
  }
}
