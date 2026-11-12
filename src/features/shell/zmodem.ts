/**
 * ZMODEM file transfers in the terminal: `sz file` on the server sends a
 * file to this computer, `rz` asks for files to upload. The terminal's raw
 * output goes through a zmodem.js Sentry, which passes ordinary output on
 * and takes over while a transfer runs. It imports nothing from the app but
 * i18n, so the tests can load it.
 */
import Zmodem from "zmodem.js";
import { t } from "../../i18n";

/** A file picked on this computer, read in pieces as it is sent. */
export interface LocalFile {
  name: string;
  size: number;
  /** Seconds since the epoch, or null. */
  mtime: number | null;
  read(offset: number, length: number): Promise<Uint8Array>;
}

export interface ZmodemHost {
  /** Raw bytes for the remote side. Resolves once they are queued. */
  write(bytes: Uint8Array): Promise<void>;
  /** Ordinary terminal output, to decode and show. */
  output(bytes: Uint8Array): void;
  /** A status line for the terminal. */
  status(text: string): void;
  /** Files to send for `rz`; empty to cancel. */
  pickFiles(): Promise<LocalFile[]>;
  /** Store a received file; returns where it went. */
  saveFile(name: string, chunks: Uint8Array[]): Promise<string>;
  /** A transfer started or ended: typed input should be held meanwhile. */
  onActiveChange(active: boolean): void;
}

/** Bytes of a file sent per ZMODEM data subpacket batch. */
const SEND_CHUNK = 64 * 1024;

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** A file name from the sender, reduced to a plain name safe to save under. */
export function safeFileName(name: string): string {
  const base = name.split(/[\\/]/).pop() ?? "";
  // eslint-disable-next-line no-control-regex
  const cleaned = base.replace(/[\u0000-\u001f<>:"|?*]/g, "_").replace(/^\.+$/, "").trim();
  return cleaned || "received-file";
}

/**
 * The header that starts a transfer (ZRQINIT or ZRINIT, hex form), as the
 * Sentry passes it on to the terminal: line noise to a reader.
 */
// eslint-disable-next-line no-control-regex
const START_HEADER = /\*\*\x18B0[01][0-9a-f]{12}\r[\n\x8a]?\x11?/g;

/** Terminal output with transfer start headers removed. */
export function withoutStartHeaders(bytes: Uint8Array): Uint8Array {
  // Latin-1 maps bytes to characters one to one, so this keeps all others.
  let text = "";
  for (const byte of bytes) text += String.fromCharCode(byte);
  if (!text.includes("**\x18B0")) return bytes;
  const kept = text.replace(START_HEADER, "");
  return Uint8Array.from(kept, (ch) => ch.charCodeAt(0));
}

export class ZmodemController {
  private sentry: ZmodemSentry;
  private session: ZmodemSession | null = null;
  // Writes go out in order; each waits for the one before.
  private writes: Promise<void> = Promise.resolve();
  private writeError: unknown = null;

  private readonly host: ZmodemHost;

  constructor(host: ZmodemHost) {
    this.host = host;
    this.sentry = this.createSentry();
  }

  private createSentry(): ZmodemSentry {
    return new Zmodem.Sentry({
      to_terminal: (octets) => {
        const bytes = withoutStartHeaders(Uint8Array.from(octets));
        if (bytes.length) this.host.output(bytes);
      },
      sender: (octets) => this.send(octets),
      on_retract: () => undefined,
      on_detect: (detection) => this.begin(detection),
    });
  }

  get active(): boolean {
    return this.session !== null;
  }

  /** Feed the terminal's raw output. */
  consume(bytes: Uint8Array) {
    try {
      this.sentry.consume(bytes);
    } catch (error) {
      this.recover(error);
    }
  }

  /**
   * After a protocol error the Sentry would keep feeding the dead session, so
   * it starts afresh. lrzsz's `sz` on a terminal ends without the final "OO"
   * zmodem.js expects: the transfer has finished, and what follows is the
   * shell's output again.
   */
  private recover(error: unknown) {
    const session = this.session as (ZmodemSession & { _input_buffer?: number[] }) | null;
    const leftover = /after ZFIN/.test(String(error)) ? session?._input_buffer : undefined;
    this.sentry = this.createSentry();
    if (leftover) {
      this.finish();
      if (leftover.length) this.consume(Uint8Array.from(leftover));
      return;
    }
    try {
      session?.abort();
    } catch {
      // Already ending.
    }
    this.finish(`[iShell] ${t("传输出错：{0}", String(error))}`);
  }

  /** Stop the running transfer, as Ctrl+C would. */
  abort() {
    if (!this.session) return;
    try {
      this.session.abort();
    } catch {
      // Already ending.
    }
    this.sentry = this.createSentry();
    this.finish(`[iShell] ${t("已取消传输")}`);
  }

  private send(octets: ArrayLike<number>) {
    const bytes = Uint8Array.from(octets);
    this.writes = this.writes
      .then(() => this.host.write(bytes))
      .catch((error) => {
        this.writeError = error;
      });
  }

  /** Wait until what was sent has been queued, so large files don't pile up. */
  private async drain() {
    await this.writes;
    if (this.writeError) throw this.writeError;
  }

  private finish(message?: string) {
    if (!this.session) return;
    this.session = null;
    if (message) this.host.status(message);
    this.host.onActiveChange(false);
  }

  private begin(detection: ZmodemDetection) {
    const session = detection.confirm();
    this.session = session;
    this.writeError = null;
    this.host.onActiveChange(true);
    session.on("session_end", () => this.finish());
    if (session.type === "send") void this.upload(session);
    else this.download(session);
  }

  /** `sz` on the server: receive what it offers. */
  private download(session: ZmodemSession) {
    session.on("offer", (offer: ZmodemOffer) => {
      const details = offer.get_details();
      const name = safeFileName(details.name);
      const size = details.size ?? 0;
      const chunks: Uint8Array[] = [];
      let received = 0;
      let lastReport = 0;
      this.host.status(`[iShell] ${t("正在接收 {0}（{1}）…", name, formatBytes(size))}`);
      offer
        .accept({
          on_input: (payload) => {
            const chunk = Uint8Array.from(payload);
            chunks.push(chunk);
            received += chunk.length;
            const now = Date.now();
            if (size > 0 && now - lastReport > 1000) {
              lastReport = now;
              this.host.status(`[iShell] ${name}：${Math.floor((received / size) * 100)}%`);
            }
          },
        })
        .then(() => this.host.saveFile(name, chunks))
        .then(
          (path) => this.host.status(`[iShell] ${t("已接收 {0}（{1}），保存到 {2}", name, formatBytes(received), path)}`),
          (error) => this.host.status(`[iShell] ${t("接收 {0} 失败：{1}", name, String(error))}`),
        );
    });
    session.start();
  }

  /** `rz` on the server: send files picked here. */
  private async upload(session: ZmodemSession) {
    let files: LocalFile[] = [];
    try {
      files = await this.host.pickFiles();
    } catch (error) {
      this.host.status(`[iShell] ${t("无法读取文件：{0}", String(error))}`);
    }
    if (!files.length) {
      this.host.status(`[iShell] ${t("未选择文件，已取消上传")}`);
      // Close the session cleanly so `rz` exits rather than waiting.
      try {
        await session.close();
      } catch {
        this.abort();
      }
      this.finish();
      return;
    }
    try {
      let bytesRemaining = files.reduce((sum, file) => sum + file.size, 0);
      for (const [index, file] of files.entries()) {
        const transfer = await session.send_offer({
          name: file.name,
          size: file.size,
          mtime: file.mtime ?? undefined,
          files_remaining: files.length - index,
          bytes_remaining: bytesRemaining,
        });
        bytesRemaining -= file.size;
        if (!transfer) {
          this.host.status(`[iShell] ${t("服务器跳过了 {0}", file.name)}`);
          continue;
        }
        this.host.status(`[iShell] ${t("正在发送 {0}（{1}）…", file.name, formatBytes(file.size))}`);
        let offset = transfer.get_offset?.() ?? 0;
        let lastReport = Date.now();
        while (offset < file.size) {
          if (!this.session) return;
          const chunk = await file.read(offset, Math.min(SEND_CHUNK, file.size - offset));
          if (!chunk.length) throw new Error(t("文件读取不完整"));
          offset += chunk.length;
          if (offset < file.size) transfer.send(chunk);
          else await transfer.end(chunk);
          await this.drain();
          if (Date.now() - lastReport > 1000 && file.size > 0) {
            lastReport = Date.now();
            this.host.status(`[iShell] ${file.name}：${Math.floor((offset / file.size) * 100)}%`);
          }
        }
        if (file.size === 0) await transfer.end([]);
        this.host.status(`[iShell] ${t("已发送 {0}", file.name)}`);
      }
      await session.close();
      await this.drain();
      this.finish();
    } catch (error) {
      this.host.status(`[iShell] ${t("发送失败：{0}", String(error))}`);
      this.abort();
    }
  }
}

// The parts of zmodem.js used here (it ships no types).
interface ZmodemSentry {
  consume(bytes: ArrayLike<number>): void;
}
interface ZmodemDetection {
  confirm(): ZmodemSession;
}
interface ZmodemSession {
  type: "send" | "receive";
  on(event: string, handler: (arg: never) => void): void;
  start(): void;
  abort(): void;
  close(): Promise<void>;
  send_offer(params: Record<string, unknown>): Promise<ZmodemTransfer | undefined>;
}
interface ZmodemTransfer {
  get_offset?(): number;
  send(bytes: ArrayLike<number>): void;
  end(bytes: ArrayLike<number>): Promise<void>;
}
interface ZmodemOffer {
  get_details(): { name: string; size?: number };
  accept(options: { on_input: (payload: number[] | Uint8Array) => void }): Promise<void>;
}
