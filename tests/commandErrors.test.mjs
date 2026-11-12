import assert from 'node:assert/strict';
import test from 'node:test';
import { CommandError, ErrorCode, hasErrorCode, toCommandError } from '../src/api/tauri.ts';

test('command errors keep the message for display and the code for decisions', () => {
  const conflict = toCommandError({ code: 'file_changed', message: '远程文件已被修改' });
  assert.equal(String(conflict), '远程文件已被修改');
  assert.equal(`${conflict}`, '远程文件已被修改');
  assert.ok(hasErrorCode(conflict, ErrorCode.fileChanged));

  const plain = toCommandError('找不到终端会话');
  assert.equal(String(plain), '找不到终端会话');
  assert.equal(plain.code, 'error');
  assert.ok(!hasErrorCode(plain, ErrorCode.fileChanged));

  assert.equal(String(toCommandError(42)), '42');
  assert.equal(toCommandError(conflict), conflict);
  assert.ok(conflict instanceof CommandError && conflict instanceof Error);
  assert.ok(!hasErrorCode(new Error('x'), 'error'));
});
