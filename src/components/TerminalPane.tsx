import { memo, useEffect, useMemo, useRef, useState } from "react";
import { Channel } from "@tauri-apps/api/core";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Terminal } from "@xterm/xterm";
import { RefreshCw, X } from "lucide-react";
import { command, isTauri, saveReceivedFile, writeTerminalBytes as sendRawBytes } from "../api/tauri";
import { ZmodemController, type LocalFile } from "../features/shell/zmodem";
import { getTerminalSurfaceReport, getTerminalTheme, getThemeDefinition, type AppTheme } from "../constants/theme";
import { boldWeightFor, type TerminalFontWeight } from "../features/settings/terminalFonts";
import { splitShortcutDirection, type PaneRect, type SplitDirection } from "../features/shell/panes";
import { directoryLinkProvider } from "../features/shell/directoryLinks";
import { createClipboardPasteHandler, imagePasteData, isTerminalPasteShortcut } from "../features/shell/clipboard";
import { isLocalServerId } from "../features/shell/local";
import {
  cleanSuggestedCommand,
  isAskAiShortcut,
  isGenerateCommandShortcut,
  naturalLanguageRequest,
  requestFromPromptLine,
} from "../features/shell/aiAssist";
import { enableUnicode11, enableWebglRenderer } from "../features/shell/renderer";
import type { ShellTab } from "../features/shell/types";
import { isTerminalSearchShortcut, TerminalSearchBar } from "./TerminalSearchBar";
import {
  FRAME_CLOSED,
  FRAME_DATA,
  FRAME_READY,
  parseFrame,
  TerminalOutput,
} from "../features/shell/terminalStream";

import { t, translateMessage } from "../i18n";
import { useLocale } from "../i18n/useLocale";
const passwordPromptPattern = /(?:password|passphrase|密码|口令)[^:\n\r]*[:：]\s*$/i;
const platformSource = `${navigator.userAgent} ${navigator.platform}`.toLowerCase();
const isWindowsPlatform = platformSource.includes("win");
const isMacPlatform = platformSource.includes("mac");
const HISTORY_SUGGESTION_LIMIT = 8;

interface HistorySuggestState {
  active: boolean;
  query: string;
  selectedIndex: number;
  x: number;
  y: number;
  maxHeight: number;
}

/** Where a pane sits when its tab is split; absent for a tab that fills the terminal area. */
export interface PanePlacement {
  rect: PaneRect;
  focused: boolean;
}

const closedHistorySuggest: HistorySuggestState = {
  active: false,
  query: "",
  selectedIndex: 0,
  x: 0,
  y: 0,
  maxHeight: 248,
};

function TerminalPaneBase({
  tab,
  visible,
  theme,
  fontSize,
  fontFamily,
  fontWeight,
  opaqueBackground,
  gpuRendering = true,
  layoutSignal,
  setNotice,
  onReady,
  onClosed,
  onReconnect,
  onCommandSubmitted,
  onOpenDirectory,
  onAskAi,
  onSuggestCommand,
  pasteRequest,
  commandRequest,
  commandHistory,
  pane,
  onFocusPane,
  onClosePane,
  onSplit,
  broadcasting = false,
  onBroadcastInput,
  inputRegistry,
}: {
  tab: ShellTab;
  visible: boolean;
  theme: AppTheme;
  fontSize: number;
  /** CSS font-family, with fallbacks. */
  fontFamily: string;
  fontWeight: TerminalFontWeight;
  /**
   * Draw on the theme's solid surface instead of a transparent background,
   * which lets glyphs use subpixel (ClearType) antialiasing.
   */
  opaqueBackground: boolean;
  /** Draw with WebGL; read when the terminal is created. */
  gpuRendering?: boolean;
  layoutSignal: string;
  setNotice: (notice: string) => void;
  onReady?: () => void;
  onClosed?: (reason: string) => void;
  onReconnect?: () => void;
  onCommandSubmitted?: (command: string) => void;
  onOpenDirectory?: (path: string) => void;
  /** ⌘⇧A / Ctrl+Shift+A with output selected: ask the AI about it. */
  onAskAi?: (selection: string) => void;
  /** ⌘↩ / Ctrl+Enter on a `# …` line: a command for the description. */
  onSuggestCommand?: (request: string) => Promise<string>;
  pasteRequest?: { id: number; command: string } | null;
  commandRequest?: { id: number; command: string; requirePromptStart?: boolean; blockedNotice?: string } | null;
  commandHistory: readonly string[];
  pane?: PanePlacement;
  onFocusPane?: () => void;
  onClosePane?: () => void;
  onSplit?: (direction: SplitDirection) => void;
  /** This session is a broadcast target: input typed here is mirrored to the other targets. */
  broadcasting?: boolean;
  onBroadcastInput?: (sourceTabId: string, data: string) => void;
  /** Where the pane registers a function that types into it, for broadcasts from other panes. */
  inputRegistry?: Map<string, (data: string) => void>;
}) {
  // Re-render on language changes (memoized or a window root).
  useLocale();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const themeRef = useRef(theme);
  const requestFitRef = useRef<(() => void) | null>(null);
  const sessionRef = useRef<string | null>(tab.sessionId);
  const closedSessionRef = useRef<string | null>(null);
  const readySessionRef = useRef<string | null>(null);
  // Decoded output per session; survives re-attaching, so nothing repeats.
  const outputsRef = useRef<Record<string, TerminalOutput>>({});
  // ZMODEM (rz/sz) per session, fed once the session's replayed output has
  // been shown, so a transfer in old output is not started again.
  const zmodemsRef = useRef<Record<string, ZmodemController>>({});
  const liveSessionsRef = useRef(new Set<string>());
  const encodingRef = useRef(tab.encoding ?? "UTF-8");
  encodingRef.current = tab.encoding ?? "UTF-8";
  const connectingNoticeRef = useRef(false);
  const fitTimerRef = useRef<number | null>(null);
  const fitSettledTimerRef = useRef<number | null>(null);
  const lastSizeRef = useRef("");
  const onReadyRef = useRef(onReady);
  const onClosedRef = useRef(onClosed);
  const onCommandSubmittedRef = useRef(onCommandSubmitted);
  const onOpenDirectoryRef = useRef(onOpenDirectory);
  const onAskAiRef = useRef(onAskAi);
  onAskAiRef.current = onAskAi;
  const onSuggestCommandRef = useRef(onSuggestCommand);
  onSuggestCommandRef.current = onSuggestCommand;
  const suggestingRef = useRef(false);
  onOpenDirectoryRef.current = onOpenDirectory;
  const onSplitRef = useRef(onSplit);
  onSplitRef.current = onSplit;
  const broadcastingRef = useRef(broadcasting);
  broadcastingRef.current = broadcasting;
  const onBroadcastInputRef = useRef(onBroadcastInput);
  onBroadcastInputRef.current = onBroadcastInput;
  const commandDraftRef = useRef("");
  const sensitiveInputRef = useRef(false);
  const commandHistoryRef = useRef(commandHistory);
  const historySuggestRef = useRef<HistorySuggestState>(closedHistorySuggest);
  const selectedSuggestionRef = useRef<HTMLButtonElement | null>(null);
  const [historySuggest, setHistorySuggest] = useState<HistorySuggestState>(closedHistorySuggest);
  const [searchAddon, setSearchAddon] = useState<SearchAddon | null>(null);
  // 0 = closed; otherwise bumped on every open shortcut so the bar refocuses.
  const [searchSignal, setSearchSignal] = useState(0);
  commandHistoryRef.current = commandHistory;

  const suggestedCommands = useMemo(
    () => getHistoryMatches(historySuggest.query),
    [commandHistory, historySuggest.query],
  );

  useEffect(() => {
    sessionRef.current = tab.sessionId;
    commandDraftRef.current = "";
    sensitiveInputRef.current = false;
    closeHistorySuggest();
    lastSizeRef.current = "";
    if (tab.sessionId && closedSessionRef.current !== tab.sessionId) {
      if (terminalRef.current) terminalRef.current.options.disableStdin = false;
    }
    if (tab.sessionId && closedSessionRef.current && closedSessionRef.current !== tab.sessionId) {
      closedSessionRef.current = null;
    }
    window.setTimeout(reportTerminalSize, 0);
  }, [tab.sessionId]);

  function markReady(sessionId: string) {
    if (readySessionRef.current === sessionId) return;
    readySessionRef.current = sessionId;
    onReadyRef.current?.();
  }

  function writeTerminalBytes(sessionId: string, offset: number, bytes: Uint8Array) {
    const output = (outputsRef.current[sessionId] ??= new TerminalOutput(encodingRef.current));
    const fresh = output.take(offset, bytes);
    if (!fresh.length) return;
    if (liveSessionsRef.current.has(sessionId)) zmodemFor(sessionId).consume(fresh);
    else showOutput(sessionId, fresh);
  }

  /** Decode and show ordinary output. */
  function showOutput(sessionId: string, bytes: Uint8Array) {
    const text = outputsRef.current[sessionId]?.decode(bytes);
    if (!text) return;
    terminalRef.current?.write(text);
    trackSensitivePrompt(text);
  }

  function zmodemFor(sessionId: string): ZmodemController {
    let picked: string[] = [];
    return (zmodemsRef.current[sessionId] ??= new ZmodemController({
      write: (bytes) => sendRawBytes(sessionId, bytes),
      output: (bytes) => showOutput(sessionId, bytes),
      status: (text) => terminalRef.current?.write(`\r\n${text}`),
      pickFiles: async () => {
        const files = await command("zmodem_pick_files");
        picked = files.map((file) => file.id);
        return files.map(
          (file): LocalFile => ({
            name: file.name,
            size: file.size,
            mtime: file.mtime,
            read: async (offset, length) =>
              new Uint8Array(await command("zmodem_read_file", { id: file.id, offset, length })),
          }),
        );
      },
      saveFile: (name, chunks) => saveReceivedFile(name, chunks),
      onActiveChange: (active) => {
        if (active) {
          terminalRef.current?.write(`\r\n[iShell] ${t("ZMODEM 传输开始，按 Ctrl+C 取消")}`);
          return;
        }
        terminalRef.current?.write("\r\n");
        if (picked.length) void command("zmodem_forget_files", { ids: picked }).catch(() => undefined);
        picked = [];
      },
    }));
  }

  /** Decode the session's further output with `encoding`. */
  function switchDecoder(sessionId: string, encoding: string) {
    const output = outputsRef.current[sessionId];
    if (!output) return;
    const rest = output.setEncoding(encoding);
    if (rest) terminalRef.current?.write(rest);
  }

  useEffect(() => {
    if (tab.sessionId) switchDecoder(tab.sessionId, tab.encoding ?? "UTF-8");
  }, [tab.encoding, tab.sessionId]);

  function showClosed(sessionId: string, reason: string) {
    if (closedSessionRef.current === sessionId) return;
    closedSessionRef.current = sessionId;
    if (terminalRef.current) terminalRef.current.options.disableStdin = true;
    terminalRef.current?.writeln(`\r\n[iShell] ${reason}`);
    setNotice(reason);
    onClosedRef.current?.(reason);
  }

  function reportTerminalSize() {
    const terminal = terminalRef.current;
    const sessionId = sessionRef.current;
    if (!isTauri || !terminal || !sessionId) return;
    const cols = terminal.cols;
    const rows = terminal.rows;
    if (!cols || !rows) return;
    const nextSize = `${sessionId}:${cols}:${rows}`;
    if (lastSizeRef.current === nextSize) return;
    lastSizeRef.current = nextSize;
    command("terminal_resize", { sessionId, cols, rows }).catch((error) => setNotice(String(error)));
  }

  function updateCommandDraft(data: string) {
    const text = data.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "");
    let draft = commandDraftRef.current;
    for (const character of Array.from(text)) {
      if (character === "\r" || character === "\n") {
        if (!sensitiveInputRef.current) {
          const commandText = readVisibleCommand() || draft.trim();
          if (commandText) onCommandSubmittedRef.current?.(commandText);
        }
        sensitiveInputRef.current = false;
        draft = "";
      } else if (character === "\x03" || character === "\x15") {
        sensitiveInputRef.current = false;
        draft = "";
      } else if (character === "\x7f" || character === "\b") {
        draft = Array.from(draft).slice(0, -1).join("");
      } else if (character >= " ") {
        draft += character;
      }
    }
    commandDraftRef.current = draft;
  }

  function trackSensitivePrompt(data: string) {
    const plain = data.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "");
    const tail = plain.split(/\r?\n/).pop()?.trimEnd() ?? "";
    if (passwordPromptPattern.test(tail)) {
      sensitiveInputRef.current = true;
      commandDraftRef.current = "";
    }
  }

  function readVisibleCommand() {
    const terminal = terminalRef.current;
    if (!terminal) return "";
    const buffer = terminal.buffer.active;
    const line = buffer.getLine(buffer.baseY + buffer.cursorY)?.translateToString(true).trimEnd() ?? "";
    const promptIndex = Math.max(line.lastIndexOf("$ "), line.lastIndexOf("# "), line.lastIndexOf("> "));
    return promptIndex >= 0 ? line.slice(promptIndex + 2).trim() : "";
  }

  function isAtShellPromptCommandStart() {
    const terminal = terminalRef.current;
    if (!terminal) return false;
    const buffer = terminal.buffer.active;
    const line = buffer.getLine(buffer.baseY + buffer.cursorY)?.translateToString(false) ?? "";
    const cursorX = buffer.cursorX;
    const promptEnds = ["$ ", "# ", "> "]
      .map((prompt) => {
        const index = line.lastIndexOf(prompt, cursorX);
        return index >= 0 ? index + prompt.length : -1;
      })
      .filter((index) => index >= 0);
    if (promptEnds.length === 0) return false;
    return Math.max(...promptEnds) === cursorX;
  }

  function getHistoryMatches(query: string) {
    const normalized = query.trim().toLowerCase();
    const seen = new Set<string>();
    const matches: string[] = [];
    for (const historyCommand of commandHistoryRef.current) {
      const commandText = historyCommand.trim();
      if (!commandText || seen.has(commandText)) continue;
      if (normalized && !commandText.toLowerCase().includes(normalized)) continue;
      seen.add(commandText);
      matches.push(commandText);
      if (matches.length >= HISTORY_SUGGESTION_LIMIT) break;
    }
    return matches;
  }

  function updateHistorySuggest(next: HistorySuggestState) {
    historySuggestRef.current = next;
    setHistorySuggest(next);
  }

  function closeHistorySuggest() {
    updateHistorySuggest(closedHistorySuggest);
  }

  function positionHistorySuggest() {
    const terminal = terminalRef.current;
    const container = containerRef.current;
    const frame = container?.parentElement;
    if (!terminal || !container || !frame) return { x: 16, y: 16, maxHeight: 248 };

    const frameRect = frame.getBoundingClientRect();
    const clampPosition = (x: number, y: number) => {
      const popupWidth = Math.min(420, Math.max(220, frameRect.width - 24));
      const maxHeight = Math.min(248, Math.max(104, frameRect.height - 16));
      return {
        x: Math.min(Math.max(8, x), Math.max(8, frameRect.width - popupWidth - 8)),
        y: Math.min(Math.max(8, y), Math.max(8, frameRect.height - maxHeight - 8)),
        maxHeight,
      };
    };
    const helper = container.querySelector<HTMLTextAreaElement>(".xterm-helper-textarea");
    const helperRect = helper?.getBoundingClientRect();
    if (helperRect && helperRect.width > 0 && helperRect.height > 0) {
      return clampPosition(helperRect.left - frameRect.left, helperRect.bottom - frameRect.top + 6);
    }

    const containerRect = container.getBoundingClientRect();
    const cellWidth = terminal.cols > 0 ? containerRect.width / terminal.cols : fontSize * 0.62;
    const cellHeight = terminal.rows > 0 ? containerRect.height / terminal.rows : fontSize * 1.35;
    const buffer = terminal.buffer.active;
    return clampPosition(
      containerRect.left - frameRect.left + buffer.cursorX * cellWidth,
      containerRect.top - frameRect.top + (buffer.cursorY + 1) * cellHeight + 6,
    );
  }

  function openHistorySuggest() {
    updateHistorySuggest({
      active: true,
      query: "",
      selectedIndex: 0,
      ...positionHistorySuggest(),
    });
  }

  function setHistorySuggestQuery(query: string, selectedIndex = 0) {
    updateHistorySuggest({
      active: true,
      query,
      selectedIndex,
      ...positionHistorySuggest(),
    });
  }

  function pickHistorySuggestion(commandText: string) {
    closeHistorySuggest();
    terminalRef.current?.focus();
    rawSendTerminalInput(commandText);
  }

  function handleHistorySuggestInput(data: string) {
    const current = historySuggestRef.current;
    if (!current.active) return false;

    if (data === "\x1b") {
      closeHistorySuggest();
      return true;
    }
    if (data === "\x03") {
      closeHistorySuggest();
      return true;
    }
    if (data === "\r" || data === "\t") {
      const matches = getHistoryMatches(current.query);
      const commandText = matches[Math.min(current.selectedIndex, matches.length - 1)];
      if (commandText) pickHistorySuggestion(commandText);
      return true;
    }
    if (data === "\x7f" || data === "\b") {
      if (!current.query) {
        closeHistorySuggest();
        return true;
      }
      setHistorySuggestQuery(Array.from(current.query).slice(0, -1).join(""));
      return true;
    }
    if (data === "\x1b[A" || data === "\x1b[B") {
      const matches = getHistoryMatches(current.query);
      if (matches.length === 0) return true;
      const direction = data === "\x1b[A" ? -1 : 1;
      const selectedIndex = (current.selectedIndex + direction + matches.length) % matches.length;
      updateHistorySuggest({ ...current, selectedIndex, ...positionHistorySuggest() });
      return true;
    }

    const text = data.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "");
    const printable = Array.from(text).filter((character) => character >= " " && character !== "\x7f").join("");
    if (printable) {
      setHistorySuggestQuery(current.query + printable);
      return true;
    }
    return true;
  }

  /** The `# …` description on the cursor's line, if that is what it holds. */
  function currentAiRequest(): string | null {
    const buffer = terminalRef.current?.buffer.active;
    const line = buffer?.getLine(buffer.baseY + buffer.cursorY)?.translateToString(true) ?? "";
    return requestFromPromptLine(line) ?? naturalLanguageRequest(commandDraftRef.current);
  }

  /** Replace the `# …` line with a command for it, without running it. */
  async function suggestCommand(request: string) {
    const suggest = onSuggestCommandRef.current;
    const sessionId = sessionRef.current;
    if (!suggest) return;
    suggestingRef.current = true;
    setNotice(t("AI 正在生成命令…"));
    try {
      const commandText = cleanSuggestedCommand(await suggest(request));
      // The user may have switched sessions or typed on meanwhile.
      if (sessionRef.current !== sessionId || currentAiRequest() !== request) return;
      if (!commandText) {
        setNotice(t("AI 没有给出命令"));
        return;
      }
      // Ctrl+U clears the line; the command is typed but not submitted.
      rawSendTerminalInput(`\x15${commandText}`, false);
      setNotice(commandText.startsWith("#") ? t("AI 未给出可执行命令，说明已放在命令行") : t("命令已填入，确认后按回车执行"));
    } catch (error) {
      setNotice(t("生成命令失败：{0}", String(error)));
    } finally {
      suggestingRef.current = false;
    }
  }

  function sendTerminalInput(data: string) {
    if (handleHistorySuggestInput(data)) return;
    rawSendTerminalInput(data);
  }

  /**
   * Send input as if typed. `broadcast` mirrors it to the other broadcast
   * targets when this pane is one; input that arrived from a broadcast, or that
   * only makes sense for this session, passes false.
   */
  function rawSendTerminalInput(data: string, broadcast = true) {
    const currentSession = sessionRef.current;
    if (currentSession && closedSessionRef.current === currentSession) return;
    // While a file transfer runs the line carries ZMODEM, not keystrokes.
    const transfer = currentSession ? zmodemsRef.current[currentSession] : undefined;
    if (transfer?.active) {
      if (data.includes("\x03")) transfer.abort();
      return;
    }
    if (broadcast && broadcastingRef.current) onBroadcastInputRef.current?.(tab.id, data);
    updateCommandDraft(data);
    if (isTauri && currentSession) {
      command("terminal_input", { sessionId: currentSession, data }).catch((error) => {
        setNotice(String(error));
        if (terminalRef.current) terminalRef.current.options.disableStdin = true;
      });
    } else if (!isTauri) {
      // Browser demo: echo locally and fake a prompt after every Enter.
      terminalRef.current?.write(data.replace(/\r/g, "\r\n$ "));
    }
  }

  useEffect(() => {
    onReadyRef.current = onReady;
  }, [onReady]);

  useEffect(() => {
    onClosedRef.current = onClosed;
  }, [onClosed]);

  useEffect(() => {
    onCommandSubmittedRef.current = onCommandSubmitted;
  }, [onCommandSubmitted]);

  useEffect(() => {
    commandHistoryRef.current = commandHistory;
  }, [commandHistory]);

  useEffect(() => {
    if (!containerRef.current) return;

    const fit = new FitAddon();
    const terminal = new Terminal({
      cursorBlink: true,
      fontFamily,
      fontSize,
      fontWeight,
      fontWeightBold: boldWeightFor(fontWeight),
      lineHeight: 1.35,
      scrollback: 8000,
      // Required for the glass theme: without this xterm paints an opaque
      // background (ignoring the #00000000 theme color) and covers the macOS
      // vibrancy across the whole terminal region.
      // An opaque background (opaqueBackground) turns this off for ClearType.
      allowTransparency: !opaqueBackground,
      // Search highlights use the decorations API.
      allowProposedApi: true,
      theme: getTerminalTheme(theme, opaqueBackground),
    });

    const search = new SearchAddon();
    terminal.loadAddon(fit);
    terminal.loadAddon(search);
    enableUnicode11(terminal);
    // The theme background is transparent, so xterm would answer a background
    // query (OSC 11 "?") with black and CLIs that adapt to it (Codex, Claude
    // Code, vim) would pick dark styling on light themes, e.g. dark ink on a
    // dark input box. Answer with the surface the terminal actually sits on.
    terminal.parser.registerOscHandler(11, (data) => {
      if (data !== "?") return false;
      // Reply to this session only, never through the (possibly broadcast) input path.
      const sessionId = sessionRef.current;
      if (isTauri && sessionId) {
        command("terminal_input", {
          sessionId,
          data: `\x1b]11;${getTerminalSurfaceReport(themeRef.current)}\x1b\\`,
        }).catch(() => undefined);
      }
      return true;
    });
    terminal.open(containerRef.current);
    const releaseWebgl = gpuRendering
      ? enableWebglRenderer(terminal, (reason) => console.warn(`[iShell] ${reason}`))
      : undefined;
    terminal.registerLinkProvider(directoryLinkProvider(
      terminal,
      (path) => onOpenDirectoryRef.current?.(path),
      platformSource.includes("mac"),
    ));
    fit.fit();
    terminal.writeln(`iShell · ${tab.title}`);
    terminal.writeln("");

    let disposed = false;
    const pasteClipboard = createClipboardPasteHandler({
      getSession: () => {
        const sessionId = sessionRef.current;
        return sessionId && closedSessionRef.current !== sessionId
          ? { sessionId, serverId: tab.serverId }
          : null;
      },
      isDisposed: () => disposed,
      readClipboard: ({ sessionId, serverId }) => {
        if (zmodemsRef.current[sessionId]?.active) {
          return Promise.reject(new Error(t("文件传输期间无法粘贴，请等待传输完成")));
        }
        return command("terminal_paste_clipboard", { sessionId, id: serverId });
      },
      pasteText: (text) => {
        if (sessionRef.current && zmodemsRef.current[sessionRef.current]?.active) {
          throw new Error(t("文件传输期间无法粘贴，请等待传输完成"));
        }
        closeHistorySuggest();
        terminal.paste(text);
      },
      pasteImage: (path) => {
        if (sessionRef.current && zmodemsRef.current[sessionRef.current]?.active) {
          throw new Error(t("截图已上传，文件传输期间无法填入路径：{0}", path));
        }
        closeHistorySuggest();
        // A remote path is only valid on this host. Bypass broadcast input.
        rawSendTerminalInput(imagePasteData(path, terminal.modes.bracketedPasteMode), false);
      },
      setNotice,
    });
    // Covers native menu/context-menu paste as well as keyboard shortcuts.
    // Capture before xterm's text-only paste handler to prevent double pastes.
    const container = containerRef.current;
    const onPaste = (event: ClipboardEvent) => {
      if (!isTauri || isLocalServerId(tab.serverId)) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      void pasteClipboard();
    };
    container.addEventListener("paste", onPaste, true);

    terminal.attachCustomKeyEventHandler((event) => {
      if (isTauri && !isLocalServerId(tab.serverId) && isTerminalPasteShortcut(event, isWindowsPlatform)) {
        event.preventDefault();
        void pasteClipboard();
        return false;
      }
      const splitDirection = splitShortcutDirection(event);
      if (splitDirection && onSplitRef.current) {
        onSplitRef.current(splitDirection);
        event.preventDefault();
        return false;
      }

      if (isAskAiShortcut(event, isMacPlatform) && onAskAiRef.current) {
        const selection = terminal.getSelection();
        if (selection.trim()) {
          onAskAiRef.current(selection);
          event.preventDefault();
          return false;
        }
      }

      if (isGenerateCommandShortcut(event, isMacPlatform) && onSuggestCommandRef.current) {
        const request = currentAiRequest();
        if (request) {
          event.preventDefault();
          if (!suggestingRef.current) void suggestCommand(request);
          return false;
        }
      }

      if (isTerminalSearchShortcut(event)) {
        closeHistorySuggest();
        setSearchSignal((signal) => signal + 1);
        event.preventDefault();
        return false;
      }

      if (
        event.type === "keydown" &&
        event.key === "Alt" &&
        event.altKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.shiftKey &&
        !historySuggestRef.current.active &&
        !sensitiveInputRef.current &&
        commandDraftRef.current.length === 0 &&
        isAtShellPromptCommandStart()
      ) {
        openHistorySuggest();
        event.preventDefault();
        return false;
      }

      // Product shortcut on Windows is intentionally Win+C / Win+V. Keep
      // Ctrl+C available for sending SIGINT to the remote shell.
      if (!isWindowsPlatform || event.type !== "keydown" || !event.metaKey || event.ctrlKey || event.altKey) {
        return true;
      }

      if (event.key.toLowerCase() === "c") {
        const selection = terminal.getSelection();
        if (selection) {
          navigator.clipboard.writeText(selection).catch((error) => setNotice(t("复制失败：{0}", String(error))));
        }
        event.preventDefault();
        return false;
      }

      if (event.key.toLowerCase() === "v") {
        navigator.clipboard
          .readText()
          .then((text) => {
            if (!text) return;
            closeHistorySuggest();
            rawSendTerminalInput(text);
          })
          .catch((error) => setNotice(t("粘贴失败：{0}", String(error))));
        event.preventDefault();
        return false;
      }

      return true;
    });

    terminal.onData((data) => sendTerminalInput(data));

    let fitFrame = 0;
    let settledFrame = 0;
    const fitWhenVisible = () => {
      // Skip while hidden (display:none) — the container has no size then.
      if (disposed || containerRef.current?.offsetParent === null) return;
      cancelAnimationFrame(fitFrame);
      cancelAnimationFrame(settledFrame);
      fitFrame = requestAnimationFrame(() => {
        if (
          disposed ||
          !containerRef.current?.clientWidth ||
          !containerRef.current?.clientHeight
        ) return;
        fit.fit();
        reportTerminalSize();
        settledFrame = requestAnimationFrame(() => {
          if (disposed || containerRef.current?.offsetParent === null) return;
          fit.fit();
          reportTerminalSize();
          terminal.refresh(0, terminal.rows - 1);
        });
      });
    };
    const refit = () => {
      if (disposed) return;
      if (fitTimerRef.current !== null) window.clearTimeout(fitTimerRef.current);
      if (fitSettledTimerRef.current !== null) window.clearTimeout(fitSettledTimerRef.current);
      fitTimerRef.current = window.setTimeout(() => {
        fitTimerRef.current = null;
        fitWhenVisible();
      }, 16);
      fitSettledTimerRef.current = window.setTimeout(() => {
        fitSettledTimerRef.current = null;
        fitWhenVisible();
      }, 210);
    };
    requestFitRef.current = refit;
    const onVisible = () => {
      if (!document.hidden) refit();
    };
    const layoutRoot = containerRef.current.closest(".workbench-body");
    const onTransitionEnd = (event: Event) => {
      if (
        !(event.target instanceof Element) ||
        !event.target.matches(".connection-manager, .status-panel, .files-region, .terminal-region")
      ) return;
      const property = (event as TransitionEvent).propertyName;
      if (/^(width|height|flex-basis|margin|padding|transform)/.test(property)) refit();
    };
    window.addEventListener("resize", refit);
    window.addEventListener("focus", refit);
    window.addEventListener("pageshow", refit);
    document.addEventListener("visibilitychange", onVisible);
    document.fonts?.addEventListener("loadingdone", refit);
    void document.fonts?.ready.then(refit);
    layoutRoot?.addEventListener("transitionend", onTransitionEnd);
    // Refit when the container itself resizes (e.g. dragging the file dock).
    const observer = new ResizeObserver(refit);
    observer.observe(containerRef.current);
    if (containerRef.current.parentElement) observer.observe(containerRef.current.parentElement);

    terminalRef.current = terminal;
    setSearchAddon(search);
    refit();

    return () => {
      disposed = true;
      container.removeEventListener("paste", onPaste, true);
      requestFitRef.current = null;
      cancelAnimationFrame(fitFrame);
      cancelAnimationFrame(settledFrame);
      window.removeEventListener("resize", refit);
      window.removeEventListener("focus", refit);
      window.removeEventListener("pageshow", refit);
      document.removeEventListener("visibilitychange", onVisible);
      document.fonts?.removeEventListener("loadingdone", refit);
      layoutRoot?.removeEventListener("transitionend", onTransitionEnd);
      if (fitTimerRef.current !== null) window.clearTimeout(fitTimerRef.current);
      if (fitSettledTimerRef.current !== null) window.clearTimeout(fitSettledTimerRef.current);
      observer.disconnect();
      setSearchAddon(null);
      releaseWebgl?.();
      terminal.dispose();
    };
  }, [setNotice, tab.title]);

  useEffect(() => {
    if (!visible || !pasteRequest) return;
    terminalRef.current?.focus();
    closeHistorySuggest();
    rawSendTerminalInput(pasteRequest.command);
  }, [pasteRequest?.id, visible]);

  useEffect(() => {
    if (!visible || !commandRequest) return;
    closeHistorySuggest();
    if (
      commandRequest.requirePromptStart &&
      (sensitiveInputRef.current || commandDraftRef.current.length > 0 || !isAtShellPromptCommandStart())
    ) {
      setNotice(commandRequest.blockedNotice ?? t("终端当前不在普通命令行状态，已取消操作"));
      return;
    }
    terminalRef.current?.focus();
    rawSendTerminalInput(commandRequest.command, false);
  }, [commandRequest?.id, visible]);

  useEffect(() => {
    if (!inputRegistry) return;
    const tabId = tab.id;
    inputRegistry.set(tabId, (data) => rawSendTerminalInput(data, false));
    return () => {
      inputRegistry.delete(tabId);
    };
  }, [inputRegistry, tab.id]);

  useEffect(() => {
    themeRef.current = theme;
    if (!terminalRef.current) return;
    terminalRef.current.options.allowTransparency = !opaqueBackground;
    terminalRef.current.options.theme = getTerminalTheme(theme, opaqueBackground);
    requestFitRef.current?.();
  }, [theme, opaqueBackground]);

  useEffect(() => {
    if (!terminalRef.current) return;
    const options = terminalRef.current.options;
    options.fontSize = fontSize;
    options.fontFamily = fontFamily;
    options.fontWeight = fontWeight;
    options.fontWeightBold = boldWeightFor(fontWeight);
    requestFitRef.current?.();
  }, [fontSize, fontFamily, fontWeight]);

  // Each pane subscribes to its session with a channel of its own. Attaching
  // replays the session's buffered output and subscribes in one step, so
  // nothing falls in between; output already shown is skipped by offset.
  useEffect(() => {
    if (!isTauri || !tab.sessionId) return;
    const sessionId = tab.sessionId;
    let active = true;
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (buffer) => {
      if (!active || sessionRef.current !== sessionId) return;
      const frame = parseFrame(buffer);
      if (!frame) return;
      if (frame.kind === FRAME_DATA) writeTerminalBytes(sessionId, frame.offset, frame.payload);
      else if (frame.kind === FRAME_READY) markReady(sessionId);
      else if (frame.kind === FRAME_CLOSED)
        showClosed(sessionId, translateMessage(new TextDecoder().decode(frame.payload)));
    };
    command("attach_terminal", { sessionId, onOutput: channel })
      .then((encoding) => {
        if (!active) return;
        switchDecoder(sessionId, encoding);
        liveSessionsRef.current.add(sessionId);
      })
      .catch((error) => {
        if (active) setNotice(String(error));
      });
    return () => {
      active = false;
      liveSessionsRef.current.delete(sessionId);
      void command("detach_terminal", { sessionId, channelId: channel.id }).catch(() => undefined);
    };
  }, [setNotice, tab.sessionId]);

  useEffect(() => {
    if (!terminalRef.current) return;
    if (!isTauri && tab.sessionId?.startsWith("demo-")) {
      terminalRef.current.clear();
      terminalRef.current.writeln(`[iShell] connected to ${tab.title}`);
      terminalRef.current.write("$ ");
    }
    if (isTauri && tab.state === "connecting" && !connectingNoticeRef.current) {
      if (terminalRef.current) terminalRef.current.options.disableStdin = false;
      terminalRef.current.writeln("\r\n[iShell] connecting...");
      connectingNoticeRef.current = true;
    }
    if (isTauri && tab.state === "closed") {
      if (terminalRef.current) terminalRef.current.options.disableStdin = true;
    }
    if (tab.state !== "connecting") {
      connectingNoticeRef.current = false;
    }
  }, [tab.sessionId, tab.state, tab.title]);

  useEffect(() => {
    if (!visible) return;
    requestFitRef.current?.();
  }, [layoutSignal, visible]);

  useEffect(() => {
    if (!visible) closeHistorySuggest();
  }, [visible]);

  useEffect(() => {
    if (!historySuggest.active) return;
    selectedSuggestionRef.current?.scrollIntoView({ block: "nearest" });
  }, [historySuggest.active, historySuggest.selectedIndex, suggestedCommands.length]);

  return (
    <section
      className={`terminal-panel ${visible ? "active" : ""} ${pane ? "split" : ""} ${pane?.focused ? "focused" : ""} ${
        pane && pane.rect.top > 0 ? "pane-below" : ""
      } ${broadcasting ? "broadcasting" : ""}`}
      style={pane ? {
        inset: "auto",
        left: `${pane.rect.left}%`,
        top: `${pane.rect.top}%`,
        width: `${pane.rect.width}%`,
        height: `${pane.rect.height}%`,
      } : undefined}
      aria-hidden={!visible}
      data-pane-id={tab.id}
      onPointerDownCapture={pane && !pane.focused ? onFocusPane : undefined}
      onFocusCapture={pane && !pane.focused ? onFocusPane : undefined}
    >
      <div
        className="terminal-frame"
        style={opaqueBackground ? { background: getThemeDefinition(theme).terminalSurface } : undefined}
      >
        <div className="terminal-host" ref={containerRef} />
        {searchSignal > 0 && searchAddon && (
          <TerminalSearchBar
            search={searchAddon}
            theme={theme}
            openSignal={searchSignal}
            onClose={() => {
              setSearchSignal(0);
              terminalRef.current?.focus();
            }}
          />
        )}
        {historySuggest.active && (
          <div
            className="terminal-history-suggest"
            style={{ left: historySuggest.x, top: historySuggest.y, maxHeight: historySuggest.maxHeight }}
            role="listbox"
            aria-label={t("历史命令候选")}
          >
            <div className="terminal-history-suggest-head">
              <span>{historySuggest.query || t("历史命令")}</span>
              <kbd>Enter</kbd>
            </div>
            <div
              className="terminal-history-suggest-list"
              style={{ maxHeight: Math.max(52, historySuggest.maxHeight - 30) }}
            >
              {suggestedCommands.length > 0 ? (
                suggestedCommands.map((commandText, index) => (
                  <button
                    key={`${index}-${commandText}`}
                    ref={index === historySuggest.selectedIndex ? selectedSuggestionRef : undefined}
                    type="button"
                    className={`terminal-history-suggest-row ${
                      index === historySuggest.selectedIndex ? "selected" : ""
                    }`}
                    onMouseDown={(event) => event.preventDefault()}
                    onClick={() => pickHistorySuggestion(commandText)}
                    title={commandText}
                    role="option"
                    aria-selected={index === historySuggest.selectedIndex}
                  >
                    <code>{commandText}</code>
                  </button>
                ))
              ) : (
                <div className="terminal-history-suggest-empty">{t("没有匹配命令")}</div>
              )}
            </div>
          </div>
        )}
        {(pane || broadcasting) && (
          <div className="pane-label">
            {broadcasting && <span className="pane-label-broadcast" title={t("正在广播输入")}>{t("广播")}</span>}
            {pane && (
              <>
                <span className={`tab-dot ${tab.state}`} style={{ color: tab.color }} />
                <span className="pane-label-title" title={`${tab.title} · ${tab.subtitle}`}>{tab.title}</span>
                <button type="button" title={t("关闭窗格")} aria-label={t("关闭窗格 {0}", tab.title)} onClick={onClosePane}>
                  <X size={12} />
                </button>
              </>
            )}
          </div>
        )}
      </div>
      {tab.state === "closed" && (
        <button type="button" className="terminal-reconnect" onClick={onReconnect}>
          <RefreshCw size={13} />
          {t("重新连接")}</button>
      )}
    </section>
  );
}

export const TerminalPane = memo(TerminalPaneBase, (previous, next) => {
  const prevTab = previous.tab;
  const nextTab = next.tab;
  return (
    prevTab.id === nextTab.id &&
    prevTab.title === nextTab.title &&
    prevTab.sessionId === nextTab.sessionId &&
    prevTab.state === nextTab.state &&
    previous.visible === next.visible &&
    previous.theme === next.theme &&
    previous.fontSize === next.fontSize &&
    previous.fontFamily === next.fontFamily &&
    previous.fontWeight === next.fontWeight &&
    previous.opaqueBackground === next.opaqueBackground &&
    previous.layoutSignal === next.layoutSignal &&
    previous.pasteRequest?.id === next.pasteRequest?.id &&
    previous.commandRequest?.id === next.commandRequest?.id &&
    previous.commandHistory === next.commandHistory &&
    previous.pane?.focused === next.pane?.focused &&
    previous.pane?.rect.left === next.pane?.rect.left &&
    previous.pane?.rect.top === next.pane?.rect.top &&
    previous.pane?.rect.width === next.pane?.rect.width &&
    previous.pane?.rect.height === next.pane?.rect.height &&
    Boolean(previous.onSplit) === Boolean(next.onSplit) &&
    previous.broadcasting === next.broadcasting
  );
});
