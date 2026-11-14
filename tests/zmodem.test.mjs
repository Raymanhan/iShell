// The ZMODEM glue against the real lrzsz (`sz`/`rz`), connected by pipes as
// the terminal would connect them. Skipped where lrzsz is not installed.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { ZmodemController, formatBytes, safeFileName } from '../src/features/shell/zmodem.ts';

const haveLrzsz = spawnSync('sh', ['-c', 'command -v sz && command -v rz']).status === 0;

/** Run `args` with the controller on its other end; resolves when it exits. */
function connect(args, cwd, hostPatch) {
  const child = spawn(args[0], args.slice(1), { cwd, stdio: ['pipe', 'pipe', 'ignore'] });
  const log = { output: [], status: [], active: [] };
  const controller = new ZmodemController({
    write: async (bytes) => {
      if (!child.stdin.destroyed) child.stdin.write(bytes);
    },
    output: (bytes) => log.output.push(Buffer.from(bytes).toString('latin1')),
    status: (text) => log.status.push(text),
    pickFiles: async () => [],
    saveFile: async () => '/nowhere',
    onActiveChange: (active) => log.active.push(active),
    ...hostPatch,
  });
  child.stdout.on('data', (data) => controller.consume(new Uint8Array(data)));
  const exited = new Promise((resolve) => child.on('close', (code) => resolve(code)));
  return { controller, log, exited };
}

test('sz on the server: the file arrives intact', { skip: !haveLrzsz, timeout: 20_000 }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ishell-sz-'));
  const content = randomBytes(300_000);
  writeFileSync(join(dir, 'data.bin'), content);
  let saved = null;
  const { log, exited } = connect(['sz', '-b', 'data.bin'], dir, {
    saveFile: async (name, chunks) => {
      saved = { name, data: Buffer.concat(chunks) };
      return `/downloads/${name}`;
    },
  });
  assert.equal(await exited, 0, 'sz reports success');
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(saved?.name, 'data.bin');
  assert.ok(saved.data.equals(content), 'received bytes match');
  assert.deepEqual(log.active, [true, false]);
  assert.ok(log.status.some((line) => line.includes('已接收 data.bin') && line.includes('/downloads/data.bin')));
});

test('rz on the server: picked files are uploaded', { skip: !haveLrzsz, timeout: 20_000 }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ishell-rz-'));
  const first = randomBytes(200_000);
  const second = Buffer.from('hello\r\n~.\r\n\x18\x18\x18 zmodem');
  const files = [
    { name: 'first.bin', data: first },
    { name: 'second.txt', data: second },
  ].map(({ name, data }) => ({
    name,
    size: data.length,
    mtime: 1_700_000_000,
    read: async (offset, length) => new Uint8Array(data.subarray(offset, offset + length)),
  }));
  const { log, exited } = connect(['rz', '-b', '-E'], dir, { pickFiles: async () => files });
  assert.equal(await exited, 0, 'rz reports success');
  assert.ok(readFileSync(join(dir, 'first.bin')).equals(first));
  assert.ok(readFileSync(join(dir, 'second.txt')).equals(second));
  assert.deepEqual(log.active, [true, false]);
});

test('rz with nothing picked ends cleanly', { skip: !haveLrzsz, timeout: 20_000 }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ishell-rz-'));
  const { log, exited } = connect(['rz', '-b'], dir, {});
  await exited;
  assert.deepEqual(log.active, [true, false]);
  assert.ok(log.status.some((line) => line.includes('已取消上传')));
});

test('ordinary output passes through untouched', () => {
  const seen = [];
  const controller = new ZmodemController({
    write: async () => {},
    output: (bytes) => seen.push(Buffer.from(bytes).toString()),
    status: () => {},
    pickFiles: async () => [],
    saveFile: async () => '',
    onActiveChange: () => {},
  });
  controller.consume(new TextEncoder().encode('ls -la\r\n中文 **not zmodem**\r\n'));
  assert.equal(seen.join(''), 'ls -la\r\n中文 **not zmodem**\r\n');
  assert.equal(controller.active, false);
});

test('received names are made safe to save', () => {
  assert.equal(safeFileName('../../etc/passwd'), 'passwd');
  assert.equal(safeFileName('C:\\temp\\a<b>.txt'), 'a_b_.txt');
  assert.equal(safeFileName('..'), 'received-file');
  assert.equal(safeFileName(''), 'received-file');
  assert.equal(formatBytes(1536), '1.5 KB');
});

test('transfer start headers are kept out of the terminal', async () => {
  const { withoutStartHeaders } = await import('../src/features/shell/zmodem.ts');
  const bytes = (text) => Uint8Array.from(text, (ch) => ch.charCodeAt(0));
  const text = (data) => String.fromCharCode(...data);
  assert.equal(text(withoutStartHeaders(bytes('rz\r**\x18B00000000000000\r\x8a\x11'))), 'rz\r');
  assert.equal(text(withoutStartHeaders(bytes('a**\x18B0100000023be50\r\nb'))), 'ab');
  const utf8 = new TextEncoder().encode('中文 **');
  assert.deepEqual(withoutStartHeaders(utf8), utf8);
});
