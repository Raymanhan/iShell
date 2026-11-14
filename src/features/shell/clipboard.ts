import { t } from "../../i18n";

export type ClipboardPaste =
  | { kind: "text"; content: string }
  | { kind: "image"; content: string }
  | { kind: "empty" };

export interface ClipboardSession {
  sessionId: string;
  serverId: string;
}

export function isTerminalPasteShortcut(event: Pick<KeyboardEvent,
  "type" | "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey"
>, windows: boolean): boolean {
  if (event.type !== "keydown" || event.altKey) return false;
  if (event.key === "Insert") return event.shiftKey && !event.ctrlKey && !event.metaKey;
  if (event.key.toLowerCase() !== "v") return false;
  if (event.metaKey && !event.ctrlKey) return !event.shiftKey;
  // Preserve Ctrl+V (literal-next) in Unix terminals.
  return event.ctrlKey && !event.metaKey && (windows || event.shiftKey);
}

/** Quote paths for shell prompts as well as CLI input, and never send Enter. */
export function imagePasteData(path: string, bracketedPaste: boolean): string {
  const text = `'${path.replace(/'/g, "'\\''")}' `;
  return bracketedPaste ? `\x1b[200~${text}\x1b[201~` : text;
}

export function createClipboardPasteHandler(options: {
  getSession: () => ClipboardSession | null;
  isDisposed: () => boolean;
  readClipboard: (session: ClipboardSession) => Promise<ClipboardPaste>;
  pasteText: (text: string) => void;
  pasteImage: (path: string) => void;
  setNotice: (notice: string) => void;
}): () => Promise<void> {
  let busy = false;
  return async () => {
    if (options.isDisposed()) return;
    if (busy) {
      options.setNotice(t("正在处理上一次粘贴，请稍候"));
      return;
    }
    const session = options.getSession();
    if (!session) {
      options.setNotice(t("请先连接目标服务器"));
      return;
    }
    busy = true;
    options.setNotice(t("正在粘贴…"));
    try {
      const result = await options.readClipboard(session);
      if (options.isDisposed()) return;
      const current = options.getSession();
      if (current?.sessionId !== session.sessionId || current.serverId !== session.serverId) {
        if (result.kind === "image") {
          options.setNotice(t("截图已上传至 {0}，原终端会话已变化，未填入路径", result.content));
        }
        return;
      }
      if (result.kind === "image") {
        options.pasteImage(result.content);
        options.setNotice(t("截图已上传，路径已填入终端"));
      } else if (result.kind === "text") {
        options.pasteText(result.content);
        options.setNotice("");
      } else {
        options.setNotice(t("剪贴板中没有可粘贴的文字或图片"));
      }
    } catch (error) {
      if (!options.isDisposed()) options.setNotice(t("粘贴失败：{0}", String(error)));
    } finally {
      busy = false;
    }
  };
}
