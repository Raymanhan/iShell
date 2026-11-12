import assert from 'node:assert/strict';
import test from 'node:test';
import {
  applyAiEvent,
  applyQueuedText,
  queueText,
} from '../src/features/ai/types.ts';

const events = [
  ['a1', { type: 'thinking', text: '先看' }],
  ['a1', { type: 'thinking', text: '磁盘' }],
  ['a1', { type: 'text', text: '磁盘' }],
  ['a1', { type: 'text', text: '使用 42%' }],
  ['a1', { type: 'thinking', text: '再看内存' }],
  ['a2', { type: 'text', text: '下一轮' }],
  ['a2', { type: 'text', text: '回复' }],
];

test('batched deltas give exactly what applying each one would', () => {
  const start = [{ id: 'u1', kind: 'user', text: '检查服务器' }];
  const oneByOne = events.reduce(
    (entries, [id, event]) => applyAiEvent(entries, event, id),
    start,
  );
  const queue = events.reduce(
    (queued, [id, event]) => queueText(queued, event, id),
    [],
  );
  assert.deepEqual(applyQueuedText(start, queue), oneByOne);
  assert.equal(oneByOne[1].thinking, '先看磁盘再看内存');
  assert.equal(oneByOne[1].text, '磁盘使用 42%');
  assert.equal(oneByOne[2].text, '下一轮回复');
});

test('consecutive deltas of one reply join into one queued item', () => {
  const queue = events.reduce(
    (queued, [id, event]) => queueText(queued, event, id),
    [],
  );
  assert.deepEqual(
    queue.map(({ assistantId, event }) => [assistantId, event.type, event.text]),
    [
      ['a1', 'thinking', '先看磁盘'],
      ['a1', 'text', '磁盘使用 42%'],
      ['a1', 'thinking', '再看内存'],
      ['a2', 'text', '下一轮回复'],
    ],
  );
});

test('an empty queue changes nothing', () => {
  const entries = [{ id: 'u1', kind: 'user', text: 'hi' }];
  assert.equal(applyQueuedText(entries, []), entries);
});
