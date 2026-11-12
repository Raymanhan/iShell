import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');

test('the frontend command map lists exactly the commands the backend registers', () => {
  const handler = read('src-tauri/src/lib.rs').match(/generate_handler!\[([\s\S]*?)\]/)[1];
  const registered = handler
    .split(',')
    .map((entry) => entry.trim().split('::').pop())
    .filter(Boolean)
    .sort();

  const map = read('src/api/commands.ts').match(/export interface CommandMap \{([\s\S]*?)\n\}/)[1];
  const typed = [...map.matchAll(/^  ([a-z_]+): \{/gm)].map((match) => match[1]).sort();

  assert.ok(registered.length > 40, `parsed ${registered.length} registered commands`);
  assert.deepEqual(
    typed.filter((name) => !registered.includes(name)),
    [],
    'typed but not registered in lib.rs',
  );
  assert.deepEqual(
    registered.filter((name) => !typed.includes(name)),
    [],
    'registered in lib.rs but missing from src/api/commands.ts',
  );
});
