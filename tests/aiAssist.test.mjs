import assert from 'node:assert/strict';
import test from 'node:test';
import {
  cleanSuggestedCommand,
  isAskAiShortcut,
  isGenerateCommandShortcut,
  naturalLanguageRequest,
  requestFromPromptLine,
  selectionPrompt,
} from '../src/features/shell/aiAssist.ts';

const key = (patch) => ({ type: 'keydown', key: 'a', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...patch });

test('shortcuts use the platform modifier and leave plain keys alone', () => {
  assert.ok(isAskAiShortcut(key({ ctrlKey: true, shiftKey: true, key: 'A' }), false));
  assert.ok(isAskAiShortcut(key({ metaKey: true, shiftKey: true }), true));
  assert.ok(!isAskAiShortcut(key({ ctrlKey: true }), false), 'Ctrl+A still goes to the shell');
  assert.ok(!isAskAiShortcut(key({ ctrlKey: true, shiftKey: true }), true));
  assert.ok(!isAskAiShortcut(key({ ctrlKey: true, shiftKey: true, type: 'keyup' }), false));
  assert.ok(isGenerateCommandShortcut(key({ ctrlKey: true, key: 'Enter' }), false));
  assert.ok(!isGenerateCommandShortcut(key({ key: 'Enter' }), false), 'plain Enter runs the line');
  assert.ok(!isGenerateCommandShortcut(key({ ctrlKey: true, shiftKey: true, key: 'Enter' }), false));
});

test('only a # line is a request', () => {
  assert.equal(naturalLanguageRequest('# 查看占用 8080 端口的进程 '), '查看占用 8080 端口的进程');
  assert.equal(naturalLanguageRequest('#find big files'), 'find big files');
  assert.equal(naturalLanguageRequest('#'), null);
  assert.equal(naturalLanguageRequest('ls # comment'), null);
  assert.equal(naturalLanguageRequest(''), null);
});

test('a suggested command becomes one line that cannot submit itself', () => {
  assert.equal(cleanSuggestedCommand('```bash\nss -ltnp | grep :8080\n```'), 'ss -ltnp | grep :8080');
  assert.equal(cleanSuggestedCommand('$ du -sh * | sort -h\n'), 'du -sh * | sort -h');
  assert.equal(cleanSuggestedCommand('ls\nrm -rf /'), 'ls', 'only the first line');
  assert.equal(cleanSuggestedCommand('echo a\rb\x1b[2J\x03'), 'echo a');
  assert.equal(cleanSuggestedCommand('echo a\x1b[2J\x03 b'), 'echo a [2J  b');
  assert.equal(cleanSuggestedCommand('   '), '');
});

test('selections are quoted without breaking out of the fence', () => {
  assert.equal(selectionPrompt('error: x\n\n'), '解释下面这段终端输出，指出问题原因并给出下一步建议：\n```\nerror: x\n```');
  assert.ok(selectionPrompt('see ```code```').includes('~~~\nsee ```code```\n~~~'));
  assert.ok(selectionPrompt('x'.repeat(20_000)).length < 12_200);
});

test('the request is read from the prompt line, root prompts included', () => {
  assert.equal(requestFromPromptLine('deploy@web:~$ # 查看占用 8080 端口的进程'), '查看占用 8080 端口的进程');
  assert.equal(requestFromPromptLine('root@web:~# # restart nginx '), 'restart nginx');
  assert.equal(requestFromPromptLine('% #list big files'), 'list big files');
  assert.equal(requestFromPromptLine('root@web:~# ls -la'), null, 'a root prompt alone is not a request');
  assert.equal(requestFromPromptLine('deploy@web:~$ grep "#x" a # note'), null);
  assert.equal(requestFromPromptLine('root@web:~# # '), null);
});
