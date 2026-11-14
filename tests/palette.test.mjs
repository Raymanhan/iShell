import assert from 'node:assert/strict';
import test from 'node:test';
import { entryScore, isPaletteShortcut, rankEntries, termScore } from '../src/features/palette/match.ts';

const entries = [
  { id: 'a', title: '连接：web-prod', detail: 'deploy@10.0.0.1', keywords: 'ssh connect' },
  { id: 'b', title: '连接：db-prod', detail: 'root@10.0.0.2', keywords: 'ssh connect' },
  { id: 'c', title: '打开设置', keywords: 'settings preferences' },
  { id: 'd', title: '本地终端', keywords: 'local shell terminal' },
];

test('substrings beat scattered letters, word starts beat the middle', () => {
  assert.ok(termScore('web', 'web-prod') > termScore('wbp', 'web-prod'));
  assert.ok(termScore('prod', 'web prod') > termScore('rod', 'web prod'));
  assert.equal(termScore('xyz', 'web-prod'), null);
  assert.equal(termScore('', 'anything'), 0);
});

test('every term must match, in the title or the details', () => {
  assert.deepEqual(rankEntries(entries, 'prod').map((e) => e.id).sort(), ['a', 'b']);
  assert.deepEqual(rankEntries(entries, 'root').map((e) => e.id), ['b']);
  assert.deepEqual(rankEntries(entries, 'ssh db').map((e) => e.id), ['b']);
  assert.deepEqual(rankEntries(entries, '设置').map((e) => e.id), ['c']);
  assert.deepEqual(rankEntries(entries, 'local').map((e) => e.id), ['d']);
  assert.equal(entryScore(entries[0], 'nothing-here'), null);
  assert.equal(rankEntries(entries, '  ').length, 4, 'an empty query lists everything');
});

test('a title match ranks above a match only in the details', () => {
  const ranked = rankEntries(
    [
      { id: 'detail', title: 'Alpha', detail: 'term' },
      { id: 'title', title: 'term host' },
    ],
    'term',
  );
  assert.deepEqual(ranked.map((e) => e.id), ['title', 'detail']);
});

const key = (patch) => ({ type: 'keydown', key: 'k', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...patch });

test('the shortcut leaves Ctrl+K to the shell', () => {
  assert.ok(isPaletteShortcut(key({ metaKey: true }), true));
  assert.ok(isPaletteShortcut(key({ metaKey: true, shiftKey: true, key: 'P' }), true));
  assert.ok(!isPaletteShortcut(key({ ctrlKey: true }), false));
  assert.ok(isPaletteShortcut(key({ ctrlKey: true, shiftKey: true, key: 'P' }), false));
  assert.ok(!isPaletteShortcut(key({ ctrlKey: true, shiftKey: true, key: 'P', type: 'keyup' }), false));
  assert.ok(!isPaletteShortcut(key({ metaKey: true, ctrlKey: true }), true));
});
