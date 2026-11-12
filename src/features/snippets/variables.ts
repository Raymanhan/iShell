/**
 * Snippet variables: `{{name}}` or `{{name:default}}` in a snippet's command,
 * filled in when it is sent. Pure helpers; they import only i18n, so the
 * tests can load them.
 */
import { t } from "../../i18n";

export interface SnippetVariable {
  name: string;
  defaultValue: string;
}

// A name is letters (any script), digits, `_`, `-` or `.`; a default runs to `}}`.
const PATTERN = /\{\{\s*([\p{L}\p{N}_.-]+)\s*(?::([^}\n]*))?\}\}/gu;

/** The variables a command uses, each once, in order of first use. */
export function snippetVariables(command: string): SnippetVariable[] {
  const variables: SnippetVariable[] = [];
  for (const match of command.matchAll(PATTERN)) {
    const name = match[1];
    const existing = variables.find((variable) => variable.name === name);
    if (!existing) variables.push({ name, defaultValue: match[2] ?? "" });
    else if (!existing.defaultValue && match[2]) existing.defaultValue = match[2];
  }
  return variables;
}

/**
 * The command with every variable replaced. Values are inserted as typed;
 * a line break would make the terminal run an extra line, so it is refused.
 */
export function fillSnippet(command: string, values: Record<string, string>): string {
  for (const [name, value] of Object.entries(values)) {
    if (/[\r\n]/.test(value)) throw new Error(t("变量 {0} 不能包含换行", name));
  }
  return command.replace(PATTERN, (_, name: string, fallback?: string) => values[name] ?? fallback ?? "");
}
