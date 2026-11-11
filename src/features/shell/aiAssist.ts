/** The terminal's AI shortcuts. Pure helpers, free of imports for the tests. */

interface KeyLike {
  type: string;
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

/** ⌘ on macOS, Ctrl elsewhere. */
function primary(event: KeyLike, mac: boolean) {
  return mac ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
}

/** ⌘⇧A / Ctrl+Shift+A: ask the AI about the selected output. */
export function isAskAiShortcut(event: KeyLike, mac: boolean): boolean {
  return (
    event.type === "keydown" &&
    primary(event, mac) &&
    event.shiftKey &&
    !event.altKey &&
    event.key.toLowerCase() === "a"
  );
}

/** ⌘↩ / Ctrl+Enter on a `# …` line: turn the description into a command. */
export function isGenerateCommandShortcut(event: KeyLike, mac: boolean): boolean {
  return (
    event.type === "keydown" &&
    primary(event, mac) &&
    !event.shiftKey &&
    !event.altKey &&
    event.key === "Enter"
  );
}

/** The description typed after `#` at the prompt, or null for anything else. */
export function naturalLanguageRequest(line: string): string | null {
  const match = /^\s*#\s*(.+?)\s*$/.exec(line);
  return match && match[1] ? match[1] : null;
}

/**
 * The description on the cursor's line as the terminal shows it, prompt
 * included: a `#` comment typed right after the prompt (`$ # …`, or
 * `~# # …` as root). A `#` inside a command is not a request.
 */
export function requestFromPromptLine(line: string): string | null {
  const match = /(?:^|[$#%>]\s+)#\s*(\S.*?)\s*$/.exec(line);
  return match ? match[1] : null;
}

/**
 * One command line from the model's answer: code fences and prompt marks
 * removed, the first meaningful line kept. Nothing that could submit the
 * line (no newline, no control characters) survives.
 */
export function cleanSuggestedCommand(answer: string): string {
  const lines = answer
    .replace(/```[a-zA-Z0-9_-]*/g, "\n")
    .split(/\r\n|\r|\n/)
    .map((line) => line.replace(/^\s*\$\s+/, "").trim())
    .filter(Boolean);
  const line = lines[0] ?? "";
  // eslint-disable-next-line no-control-regex
  return line.replace(/[\u0000-\u001f\u007f]/g, " ").trim().slice(0, 2000);
}

/** The draft placed in the AI panel for a selection of terminal output. */
export function selectionPrompt(selection: string): string {
  const text = selection.replace(/\s+$/, "").slice(-12_000);
  const fence = text.includes("```") ? "~~~" : "```";
  return `解释下面这段终端输出，指出问题原因并给出下一步建议：\n${fence}\n${text}\n${fence}`;
}
