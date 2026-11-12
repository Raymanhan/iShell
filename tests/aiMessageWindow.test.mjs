import assert from 'node:assert/strict';
import test from 'node:test';
import {
  PAGE,
  clampHidden,
  initialHidden,
  showEarlier,
  trimWhileFollowing,
} from '../src/features/ai/messageWindow.ts';

test('a conversation opens on its latest page', () => {
  assert.equal(initialHidden(0), 0);
  assert.equal(initialHidden(PAGE), 0);
  assert.equal(initialHidden(PAGE * 3 + 5), PAGE * 2 + 5);
});

test('scrolling up loads one page at a time until everything is shown', () => {
  let hidden = initialHidden(PAGE * 2 + 7);
  const pages = [];
  while (hidden > 0) {
    hidden = showEarlier(hidden);
    pages.push(hidden);
  }
  assert.deepEqual(pages, [7, 0]);
});

test('new blocks never hide older ones, and a shorter conversation keeps a page', () => {
  // Growing: the user loaded everything, more replies arrive.
  assert.equal(clampHidden(0, PAGE * 5), 0);
  assert.equal(clampHidden(10, PAGE * 5), 10);
  // Shrinking after editing an earlier message.
  assert.equal(clampHidden(PAGE * 3, PAGE + 4), 4);
  assert.equal(clampHidden(PAGE * 3, 3), 0);
});

test('following a long task folds back to the latest page', () => {
  assert.equal(trimWhileFollowing(0, PAGE * 2), 0);
  assert.equal(trimWhileFollowing(0, PAGE * 2 + 1), PAGE + 1);
  assert.equal(trimWhileFollowing(50, 50 + PAGE * 2), 50);
});
