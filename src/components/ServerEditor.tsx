import { useState } from "react";
import { AlertCircle, Check, Copy, Eye, EyeOff, PlugZap, X } from "lucide-react";
import { describeConnectionError } from "../features/connections/connectionError";
import { swatches } from "../constants/theme";
import type { ServerInput, ServerRecord } from "../types";
import { DEFAULT_ENCODING, TERMINAL_ENCODINGS } from "../features/shell/encodings";

import { t } from "../i18n";
export type ServerForm = ServerInput & { password: string; tagsText: string };

export function ServerEditor({
  form,
  setForm,
  saving,
  testing,
  testFeedback,
  jumpHostCandidates,
  onSave,
  onTest,
  onClose,
  onDelete,
}: {
  form: ServerForm;
  setForm: (form: ServerForm) => void;
  saving: boolean;
  testing: boolean;
  testFeedback: { kind: "info" | "success" | "error"; message: string } | null;
  /** Saved servers this one can connect through. */
  jumpHostCandidates: ServerRecord[];
  onSave: () => void;
  onTest: () => void;
  onClose: () => void;
  onDelete?: () => void;
}) {
  const [passwordVisible, setPasswordVisible] = useState(false);
  const update = <K extends keyof typeof form>(key: K, value: (typeof form)[K]) => {
    setForm({ ...form, [key]: value });
  };
  const title = form.id ? t("编辑服务器") : t("新增服务器");

  return (
    <div
      className="editor-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <form
        className="editor-sheet server-editor-sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="server-editor-title"
        onMouseDown={(event) => event.stopPropagation()}
        onSubmit={(event) => {
          event.preventDefault();
          onSave();
        }}
      >
        <header className="server-editor-head">
          <div>
            <span>SSH CONNECTION</span>
            <h2 id="server-editor-title">{title}</h2>
          </div>
          <button type="button" className="icon-button" onClick={onClose} title={t("关闭")} aria-label={t("关闭")}>
            <X size={16} />
          </button>
        </header>

        <div className="server-editor-layout">
          <div className="editor-body server-editor-main">
            <section className="editor-section">
              <div className="form-grid">
                <label className="field">
                  <span>{t("名称")}</span>
                  <input value={form.name} onChange={(event) => update("name", event.target.value)} autoFocus />
                </label>
                <label className="field username-field">
                  <span>{t("用户")}</span>
                  <input value={form.username} onChange={(event) => update("username", event.target.value)} />
                </label>
                <label className="field host-field">
                  <span>{t("主机")}</span>
                  <input value={form.host} onChange={(event) => update("host", event.target.value)} />
                </label>
                <label className="field port-field">
                  <span>{t("端口")}</span>
                  <input type="number" inputMode="numeric" min={1} max={65535} value={form.port} onChange={(event) => update("port", Number(event.target.value))} />
                </label>
              </div>
              <label className="field">
                <span>{t("跳板机")}</span>
                <select
                  value={form.jumpServerId ?? ""}
                  onChange={(event) => update("jumpServerId", event.target.value || null)}
                >
                  <option value="">{t("直接连接")}</option>
                  {jumpHostCandidates.map((server) => (
                    <option key={server.id} value={server.id}>
                      {server.name}（{server.username}@{server.host}）
                    </option>
                  ))}
                </select>
                {form.jumpServerId && (
                  <small className="field-hint">
                    {t("先连接跳板机，再从跳板机连到这台服务器；跳板机使用它自己保存的账号和密码或私钥。")}</small>
                )}
              </label>
              <label className="field">
                <span>{t("终端编码")}</span>
                <select value={form.encoding || DEFAULT_ENCODING} onChange={(event) => update("encoding", event.target.value)}>
                  {!TERMINAL_ENCODINGS.some((item) => item.value === (form.encoding || DEFAULT_ENCODING)) && (
                    <option value={form.encoding ?? ""}>{form.encoding}</option>
                  )}
                  {TERMINAL_ENCODINGS.map((item) => (
                    <option key={item.value} value={item.value}>
                      {t(item.label)}
                    </option>
                  ))}
                </select>
                {(form.encoding || DEFAULT_ENCODING) !== DEFAULT_ENCODING && (
                  <small className="field-hint">{t("终端的输出和键入都按此编码转换；文件编辑仍按 UTF-8。")}</small>
                )}
              </label>
            </section>

            <section className="editor-section">
              <span className="field-group-label">{t("认证")}</span>
              <div className="auth-switch" role="group" aria-label={t("认证方式")}>
                <button type="button" className={form.authType === "password" ? "active" : ""} aria-pressed={form.authType === "password"} onClick={() => update("authType", "password")}>
                  <span>{t("密码")}</span>
                </button>
                <button type="button" className={form.authType === "key" ? "active" : ""} aria-pressed={form.authType === "key"} onClick={() => update("authType", "key")}>
                  <span>{t("私钥")}</span>
                </button>
              </div>

              {form.authType === "key" && (
                <label className="field">
                  <span>{t("私钥路径")}</span>
                  <input value={form.keyPath ?? ""} onChange={(event) => update("keyPath", event.target.value)} />
                </label>
              )}

              <label className="field">
                <span>{form.authType === "key" ? t("私钥密码") : t("密码")}</span>
                <div className="password-input-wrap">
                  <input type={passwordVisible ? "text" : "password"} value={form.password} onChange={(event) => update("password", event.target.value)} placeholder={form.id ? t("留空则不修改") : ""} />
                  <button
                    type="button"
                    className="password-toggle"
                    onClick={() => setPasswordVisible((visible) => !visible)}
                    title={passwordVisible ? t("隐藏密码") : t("显示密码")}
                    aria-label={passwordVisible ? t("隐藏密码") : t("显示密码")}
                  >
                    {passwordVisible ? <Eye size={15} /> : <EyeOff size={15} />}
                  </button>
                </div>
              </label>
            </section>

            <section className="editor-section">
              <span className="field-group-label">{t("归档")}</span>
              <label className="field">
                <span>{t("标签")}</span>
                <input value={form.tagsText} onChange={(event) => update("tagsText", event.target.value)} />
              </label>
              <div className="swatches" role="group" aria-label={t("连接颜色")}>
                {swatches.map((color) => (
                  <button key={color} type="button" className={form.color === color ? "active" : ""} style={{ backgroundColor: color }} onClick={() => update("color", color)} aria-label={color} aria-pressed={form.color === color} />
                ))}
              </div>
            </section>

            <section className="editor-section">
              <label className="field">
                <span>{t("备注")}</span>
                <textarea value={form.notes} onChange={(event) => update("notes", event.target.value)} />
              </label>
            </section>
          </div>
        </div>

        {testFeedback?.kind === "error" && <ConnectionTestError message={testFeedback.message} />}

        <div className="editor-actions">
          {onDelete && (
            <button className="danger-button" type="button" onClick={onDelete}>
              {t("删除")}</button>
          )}
          <span
            className={`connection-test-feedback ${testFeedback ? testFeedback.kind : ""}`}
            aria-live="polite"
          >
            {testFeedback?.kind === "error" ? null : testFeedback?.message}
          </span>
          <button className="btn-ghost" type="button" onClick={onClose}>
            {t("取消")}</button>
          <button className="btn-ghost" type="button" onClick={onTest} disabled={saving || testing}>
            <PlugZap size={14} />
            {testing ? t("测试中") : t("测试连接")}
          </button>
          <button className="solid-button" type="submit" disabled={saving}>
            {saving ? t("保存中") : t("保存")}
          </button>
        </div>
      </form>
    </div>
  );
}

/** Full connection-test error: a readable reason, what to check, and the raw output. */
function ConnectionTestError({ message }: { message: string }) {
  const info = describeConnectionError(message);
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard
      ?.writeText(info.detail)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1400);
      })
      .catch(() => undefined);
  };
  return (
    <div className="connection-test-error" role="alert">
      <AlertCircle size={15} aria-hidden="true" />
      <div className="connection-test-error-body">
        <strong>{t(info.reason)}</strong>
        {info.hint && <p>{t(info.hint)}</p>}
        {info.detail && <pre>{info.detail}</pre>}
      </div>
      {info.detail && (
        <button type="button" className="icon-button" onClick={copy} title={t("复制错误信息")} aria-label={t("复制错误信息")}>
          {copied ? <Check size={13} /> : <Copy size={13} />}
        </button>
      )}
    </div>
  );
}
