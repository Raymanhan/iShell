import { useEffect, useState } from "react";
import { isTauri } from "../../api/tauri";
import { readSavedTailViewerLines, TAIL_VIEWER_DEFAULT_LINES_KEY } from "../../constants/tail";
import {
  applyWindowUnderlay,
  readSavedTheme,
  readSavedWindowTransparency,
  WINDOW_TRANSPARENCY_KEY,
  type AppTheme,
} from "../../constants/theme";
import { applyNativeWindowTheme } from "../theme/nativeWindowTheme";
import { AUTO_CHECK_UPDATES_KEY, readAutoCheckUpdates } from "../updater/useUpdater";

const THEME_KEY = "ishell.theme";
const TERMINAL_FONT_SIZE_KEY = "ishell.terminalFontSize";
const AUTO_HIDE_TOP_BAR_KEY = "ishell.autoHideTopBar";
const GPU_RENDERING_KEY = "ishell.terminalGpuRendering";

export const MIN_TERMINAL_FONT_SIZE = 11;
export const MAX_TERMINAL_FONT_SIZE = 20;
const DEFAULT_TERMINAL_FONT_SIZE = 14;

/** localStorage can be unavailable (private windows, blocked storage). */
function load(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key: string, value: string) {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // The setting still applies for this session.
  }
}

export function readTerminalFontSize(raw: string | null): number {
  const saved = raw === null ? Number.NaN : Number(raw);
  return Number.isFinite(saved)
    ? Math.min(MAX_TERMINAL_FONT_SIZE, Math.max(MIN_TERMINAL_FONT_SIZE, saved))
    : DEFAULT_TERMINAL_FONT_SIZE;
}

/**
 * The window's preferences, each read from and saved to localStorage, with
 * the theme and transparency applied to the document and native window.
 */
export function useAppSettings() {
  const [theme, setTheme] = useState<AppTheme>(() => readSavedTheme(load(THEME_KEY)));
  const [windowTransparency, setWindowTransparency] = useState(() =>
    readSavedWindowTransparency(load(WINDOW_TRANSPARENCY_KEY)),
  );
  const [terminalFontSize, setTerminalFontSize] = useState(() =>
    readTerminalFontSize(load(TERMINAL_FONT_SIZE_KEY)),
  );
  const [tailViewerDefaultLines, setTailViewerDefaultLines] = useState(() =>
    readSavedTailViewerLines(load(TAIL_VIEWER_DEFAULT_LINES_KEY)),
  );
  const [autoCheckUpdates, setAutoCheckUpdates] = useState(readAutoCheckUpdates);
  const [autoHideTopBar, setAutoHideTopBar] = useState(
    () => load(AUTO_HIDE_TOP_BAR_KEY) === "true",
  );
  const [gpuRendering, setGpuRendering] = useState(() => load(GPU_RENDERING_KEY) !== "false");

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    store(THEME_KEY, theme);
    if (isTauri) void applyNativeWindowTheme(theme);
  }, [theme]);

  useEffect(() => {
    applyWindowUnderlay(theme, windowTransparency);
    store(WINDOW_TRANSPARENCY_KEY, String(windowTransparency));
  }, [theme, windowTransparency]);

  useEffect(() => store(TERMINAL_FONT_SIZE_KEY, String(terminalFontSize)), [terminalFontSize]);
  useEffect(
    () => store(TAIL_VIEWER_DEFAULT_LINES_KEY, String(tailViewerDefaultLines)),
    [tailViewerDefaultLines],
  );
  useEffect(() => store(AUTO_HIDE_TOP_BAR_KEY, String(autoHideTopBar)), [autoHideTopBar]);
  useEffect(() => store(GPU_RENDERING_KEY, String(gpuRendering)), [gpuRendering]);
  useEffect(() => store(AUTO_CHECK_UPDATES_KEY, String(autoCheckUpdates)), [autoCheckUpdates]);

  return {
    theme,
    setTheme,
    windowTransparency,
    setWindowTransparency,
    terminalFontSize,
    setTerminalFontSize,
    tailViewerDefaultLines,
    setTailViewerDefaultLines,
    autoCheckUpdates,
    setAutoCheckUpdates,
    autoHideTopBar,
    setAutoHideTopBar,
    gpuRendering,
    setGpuRendering,
  };
}
