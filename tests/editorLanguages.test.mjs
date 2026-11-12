import assert from 'node:assert/strict';
import test from 'node:test';
import { languageForFile } from '../src/features/editor/languages.ts';

test('files open with the syntax their name implies', () => {
  const cases = {
    'deploy.sh': 'shell', '.bashrc': 'shell', 'docker-compose.yml': 'yaml', 'Cargo.toml': 'toml',
    '.env': 'ini', '.env.production': 'ini', 'app.service': 'ini', 'my.cnf': 'ini',
    'Dockerfile': 'dockerfile', 'Dockerfile.prod': 'dockerfile', 'api.dockerfile': 'dockerfile',
    'package.json': 'json', 'main.py': 'python', 'schema.sql': 'sql', 'index.ts': 'typescript',
    'fix.patch': 'diff', 'notes.txt': 'plain', 'README': 'plain', 'nginx.conf': 'nginx',
  };
  for (const [name, language] of Object.entries(cases)) {
    assert.equal(languageForFile(name), language, name);
  }
});

test('nginx site files are recognized by where they live', () => {
  assert.equal(languageForFile('example.com', '/etc/nginx/sites-available/example.com'), 'nginx');
  assert.equal(languageForFile('default.conf', '/etc/nginx/conf.d/default.conf'), 'nginx');
  assert.equal(languageForFile('app.conf', '/etc/supervisor/conf.d/app.conf'), 'ini');
});
