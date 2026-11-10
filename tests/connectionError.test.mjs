import assert from 'node:assert/strict';
import test from 'node:test';
import { describeConnectionError } from '../src/features/connections/connectionError.ts';

test('OpenSSH password failure is reported as an authentication problem', () => {
  const info = describeConnectionError(
    '测试连接失败：ubuntu@81.70.52.10: Permission denied (publickey,password).',
  );
  assert.equal(info.reason, '认证失败');
  assert.ok(info.hint);
  assert.equal(info.detail, 'ubuntu@81.70.52.10: Permission denied (publickey,password).');
});

test('russh messages are classified too', () => {
  assert.equal(describeConnectionError('测试连接失败：密码认证被拒绝（用户名或密码错误）').reason, '认证失败');
  assert.equal(describeConnectionError('测试连接失败：无法连接 1.2.3.4:22：Connection refused (os error 61)').reason, '连接被拒绝');
  assert.equal(describeConnectionError('测试连接失败：密钥认证超时（15 秒）').reason, '连接超时');
});

test('network failures map to their causes', () => {
  assert.equal(
    describeConnectionError('测试连接失败：ssh: connect to host 10.0.0.9 port 22: Operation timed out').reason,
    '连接超时',
  );
  assert.equal(
    describeConnectionError('测试连接失败：ssh: Could not resolve hostname bad.example: nodename nor servname provided').reason,
    '无法解析主机名',
  );
  assert.equal(
    describeConnectionError('测试连接失败：@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@').reason,
    '主机密钥校验失败',
  );
});

test('unknown errors keep the full text and no hint', () => {
  const info = describeConnectionError('测试连接失败：something odd');
  assert.equal(info.reason, '测试连接失败');
  assert.equal(info.hint, null);
  assert.equal(info.detail, 'something odd');
});

test('harmless OpenSSH notices are dropped from the detail', () => {
  const info = describeConnectionError(
    "测试连接失败：Warning: Permanently added '1.2.3.4' (ED25519) to the list of known hosts.\r\nubuntu@1.2.3.4: Permission denied (password).",
  );
  assert.equal(info.detail, 'ubuntu@1.2.3.4: Permission denied (password).');
});
