// ZMODEM over a real SSH session, set up as the OpenSSH terminal is: the
// controller talks to a local PTY running `ssh -e none -tt`, and lrzsz runs on
// the server. Needs the live test servers (scripts/live-ssh-servers.sh) and
// python3 for the PTY; skipped otherwise.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import test from 'node:test';
import { ZmodemController } from '../src/features/shell/zmodem.ts';

const target = process.env.ISHELL_LIVE_SSH;
const user = process.env.ISHELL_LIVE_USER;
const key = process.env.ISHELL_LIVE_KEY;
const ready =
  Boolean(target && user && key) &&
  spawnSync('sh', ['-c', 'command -v python3 && command -v ssh']).status === 0;
const [host, port] = (target ?? ':').split(':');
const sshOptions = [
  '-i', key ?? '', '-p', port || '22',
  '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=no', '-o', 'UserKnownHostsFile=/dev/null',
  '-o', 'LogLevel=ERROR',
];

function remote(command, input) {
  const result = spawnSync('ssh', [...sshOptions, `${user}@${host}`, command], { input });
  assert.equal(result.status, 0, `remote ${command}: ${result.stderr}`);
  return result.stdout.toString();
}

const sha256 = (data) => createHash('sha256').update(data).digest('hex');

/** An interactive `ssh -tt` in a local PTY, driven by a ZMODEM controller. */
function terminal(command, hostPatch) {
  const child = spawn(
    'python3',
    ['-c', 'import pty, sys; pty.spawn(sys.argv[1:])', 'ssh', '-e', 'none', '-tt', ...sshOptions, `${user}@${host}`, command],
    { stdio: ['pipe', 'pipe', 'inherit'] },
  );
  const log = { status: [], active: [] };
  const shown = [];
  const controller = new ZmodemController({
    write: async (bytes) => {
      if (!child.stdin.destroyed) child.stdin.write(bytes);
    },
    output: (bytes) => shown.push(Buffer.from(bytes).toString('latin1')),
    status: (text) => log.status.push(text),
    pickFiles: async () => [],
    saveFile: async () => '',
    onActiveChange: (active) => log.active.push(active),
    ...hostPatch,
  });
  child.stdout.on('data', (data) => controller.consume(new Uint8Array(data)));
  const exited = new Promise((resolve) => child.on('close', resolve));
  return { log, exited, child, shown };
}

test('sz over SSH: the server sends a file', { skip: !ready, timeout: 30_000 }, async () => {
  const content = randomBytes(1_500_000);
  const name = `ishell-zm-${Date.now()}.bin`;
  remote(`cat > /tmp/${name}`, content);
  let saved = null;
  const { log, exited, shown } = terminal(`cd /tmp && sz -b ${name}; rm -f ${name}; echo after-sz`, {
    saveFile: async (fileName, chunks) => {
      saved = { fileName, data: Buffer.concat(chunks) };
      return `/downloads/${fileName}`;
    },
  });
  await exited;
  assert.equal(saved?.fileName, name);
  assert.equal(sha256(saved.data), sha256(content), 'the received file matches');
  assert.deepEqual(log.active, [true, false]);
  assert.ok(!log.status.some((line) => line.includes('出错')), log.status.join('\n'));
  assert.ok(shown.join('').includes('after-sz'), 'output after the transfer reaches the terminal');
  assert.ok(!shown.join('').includes('\x18B0'), 'no ZMODEM header text is shown');
});

test('rz over SSH: picked files are uploaded', { skip: !ready, timeout: 30_000 }, async () => {
  const dir = `/tmp/ishell-rz-${Date.now()}`;
  remote(`mkdir -p ${dir}`);
  const content = randomBytes(1_200_000);
  const file = {
    name: 'upload.bin',
    size: content.length,
    mtime: null,
    read: async (offset, length) => new Uint8Array(content.subarray(offset, offset + length)),
  };
  const { log, exited } = terminal(`cd ${dir} && rz -b -E`, { pickFiles: async () => [file] });
  await exited;
  const remoteHash = remote(`sha256sum ${dir}/upload.bin && rm -rf ${dir}`).split(' ')[0];
  assert.equal(remoteHash, sha256(content), 'the uploaded file matches');
  assert.deepEqual(log.active, [true, false]);
  assert.ok(log.status.some((line) => line.includes('已发送 upload.bin')));
});
