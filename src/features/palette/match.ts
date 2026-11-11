/**
 * Matching for the command palette. Pure helpers, free of imports for the
 * tests.
 */

export interface PaletteEntry {
  id: string;
  title: string;
  /** Extra text that matches but is shown smaller (an address, say). */
  detail?: string;
  /** Words that match without being shown (`ssh`, `connect`, …). */
  keywords?: string;
}

/**
 * How well one search term matches `text`, or null for no match: a run of
 * the term's characters, in order, possibly with gaps. Whole substrings beat
 * scattered letters, and earlier and word-start matches beat later ones.
 */
export function termScore(term: string, text: string): number | null {
  if (!term) return 0;
  const haystack = text.toLowerCase();
  const needle = term.toLowerCase();
  const at = haystack.indexOf(needle);
  if (at >= 0) {
    const wordStart = at === 0 || /[\s@:._/\-（(]/.test(haystack[at - 1]);
    return 1000 - Math.min(at, 200) + (wordStart ? 300 : 0) + (at === 0 ? 200 : 0);
  }
  let score = 0;
  let from = 0;
  let previous = -2;
  for (const ch of needle) {
    const index = haystack.indexOf(ch, from);
    if (index < 0) return null;
    score += index === previous + 1 ? 8 : 1;
    previous = index;
    from = index + 1;
  }
  return score;
}

/** The entry's score for a query of space-separated terms, or null. */
export function entryScore(entry: PaletteEntry, query: string): number | null {
  const terms = query.trim().split(/\s+/).filter(Boolean);
  if (!terms.length) return 0;
  let total = 0;
  for (const term of terms) {
    const title = termScore(term, entry.title);
    const rest = termScore(term, `${entry.detail ?? ""} ${entry.keywords ?? ""}`);
    // Matches in the title count for more than those in the details.
    const best = Math.max(title === null ? -1 : title * 2, rest === null ? -1 : rest);
    if (best < 0) return null;
    total += best;
  }
  return total;
}

/** The entries that match, best first; ties keep their order. */
export function rankEntries<T extends PaletteEntry>(entries: T[], query: string, limit = 60): T[] {
  if (!query.trim()) return entries.slice(0, limit);
  return entries
    .map((entry, index) => ({ entry, index, score: entryScore(entry, query) }))
    .filter((item): item is { entry: T; index: number; score: number } => item.score !== null)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .slice(0, limit)
    .map((item) => item.entry);
}

interface KeyLike {
  type: string;
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

/**
 * ⌘K or ⌘⇧P on macOS; Ctrl+Shift+P elsewhere, since Ctrl+K belongs to the
 * shell (it cuts to the end of the line).
 */
export function isPaletteShortcut(event: KeyLike, mac: boolean): boolean {
  if (event.type !== "keydown" || event.altKey) return false;
  const key = event.key.toLowerCase();
  if (mac) {
    if (!event.metaKey || event.ctrlKey) return false;
    return (key === "k" && !event.shiftKey) || (key === "p" && event.shiftKey);
  }
  return event.ctrlKey && !event.metaKey && event.shiftKey && key === "p";
}
