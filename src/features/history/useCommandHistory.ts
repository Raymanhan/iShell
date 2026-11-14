import { useCallback, useEffect, useRef, useState } from "react";
import { command, isTauri } from "../../api/tauri";
import { COMMAND_HISTORY_LIMIT, withCommand } from "./history";

/** Browser preview only: history per server in localStorage. */
const DEMO_KEY = "ishell.commandHistory.byServer";
/** The preview's history from before it was kept per server. */
const LEGACY_DEMO_KEY = "ishell.commandHistory";
const EMPTY: readonly string[] = Object.freeze([]);

function readStrings(raw: string | null): string[] {
  try {
    const parsed = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter((item) => typeof item === "string") : [];
  } catch {
    return [];
  }
}

function readDemo(): Record<string, string[]> {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(DEMO_KEY) || "{}");
    if (!parsed || typeof parsed !== "object") return {};
    return Object.fromEntries(
      Object.entries(parsed).map(([id, list]) => [id, readStrings(JSON.stringify(list))]),
    );
  } catch {
    return {};
  }
}

/**
 * Commands typed in terminals, kept per server: a server's suggestions and
 * history panel show what was typed on it. Commands from before history was
 * per server appear for every server. Each server's list loads the first
 * time one of its terminals is open.
 */
export function useCommandHistory(serverIds: string[], onError: (message: string) => void) {
  const [byServer, setByServer] = useState<Record<string, string[]>>({});
  const requested = useRef(new Set<string>());
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  const key = [...new Set(serverIds)].sort().join("\n");
  useEffect(() => {
    for (const serverId of new Set(serverIds)) {
      if (!serverId || requested.current.has(serverId)) continue;
      requested.current.add(serverId);
      if (!isTauri) {
        const saved = readDemo()[serverId];
        const legacy = readStrings(window.localStorage.getItem(LEGACY_DEMO_KEY));
        setByServer((current) => ({ ...current, [serverId]: [...(saved ?? []), ...legacy] }));
        continue;
      }
      command("list_command_history", { serverId })
        .then((list) =>
          setByServer((current) => ({
            ...current,
            // Commands typed while the list was loading stay in front.
            [serverId]: [...(current[serverId] ?? []), ...list].slice(0, COMMAND_HISTORY_LIMIT),
          })),
        )
        .catch((error) => {
          requested.current.delete(serverId);
          onErrorRef.current(String(error));
        });
    }
    // `key` stands for the set of server ids.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  const record = useCallback((serverId: string, commandText: string) => {
    const trimmed = commandText.trim();
    if (!serverId || !trimmed) return;
    setByServer((current) => {
      const next = { ...current, [serverId]: withCommand(current[serverId] ?? EMPTY, trimmed) };
      if (!isTauri) {
        const demo = readDemo();
        demo[serverId] = withCommand(demo[serverId] ?? EMPTY, trimmed);
        window.localStorage.setItem(DEMO_KEY, JSON.stringify(demo));
      }
      return next;
    });
    if (isTauri) {
      command("save_command_history", { serverId, commandText: trimmed }).catch((error) =>
        onErrorRef.current(String(error)),
      );
    }
  }, []);

  /** The same array until the server's history changes. */
  const forServer = useCallback(
    (serverId: string | undefined): readonly string[] =>
      (serverId && byServer[serverId]) || EMPTY,
    [byServer],
  );

  return { forServer, record };
}
