import assert from 'node:assert/strict';
import test from 'node:test';
import { fillSnippet, snippetVariables } from '../src/features/snippets/variables.ts';

test('variables are found once each, with their defaults', () => {
  assert.deepEqual(
    snippetVariables('docker logs --tail {{lines:200}} {{容器}} && echo {{容器}} {{ lines }}'),
    [{ name: 'lines', defaultValue: '200' }, { name: '容器', defaultValue: '' }],
  );
  assert.deepEqual(snippetVariables('echo {{a}} {{a:later default}}'), [{ name: 'a', defaultValue: 'later default' }]);
  assert.deepEqual(snippetVariables('awk "{print $1}" ${HOME} {{}} {{ bad name }}'), []);
  assert.deepEqual(snippetVariables('systemctl restart nginx'), []);
});

test('filling replaces every use and keeps other braces alone', () => {
  assert.equal(
    fillSnippet('ssh {{user:root}}@{{host}} "echo {{host}}" ${PATH} {{other}}', { host: 'db-1' }),
    'ssh root@db-1 "echo db-1" ${PATH} ',
  );
  assert.equal(fillSnippet('kubectl -n {{ns:default}} get pods', { ns: 'prod' }), 'kubectl -n prod get pods');
  assert.equal(fillSnippet('echo {{x:可以有：冒号}}', {}), 'echo 可以有：冒号');
});

test('a value with a line break is refused', () => {
  assert.throws(() => fillSnippet('rm {{path}}', { path: '/tmp/x\nreboot' }), /换行/);
  assert.throws(() => fillSnippet('rm {{path}}', { path: 'a\rb' }), /换行/);
});
