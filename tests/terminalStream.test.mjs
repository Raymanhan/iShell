import assert from 'node:assert/strict';
import test from 'node:test';
import { FRAME_CLOSED, FRAME_DATA, FRAME_READY, TerminalOutput, parseFrame } from '../src/features/shell/terminalStream.ts';

function frame(kind, offset, payload = new Uint8Array()) {
  const bytes = new Uint8Array(9 + payload.length);
  bytes[0] = kind;
  new DataView(bytes.buffer).setBigUint64(1, BigInt(offset), true);
  bytes.set(payload, 9);
  return bytes.buffer;
}

test('frames carry a kind, a little-endian offset and a payload', () => {
  const data = parseFrame(frame(FRAME_DATA, 2 ** 33 + 7, new TextEncoder().encode('hi')));
  assert.equal(data.kind, FRAME_DATA);
  assert.equal(data.offset, 2 ** 33 + 7);
  assert.equal(new TextDecoder().decode(data.payload), 'hi');
  assert.equal(parseFrame(frame(FRAME_READY, 5)).payload.length, 0);
  const closed = parseFrame(frame(FRAME_CLOSED, 5, new TextEncoder().encode('远程终端已关闭')));
  assert.equal(new TextDecoder().decode(closed.payload), '远程终端已关闭');
  assert.equal(parseFrame(new ArrayBuffer(4)), null);
});

test('characters split across frames decode once both halves arrive', () => {
  const output = new TerminalOutput();
  const bytes = new TextEncoder().encode('中文ok');
  assert.equal(output.push(0, bytes.subarray(0, 2)), '');
  assert.equal(output.push(2, bytes.subarray(2, 4)), '中');
  assert.equal(output.push(4, bytes.subarray(4)), '文ok');
  assert.equal(output.offset, bytes.length);
});

test('replayed output is taken only once, even when it overlaps', () => {
  const output = new TerminalOutput();
  const all = new TextEncoder().encode('hello world');
  assert.equal(output.push(0, all.subarray(0, 6)), 'hello ');
  // A pane re-attaching gets the whole buffer again.
  assert.equal(output.push(0, all), 'world');
  assert.equal(output.push(0, all), '');
  assert.equal(output.push(6, all.subarray(6)), '');
});

test('a gap restarts decoding at the frame that follows it', () => {
  const output = new TerminalOutput();
  const bytes = new TextEncoder().encode('中');
  assert.equal(output.push(0, bytes.subarray(0, 1)), '');
  assert.equal(output.push(100, new TextEncoder().encode('after')), 'after', 'the incomplete character before the gap is dropped');
  assert.equal(output.offset, 105);
});

test('other encodings decode as chosen', () => {
  const output = new TerminalOutput('gbk');
  // "中文" in GBK, split inside the second character.
  assert.equal(output.push(0, new Uint8Array([0xd6, 0xd0, 0xce])), '中');
  assert.equal(output.push(3, new Uint8Array([0xc4])), '文');
});
