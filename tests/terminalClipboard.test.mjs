import assert from "node:assert/strict";
import test from "node:test";
import { createClipboardPasteHandler, imagePasteData, isTerminalPasteShortcut } from "../src/features/shell/clipboard.ts";

const key = (extra) => ({ type: "keydown", key: "v", ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...extra });

test("paste shortcuts preserve Unix literal-next and Ctrl+C", () => {
  for (const windows of [false, true]) {
    assert.equal(isTerminalPasteShortcut(key({ metaKey: true }), windows), true);
    assert.equal(isTerminalPasteShortcut(key({ ctrlKey: true, shiftKey: true }), windows), true);
    assert.equal(isTerminalPasteShortcut(key({ key: "Insert", shiftKey: true }), windows), true);
    assert.equal(isTerminalPasteShortcut(key({ key: "c", ctrlKey: true }), windows), false);
    assert.equal(isTerminalPasteShortcut(key({ type: "keyup", metaKey: true }), windows), false);
    assert.equal(isTerminalPasteShortcut(key({ metaKey: true, altKey: true }), windows), false);
  }
  assert.equal(isTerminalPasteShortcut(key({ ctrlKey: true }), false), false);
  assert.equal(isTerminalPasteShortcut(key({ ctrlKey: true }), true), true);
});

function deferred() {
  let resolve, reject;
  const promise = new Promise((a, b) => { resolve = a; reject = b; });
  return { promise, resolve, reject };
}

function harness() {
  const request = deferred();
  const texts = [], images = [], notices = [], reads = [];
  let session = { sessionId: "session-a", serverId: "server-a" };
  let disposed = false;
  const paste = createClipboardPasteHandler({
    getSession: () => session,
    isDisposed: () => disposed,
    readClipboard: (target) => { reads.push(target); return request.promise; },
    pasteText: (text) => texts.push(text),
    pasteImage: (path) => images.push(path),
    setNotice: (text) => notices.push(text),
  });
  return { paste, request, texts, images, notices, reads,
    setSession: (value) => { session = value; },
    dispose: () => { disposed = true; },
  };
}

test("screenshots target the original session and use only the image paste callback", async () => {
  const h = harness();
  const pending = h.paste();
  assert.deepEqual(h.reads, [{ sessionId: "session-a", serverId: "server-a" }]);
  assert.deepEqual(h.images, []);
  h.request.resolve({ kind: "image", content: "/tmp/ishell-clipboard-0123456789/screenshot.png" });
  await pending;
  assert.deepEqual(h.images, ["/tmp/ishell-clipboard-0123456789/screenshot.png"]);
  assert.deepEqual(h.texts, []);
});

test("ordinary multiline clipboard text retains the text paste path", async () => {
  const h = harness();
  const pending = h.paste();
  h.request.resolve({ kind: "text", content: "echo 中文\nls\n" });
  await pending;
  assert.deepEqual(h.texts, ["echo 中文\nls\n"]);
  assert.deepEqual(h.images, []);
});

test("reconnect, server changes, and disconnect during upload never paste into another session", async () => {
  for (const session of [null, { sessionId: "new-session", serverId: "server-a" }, { sessionId: "session-a", serverId: "other-server" }]) {
    const h = harness();
    const pending = h.paste();
    h.setSession(session);
    h.request.resolve({ kind: "image", content: "/tmp/uploaded.png" });
    await pending;
    assert.deepEqual(h.images, []);
    assert.deepEqual(h.texts, []);
    assert.match(h.notices.at(-1), /\/tmp\/uploaded.png/);
  }
});

test("duplicate paste is rejected while uploading and failure releases the busy state", async () => {
  const h = harness();
  const pending = h.paste();
  await h.paste();
  assert.equal(h.reads.length, 1);
  h.request.reject(new Error("upload failed"));
  await pending;
  assert.match(h.notices.at(-1), /upload failed/);
  assert.deepEqual(h.images, []);
  await h.paste();
  assert.equal(h.reads.length, 2);
});

test("closed panes, empty clipboards, and disconnected panes never insert input", async () => {
  const h = harness();
  const pending = h.paste();
  h.dispose();
  const count = h.notices.length;
  h.request.resolve({ kind: "image", content: "/tmp/uploaded.png" });
  await pending;
  assert.equal(h.notices.length, count);
  assert.deepEqual(h.images, []);
  const empty = harness();
  empty.request.resolve({ kind: "empty" });
  await empty.paste();
  assert.deepEqual(empty.texts, []);
  assert.deepEqual(empty.images, []);
  const disconnected = harness();
  disconnected.setSession(null);
  await disconnected.paste();
  assert.deepEqual(disconnected.reads, []);
});

test("image paths are quoted, respect bracketed paste, and never submit a command", () => {
  assert.equal(imagePasteData("/tmp/a b's.png", false), "'/tmp/a b'\\''s.png' ");
  assert.equal(imagePasteData("/tmp/screenshot.png", true), "\x1b[200~'/tmp/screenshot.png' \x1b[201~");
  assert.doesNotMatch(imagePasteData("/tmp/screenshot.png", false), /[\r\n]/);
});
