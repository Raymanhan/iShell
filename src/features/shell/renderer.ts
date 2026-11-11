import type { Terminal } from "@xterm/xterm";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";

/**
 * Measure characters by Unicode 11, which gives CJK, emoji and other wide
 * characters their real two-cell width. Needs `allowProposedApi`.
 */
export function enableUnicode11(terminal: Terminal) {
  terminal.loadAddon(new Unicode11Addon());
  terminal.unicode.activeVersion = "11";
}

/**
 * Draw with the GPU. Falls back to xterm's DOM renderer when WebGL is not
 * available, and returns to it if the GPU context is lost later (driver
 * reset, sleep, too many contexts). Call after `terminal.open()`.
 * Returns a function that switches back to the DOM renderer.
 */
export function enableWebglRenderer(terminal: Terminal, onFallback?: (reason: string) => void) {
  let addon: WebglAddon | null = null;
  const release = () => {
    addon?.dispose();
    addon = null;
  };
  try {
    addon = new WebglAddon();
    addon.onContextLoss(() => {
      release();
      onFallback?.("GPU 渲染上下文丢失，已切换为普通渲染");
    });
    terminal.loadAddon(addon);
  } catch (error) {
    release();
    onFallback?.(`GPU 渲染不可用：${error}`);
  }
  return release;
}
