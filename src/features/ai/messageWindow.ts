/**
 * Long conversations render only their latest blocks; older ones load a page
 * at a time when the user scrolls up. A block is one message, or one run of
 * consecutive tool calls. `hidden` counts the blocks left out at the top.
 */
export const PAGE = 40;

/** What a conversation shows when it opens: the latest page. */
export function initialHidden(total: number): number {
  return Math.max(0, total - PAGE);
}

/** One more page above what is shown. */
export function showEarlier(hidden: number): number {
  return Math.max(0, hidden - PAGE);
}

/**
 * Keeps at least a page visible after the conversation shrank (a message was
 * edited and run again). New blocks at the end never hide older ones.
 */
export function clampHidden(hidden: number, total: number): number {
  return Math.min(hidden, initialHidden(total));
}

/**
 * While the user follows the newest output, a long-running task would keep
 * adding blocks; once twice a page is shown, fold back to the latest page.
 */
export function trimWhileFollowing(hidden: number, total: number): number {
  return total - hidden > PAGE * 2 ? initialHidden(total) : hidden;
}
