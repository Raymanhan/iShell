import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';

// CI and release builds run `npm ci` on GitHub's servers. Packages pinned to a
// regional mirror (such as registry.npmmirror.com) fail there whenever the
// mirror is slow, so every tarball must come from the official registry. A
// local mirror set in .npmrc is still used: npm swaps the npmjs.org host for
// the configured registry when installing.
test('package-lock.json downloads every package from registry.npmjs.org', () => {
  const lock = JSON.parse(fs.readFileSync(new URL('../package-lock.json', import.meta.url), 'utf8'));
  const elsewhere = Object.entries(lock.packages)
    .filter(([, entry]) => entry.resolved && !entry.resolved.startsWith('https://registry.npmjs.org/'))
    .map(([name, entry]) => `${name}: ${entry.resolved}`);
  assert.deepEqual(elsewhere, []);
});
