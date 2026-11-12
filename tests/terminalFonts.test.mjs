import assert from 'node:assert/strict';
import test from 'node:test';
import {
  boldWeightFor,
  buildTerminalFontFamily,
  normalizeFontSetting,
  presetsFor,
  readTerminalFontWeight,
  readTerminalTextRendering,
  wantsOpaqueTerminal,
} from '../src/features/settings/terminalFonts.ts';
import { getTerminalTheme } from '../src/constants/theme.ts';

test('the default font is the platform stack', () => {
  assert.match(buildTerminalFontFamily('', 'windows'), /^"Cascadia Mono", .*"Microsoft YaHei", monospace$/);
  assert.match(buildTerminalFontFamily('', 'macos'), /^"SF Mono", Menlo/);
});

test('a chosen font goes first, quoted, with the platform fallbacks after it', () => {
  assert.equal(
    buildTerminalFontFamily('JetBrains Mono', 'linux').split(', ').slice(0, 2).join(', '),
    '"JetBrains Mono", "DejaVu Sans Mono"',
  );
  assert.ok(buildTerminalFontFamily('Fira Code, Sarasa Mono SC', 'windows').startsWith('"Fira Code", "Sarasa Mono SC", "Cascadia Mono"'));
  assert.ok(buildTerminalFontFamily("'Iosevka', monospace", 'windows').startsWith("'Iosevka', monospace, "));
});

test('characters that could break out of the CSS value are dropped', () => {
  assert.equal(normalizeFontSetting('  Evil; } body { color: red '), 'Evil  body  color: red');
  assert.equal(normalizeFontSetting(null), '');
  assert.ok(!buildTerminalFontFamily('a"b', 'windows').startsWith('"a"b"'));
});

test('unknown saved values fall back to the defaults', () => {
  assert.equal(readTerminalFontWeight('bogus'), 'normal');
  assert.equal(readTerminalFontWeight('600'), '600');
  assert.equal(readTerminalTextRendering(null), 'auto');
  assert.equal(readTerminalTextRendering('sharp'), 'sharp');
  assert.equal(boldWeightFor('600'), '800');
  assert.equal(boldWeightFor('normal'), 'bold');
});

test('auto rendering goes opaque on Windows and macOS for every theme', () => {
  assert.equal(wantsOpaqueTerminal('auto', 'windows'), true);
  assert.equal(wantsOpaqueTerminal('auto', 'macos'), true);
  assert.equal(wantsOpaqueTerminal('auto', 'linux'), false);
  assert.equal(wantsOpaqueTerminal('sharp', 'linux'), true);
  assert.equal(wantsOpaqueTerminal('transparent', 'windows'), false);
});

test('an opaque terminal theme uses the solid surface color', () => {
  assert.equal(getTerminalTheme('liquid-glass-pearl', true).background, '#f4f0e8');
  assert.equal(getTerminalTheme('liquid-glass-pearl').background, '#f4f0e800');
});

test('each platform gets its own bundled suggestions', () => {
  const windows = presetsFor('windows').map((preset) => preset.family);
  assert.deepEqual(windows.slice(0, 2), ['Cascadia Mono', 'Consolas']);
  assert.ok(!windows.includes('Menlo'));
  assert.ok(presetsFor('macos').some((preset) => preset.family === 'Menlo'));
});
