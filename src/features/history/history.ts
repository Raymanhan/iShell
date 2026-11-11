/** Pure helpers for command history, kept free of imports for the tests. */

export const COMMAND_HISTORY_LIMIT = 10_000;

/** A server's history with `commandText` typed last. */
export function withCommand(history: readonly string[], commandText: string): string[] {
  return [commandText, ...history].slice(0, COMMAND_HISTORY_LIMIT);
}
