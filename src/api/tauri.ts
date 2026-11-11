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

/** Raw bytes for a terminal session, sent as they are (file transfers). */
export async function writeTerminalBytes(sessionId: string, bytes: Uint8Array): Promise<void> {
  try {
    await invoke("terminal_write", bytes, { headers: { "session-id": sessionId } });
  } catch (raw) {
    throw toCommandError(raw);
  }
}

/** Save a file received over ZMODEM in the downloads folder; returns its path. */
export async function saveReceivedFile(name: string, chunks: Uint8Array[]): Promise<string> {
  const size = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const body = new Uint8Array(size);
  let at = 0;
  for (const chunk of chunks) {
    body.set(chunk, at);
    at += chunk.length;
  }
  let binary = "";
  for (const byte of new TextEncoder().encode(name)) binary += String.fromCharCode(byte);
  try {
    return await invoke<string>("zmodem_save_file", body, { headers: { "file-name": btoa(binary) } });
  } catch (raw) {
    throw toCommandError(raw);
  }
}
