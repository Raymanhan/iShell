import { useMemo, useState, type FormEvent } from "react";
import { CornerDownLeft, Pencil, Plus, Search, SquareCode, Trash2, Type } from "lucide-react";
import { fillSnippet, snippetVariables } from "../features/snippets/variables";
import type { Snippet, SnippetInput } from "../types";

const emptyDraft: SnippetInput = { id: null, name: "", command: "", description: "", run: true };

/** Saved commands for the side panel: click one to send it to the focused terminal. */
export function SnippetsPanel({
  snippets,
  broadcasting,
  onSend,
  onSave,
  onDelete,
}: {
  snippets: Snippet[];
  /** Broadcast input is on, so a sent snippet reaches every broadcast session. */
  broadcasting: boolean;
  onSend: (snippet: Snippet) => void;
  onSave: (input: SnippetInput) => Promise<unknown>;
  onDelete: (snippet: Snippet) => void;
}) {
  const [query, setQuery] = useState("");
  const [draft, setDraft] = useState<SnippetInput | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // A snippet with `{{variables}}` waiting for its values before it is sent.
  const [filling, setFilling] = useState<{
    snippet: Snippet;
    values: Record<string, string>;
    error: string | null;
  } | null>(null);

  function send(snippet: Snippet) {
    const variables = snippetVariables(snippet.command);
    if (!variables.length) {
      onSend(snippet);
      return;
    }
    setFilling({
      snippet,
      values: Object.fromEntries(variables.map((variable) => [variable.name, variable.defaultValue])),
      error: null,
    });
  }

  function sendFilled(event: FormEvent) {
    event.preventDefault();
    if (!filling) return;
    try {
      onSend({ ...filling.snippet, command: fillSnippet(filling.snippet.command, filling.values) });
      setFilling(null);
    } catch (fillError) {
      setFilling({ ...filling, error: fillError instanceof Error ? fillError.message : String(fillError) });
    }
  }

  const filtered = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    if (!normalized) return snippets;
    return snippets.filter((snippet) =>
      [snippet.name, snippet.command, snippet.description].some((text) => text.toLowerCase().includes(normalized)),
    );
  }, [query, snippets]);

  function edit(snippet: Snippet) {
    setError(null);
    setDraft({
      id: snippet.id,
      name: snippet.name,
      command: snippet.command,
      description: snippet.description,
      run: snippet.run,
    });
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!draft || saving) return;
    if (!draft.command.trim()) {
      setError("命令不能为空");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await onSave(draft);
      setDraft(null);
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      setSaving(false);
    }
  }

  return (
    <section className="history-panel snippets-panel" aria-label="命令片段">
      <div className="snippets-toolbar">
        <label className="history-search">
          <Search size={13} />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="搜索片段"
            spellCheck={false}
          />
        </label>
        <button
          type="button"
          className="snippets-add"
          title="新建片段"
          aria-label="新建片段"
          onClick={() => {
            setError(null);
            setDraft(emptyDraft);
          }}
        >
          <Plus size={14} />
        </button>
      </div>

      {draft && (
        <form className="snippet-editor" onSubmit={submit}>
          <input
            value={draft.name}
            placeholder="名称（可选）"
            aria-label="名称"
            onChange={(event) => setDraft({ ...draft, name: event.target.value })}
          />
          <textarea
            value={draft.command}
            placeholder={"命令，可多行；{{名称}} 或 {{名称:默认值}} 为发送时填写的变量\n例如：sudo systemctl restart {{服务:nginx}}"}
            aria-label="命令"
            rows={4}
            spellCheck={false}
            autoFocus
            onChange={(event) => setDraft({ ...draft, command: event.target.value })}
            onKeyDown={(event) => {
              if ((event.metaKey || event.ctrlKey) && event.key === "Enter") void submit(event);
            }}
          />
          <input
            value={draft.description}
            placeholder="说明（可选）"
            aria-label="说明"
            onChange={(event) => setDraft({ ...draft, description: event.target.value })}
          />
          <label className="snippet-run-toggle">
            <input
              type="checkbox"
              checked={draft.run}
              onChange={(event) => setDraft({ ...draft, run: event.target.checked })}
            />
            发送后立即执行（自动回车）
          </label>
          {error && <div className="snippet-error">{error}</div>}
          <div className="snippet-editor-actions">
            <button type="button" className="btn-ghost" onClick={() => setDraft(null)}>
              取消
            </button>
            <button type="submit" className="solid-button" disabled={saving}>
              {saving ? "保存中…" : "保存"}
            </button>
          </div>
        </form>
      )}

      {filling && (
        <form className="snippet-editor snippet-fill" onSubmit={sendFilled}>
          <strong>{filling.snippet.name}</strong>
          {Object.entries(filling.values).map(([name, value], index) => (
            <label key={name}>
              <span>{name}</span>
              <input
                value={value}
                autoFocus={index === 0}
                spellCheck={false}
                aria-label={`变量 ${name}`}
                onChange={(event) =>
                  setFilling({ ...filling, error: null, values: { ...filling.values, [name]: event.target.value } })
                }
              />
            </label>
          ))}
          <code className="snippet-preview">
            {(() => {
              try {
                return fillSnippet(filling.snippet.command, filling.values);
              } catch {
                return filling.snippet.command;
              }
            })()}
          </code>
          {filling.error && <div className="snippet-error">{filling.error}</div>}
          <div className="snippet-editor-actions">
            <button type="button" className="btn-ghost" onClick={() => setFilling(null)}>
              取消
            </button>
            <button type="submit" className="solid-button">
              {filling.snippet.run ? "发送并执行" : "发送"}
            </button>
          </div>
        </form>
      )}

      <div className="history-list snippets-list">
        {filtered.length > 0 ? (
          filtered.map((snippet) => (
            <div key={snippet.id} className="snippet-row">
              <button
                type="button"
                className="snippet-send"
                onClick={() => send(snippet)}
                title={snippet.run ? "发送并执行" : "发送到终端（不执行）"}
              >
                <span className="snippet-name">
                  {snippet.run ? <CornerDownLeft size={12} /> : <Type size={12} />}
                  {snippet.name}
                </span>
                <code>{snippet.command}</code>
                {snippet.description && <span className="snippet-description">{snippet.description}</span>}
              </button>
              <div className="snippet-actions">
                <button type="button" title="编辑" aria-label={`编辑 ${snippet.name}`} onClick={() => edit(snippet)}>
                  <Pencil size={12} />
                </button>
                <button
                  type="button"
                  title="删除"
                  aria-label={`删除 ${snippet.name}`}
                  className="danger"
                  onClick={() => onDelete(snippet)}
                >
                  <Trash2 size={12} />
                </button>
              </div>
            </div>
          ))
        ) : (
          <div className="history-empty">
            <SquareCode size={16} />
            <span>{snippets.length === 0 ? "还没有片段，点 + 保存常用命令" : "没有匹配的片段"}</span>
          </div>
        )}
      </div>

      <p className="snippets-hint">
        {broadcasting ? "广播已开启：片段会发送到所有广播会话" : "点击片段发送到当前终端"}
      </p>
    </section>
  );
}
