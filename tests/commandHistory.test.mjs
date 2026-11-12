import assert from 'node:assert/strict';
import test from 'node:test';
import { COMMAND_HISTORY_LIMIT, withCommand } from '../src/features/history/history.ts';

test('the newest command comes first and the list stays bounded', () => {
  const history = withCommand(['ls', 'pwd'], 'uptime');
  assert.deepEqual(history, ['uptime', 'ls', 'pwd']);
  const full = Array.from({ length: COMMAND_HISTORY_LIMIT }, (_, i) => `cmd ${i}`);
  const next = withCommand(full, 'newest');
  assert.equal(next.length, COMMAND_HISTORY_LIMIT);
  assert.equal(next[0], 'newest');
  assert.equal(next.at(-1), `cmd ${COMMAND_HISTORY_LIMIT - 2}`);
});
