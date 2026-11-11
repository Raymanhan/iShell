/**
 * Terminal output as the backend sends it: binary frames on a per-pane IPC
 * channel (see `src-tauri/src/terminal.rs`). A frame is one byte of kind,
 * eight bytes of little-endian offset, then the payload.
 */

export const FRAME_DATA = 0;
export const FRAME_READY = 1;
export const FRAME_CLOSED = 2;

export interface TerminalFrame {
  kind: number;
  /** For data, where the bytes start in the session's output. */
  offset: number;
  payload: Uint8Array;
}

export function parseFrame(buffer: ArrayBuffer): TerminalFrame | null {
  if (buffer.byteLength < 9) return null;
  const view = new DataView(buffer);
  return {
    kind: view.getUint8(0),
    // Offsets stay far below 2^53 bytes of output.
    offset: Number(view.getBigUint64(1, true)),
    payload: new Uint8Array(buffer, 9),
  };
}

/**
 * The decoded text of one session's output. Bytes are taken only once, in
 * order, however often the same output is replayed (a pane that attaches
 * again gets the buffer again), and a character split across frames is
 * decoded once both halves have arrived.
 */
export class TerminalOutput {
  private decoder: TextDecoder;
  private end = 0;

  constructor(encoding = "utf-8") {
    this.decoder = new TextDecoder(encoding, { fatal: false });
  }

  /** The encoding output is decoded with, as `TextDecoder` names it. */
  get encoding(): string {
    return this.decoder.encoding;
  }

  /**
   * Decode further output with another encoding. Returns what the old one
   * still held (a character cut off mid-way), for showing before the rest.
   */
  setEncoding(encoding: string): string {
    const next = new TextDecoder(encoding, { fatal: false });
    if (next.encoding === this.decoder.encoding) return "";
    const rest = this.decoder.decode();
    this.decoder = next;
    return rest;
  }

  /** Bytes taken so far, as an offset into the session's output. */
  get offset(): number {
    return this.end;
  }

  /**
   * Take a data frame and return the text it adds, or "" when it holds
   * nothing new. Output older than what is kept (the backend trims its
   * buffer) starts the stream at the frame's offset.
   */
  push(offset: number, bytes: Uint8Array): string {
    const frameEnd = offset + bytes.length;
    if (frameEnd <= this.end) return "";
    if (offset > this.end) {
      // A gap: what was missed is gone. Start afresh where this frame begins.
      this.decoder.decode();
      this.end = offset;
    }
    const fresh = bytes.subarray(this.end - offset);
    this.end = frameEnd;
    return this.decoder.decode(fresh, { stream: true });
  }
}
