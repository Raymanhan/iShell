import assert from 'node:assert/strict';
import test from 'node:test';
import { TerminalOutput } from '../src/features/shell/terminalStream.ts';
import { TERMINAL_ENCODINGS, encodingLabel } from '../src/features/shell/encodings.ts';

test('output is decoded with the session encoding and can switch midway', () => {
  const output = new TerminalOutput('GBK');
  assert.equal(output.encoding, 'gbk');
  assert.equal(output.push(0, Uint8Array.from([0xc4, 0xe3, 0xba])), '你');
  // The half character left over is flushed when switching.
  assert.equal(output.setEncoding('UTF-8'), '�');
  assert.equal(output.push(3, new TextEncoder().encode('好')), '好');
  assert.equal(output.setEncoding('utf8'), '', 'the same encoding is no switch');
});

test('every offered encoding is one the terminal can decode', () => {
  for (const { value } of TERMINAL_ENCODINGS) {
    assert.doesNotThrow(() => new TextDecoder(value), value);
  }
  assert.equal(encodingLabel('gbk'), 'GBK（简体中文）');
  assert.equal(encodingLabel(undefined), 'UTF-8');
  assert.equal(encodingLabel('x-mac-cyrillic'), 'x-mac-cyrillic');
});
