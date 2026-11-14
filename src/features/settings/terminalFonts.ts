import { msg } from "../../i18n";

export type FontPlatform = "windows" | "macos" | "linux" | "other";

export interface TerminalFontPreset {
  /** The CSS family name, also what is saved. */
  family: string;
  /** Why it is suggested; a message id. */
  note: string;
  /** Platforms the preset is offered on. */
  platforms: readonly FontPlatform[];
  /** Ships with the platform, so it is always there. */
  bundled?: readonly FontPlatform[];
}

/**
 * Suggested terminal fonts: monospace families drawn for screens, with
 * distinct 0/O and 1/l/I and good hinting at 12-16px.
 */
export const terminalFontPresets: readonly TerminalFontPreset[] = [
  {
    family: "Cascadia Mono",
    note: msg("Windows Terminal 默认字体，专为屏幕显示设计"),
    platforms: ["windows"],
    bundled: ["windows"],
  },
  {
    family: "Consolas",
    note: msg("Windows 自带，ClearType 微调，小字号最锐利"),
    platforms: ["windows"],
    bundled: ["windows"],
  },
  {
    family: "SF Mono",
    note: msg("macOS 系统等宽字体"),
    platforms: ["macos"],
  },
  {
    family: "Menlo",
    note: msg("macOS 自带，字形清晰"),
    platforms: ["macos"],
    bundled: ["macos"],
  },
  {
    family: "DejaVu Sans Mono",
    note: msg("多数 Linux 发行版自带，覆盖字符广"),
    platforms: ["linux"],
  },
  {
    family: "JetBrains Mono",
    note: msg("字身高、易区分 0O 和 1lI，需另行安装"),
    platforms: ["windows", "macos", "linux", "other"],
  },
  {
    family: "Sarasa Mono SC",
    note: msg("更纱黑体：中英文严格等宽对齐，中文清晰，需另行安装"),
    platforms: ["windows", "macos", "linux", "other"],
  },
];

/** What follows the chosen font, so missing glyphs (CJK, symbols) still render. */
const fallbackFamilies: Record<FontPlatform, string> = {
  windows: '"Cascadia Mono", "Cascadia Code", Consolas, "Microsoft YaHei Mono", "Microsoft YaHei", monospace',
  macos: '"SF Mono", Menlo, "PingFang SC", monospace',
  linux: '"DejaVu Sans Mono", "Noto Sans Mono", "Noto Sans Mono CJK SC", "Noto Sans CJK SC", monospace',
  other: '"SF Mono", "Cascadia Mono", "JetBrains Mono", Menlo, Consolas, monospace',
};

export function currentFontPlatform(): FontPlatform {
  const platform = typeof document === "undefined" ? undefined : document.documentElement.dataset.platform;
  return platform === "windows" || platform === "macos" || platform === "linux" ? platform : "other";
}

export function presetsFor(platform: FontPlatform) {
  return terminalFontPresets.filter((preset) => preset.platforms.includes(platform));
}

/**
 * The saved font setting, cleaned: an empty string means the platform default.
 * Characters that could end the CSS value are dropped.
 */
export function normalizeFontSetting(value: string | null | undefined) {
  return (value ?? "").replace(/[;{}<>\\]/g, "").trim().slice(0, 200);
}

/** Quote one family name for CSS unless it is a generic family or already quoted. */
function quoteFamily(name: string) {
  const trimmed = name.trim();
  if (!trimmed) return "";
  if (/^["'].*["']$/.test(trimmed)) return trimmed;
  if (/^(monospace|ui-monospace|serif|sans-serif|system-ui)$/i.test(trimmed)) return trimmed;
  return `"${trimmed.replace(/"/g, "")}"`;
}

/**
 * The CSS font-family for the terminal: the chosen family (or comma-separated
 * list) first, then the platform's default stack as a fallback.
 */
export function buildTerminalFontFamily(setting: string, platform: FontPlatform) {
  const chosen = normalizeFontSetting(setting)
    .split(",")
    .map(quoteFamily)
    .filter(Boolean)
    .join(", ");
  return chosen ? `${chosen}, ${fallbackFamilies[platform]}` : fallbackFamilies[platform];
}

export type TerminalFontWeight = "normal" | "500" | "600";
export const terminalFontWeights: readonly TerminalFontWeight[] = ["normal", "500", "600"];

export function readTerminalFontWeight(value: string | null): TerminalFontWeight {
  return terminalFontWeights.includes(value as TerminalFontWeight) ? (value as TerminalFontWeight) : "normal";
}

/** The bold weight that stays visibly heavier than the regular one. */
export function boldWeightFor(weight: TerminalFontWeight) {
  return weight === "600" ? "800" : "bold";
}

/**
 * How terminal text is drawn. Glyphs on a transparent background only get
 * grayscale antialiasing, which looks thin and soft (worst on light themes);
 * an opaque background enables subpixel antialiasing (ClearType).
 */
export type TerminalTextRendering = "auto" | "sharp" | "transparent";
export const terminalTextRenderings: readonly TerminalTextRendering[] = ["auto", "sharp", "transparent"];

export function readTerminalTextRendering(value: string | null): TerminalTextRendering {
  return terminalTextRenderings.includes(value as TerminalTextRendering)
    ? (value as TerminalTextRendering)
    : "auto";
}

/**
 * Whether the terminal draws on an opaque background for sharper text.
 * "auto" does so on Windows and macOS, whatever the theme.
 */
export function wantsOpaqueTerminal(mode: TerminalTextRendering, platform: FontPlatform) {
  if (mode === "sharp") return true;
  if (mode === "transparent") return false;
  return platform === "windows" || platform === "macos";
}

/**
 * Whether a font is installed: text drawn in it must measure differently from
 * at least one generic family it would otherwise fall back to.
 */
export function isFontAvailable(family: string) {
  if (typeof document === "undefined") return false;
  const canvas = document.createElement("canvas");
  const context = canvas.getContext("2d");
  if (!context) return false;
  const sample = "mmmmmmmmmmlli10OWw@#中文";
  const quoted = quoteFamily(family);
  return ["monospace", "serif", "sans-serif"].some((generic) => {
    context.font = `72px ${generic}`;
    const baseline = context.measureText(sample).width;
    context.font = `72px ${quoted}, ${generic}`;
    return context.measureText(sample).width !== baseline;
  });
}
