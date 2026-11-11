import { useEffect, useRef } from "react";
import { Save, X } from "lucide-react";
import { createCodeEditor, loadLanguage } from "../features/editor/codeEditor";
import { languageForFile } from "../features/editor/languages";
import type { SftpEntry } from "../types";
import { formatBytes } from "../utils/format";

export interface FileEditorState {
  entry: SftpEntry;
  tabId: string;
  serverId: string;
  content: string;
  originalContent: string;
  loading: boolean;
  saving: boolean;
  error?: string;
  /** The remote revision the content was read from; saves are checked against it. */
  revision?: string;
  /** The last save was refused because the remote file changed meanwhile. */
  conflict?: boolean;
}

/** Modal editor for small remote text files. */
export function FileEditorModal({
  editor,
  onChange,
  onSave,
  onOverwrite,
  onClose,
}: {
  editor: FileEditorState;
  onChange: (content: string) => void;
  onSave: () => void;
  /** Save even though the remote file changed since it was opened. */
  onOverwrite: () => void;
  onClose: () => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const codeEditorRef = useRef<ReturnType<typeof createCodeEditor> | null>(null);
  const handlers = useRef({ onChange, onSave });
  handlers.current = { onChange, onSave };

  // The editor is created once the file has loaded, with its text.
  const ready = !editor.loading && !(editor.error && !editor.originalContent && !editor.content);
  useEffect(() => {
    const host = hostRef.current;
    if (!ready || !host) return;
    const codeEditor = createCodeEditor(host, {
      content: editor.content,
      readOnly: editor.saving,
      onChange: (content) => handlers.current.onChange(content),
      onSave: () => handlers.current.onSave(),
    });
    codeEditorRef.current = codeEditor;
    codeEditor.view.focus();
    let active = true;
    loadLanguage(languageForFile(editor.entry.name, editor.entry.path))
      .then((language) => active && codeEditor.setLanguage(language))
      .catch(() => undefined);
    return () => {
      active = false;
      codeEditorRef.current = null;
      codeEditor.destroy();
    };
    // Recreated only for another file; later text changes come from the editor itself.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, editor.entry.path, editor.tabId]);

  useEffect(() => {
    codeEditorRef.current?.setReadOnly(editor.saving);
  }, [editor.saving]);

  return (
    <div className="file-editor-backdrop" role="presentation">
      <section className="file-editor" role="dialog" aria-modal="true" aria-labelledby="file-editor-title">
        <header className="file-editor-head">
          <div>
            <span>Text Editor</span>
            <h2 id="file-editor-title">{editor.entry.name}</h2>
            <p title={editor.entry.path}>{editor.entry.path}</p>
          </div>
          <button type="button" className="icon-button" onClick={onClose} title="关闭">
            <X size={16} />
          </button>
        </header>
        {editor.error && <div className="file-editor-error">{editor.error}</div>}
        <div className="file-editor-code" ref={hostRef} aria-busy={editor.loading}>
          {editor.loading && <div className="file-editor-loading">读取中…</div>}
        </div>
        <footer className="file-editor-actions">
          <span>{editor.loading ? "读取中…" : `${formatBytes(new TextEncoder().encode(editor.content).length)} / 1 MB`}</span>
          <div>
            <button type="button" className="btn-ghost" disabled={editor.saving} onClick={onClose}>
              取消
            </button>
            {editor.conflict && (
              <button
                type="button"
                className="btn-ghost"
                disabled={editor.saving}
                title="远程文件在打开后被修改过，覆盖会丢弃那些修改"
                onClick={onOverwrite}
              >
                仍然覆盖
              </button>
            )}
            <button
              type="button"
              className="solid-button"
              disabled={editor.loading || editor.saving || editor.content === editor.originalContent}
              onClick={onSave}
            >
              <Save size={15} />
              {editor.saving ? "保存中…" : "保存"}
            </button>
          </div>
        </footer>
      </section>
    </div>
  );
}
