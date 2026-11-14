import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import {
  bracketMatching,
  HighlightStyle,
  indentOnInput,
  StreamLanguage,
  syntaxHighlighting,
  type StreamParser,
} from "@codemirror/language";
import { highlightSelectionMatches, search, searchKeymap } from "@codemirror/search";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import {
  crosshairCursor,
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";
import type { EditorLanguage } from "./languages";
import { t } from "../../i18n";

/** Each mode is fetched only when a file in that language is opened. */
const modes: Record<Exclude<EditorLanguage, "plain">, () => Promise<StreamParser<unknown>>> = {
  shell: () => import("@codemirror/legacy-modes/mode/shell").then((m) => m.shell),
  yaml: () => import("@codemirror/legacy-modes/mode/yaml").then((m) => m.yaml),
  toml: () => import("@codemirror/legacy-modes/mode/toml").then((m) => m.toml),
  ini: () => import("@codemirror/legacy-modes/mode/properties").then((m) => m.properties),
  nginx: () => import("@codemirror/legacy-modes/mode/nginx").then((m) => m.nginx),
  dockerfile: () => import("@codemirror/legacy-modes/mode/dockerfile").then((m) => m.dockerFile),
  json: () => import("@codemirror/legacy-modes/mode/javascript").then((m) => m.json),
  javascript: () => import("@codemirror/legacy-modes/mode/javascript").then((m) => m.javascript),
  typescript: () => import("@codemirror/legacy-modes/mode/javascript").then((m) => m.typescript),
  python: () => import("@codemirror/legacy-modes/mode/python").then((m) => m.python),
  sql: () => import("@codemirror/legacy-modes/mode/sql").then((m) => m.standardSQL),
  css: () => import("@codemirror/legacy-modes/mode/css").then((m) => m.css),
  xml: () => import("@codemirror/legacy-modes/mode/xml").then((m) => m.xml),
  html: () => import("@codemirror/legacy-modes/mode/xml").then((m) => m.html),
  go: () => import("@codemirror/legacy-modes/mode/go").then((m) => m.go),
  rust: () => import("@codemirror/legacy-modes/mode/rust").then((m) => m.rust),
  lua: () => import("@codemirror/legacy-modes/mode/lua").then((m) => m.lua),
  ruby: () => import("@codemirror/legacy-modes/mode/ruby").then((m) => m.ruby),
  c: () => import("@codemirror/legacy-modes/mode/clike").then((m) => m.c),
  cpp: () => import("@codemirror/legacy-modes/mode/clike").then((m) => m.cpp),
  java: () => import("@codemirror/legacy-modes/mode/clike").then((m) => m.java),
  diff: () => import("@codemirror/legacy-modes/mode/diff").then((m) => m.diff),
};

export async function loadLanguage(language: EditorLanguage): Promise<Extension> {
  if (language === "plain") return [];
  return StreamLanguage.define(await modes[language]());
}

/** Syntax colors from the app theme's `--syntax-*` variables. */
const highlightStyle = HighlightStyle.define([
  { tag: [tags.comment, tags.meta], color: "var(--syntax-comment)" },
  { tag: [tags.keyword, tags.operatorKeyword, tags.modifier, tags.controlKeyword], color: "var(--syntax-keyword)" },
  { tag: [tags.string, tags.special(tags.string), tags.regexp], color: "var(--syntax-string)" },
  { tag: [tags.number, tags.integer, tags.float], color: "var(--syntax-number)" },
  { tag: [tags.bool, tags.null, tags.atom, tags.literal], color: "var(--syntax-literal)" },
  { tag: [tags.propertyName, tags.attributeName, tags.definition(tags.variableName), tags.labelName], color: "var(--syntax-key)" },
  { tag: [tags.typeName, tags.className, tags.tagName, tags.heading], color: "var(--syntax-type)" },
  { tag: [tags.punctuation, tags.bracket, tags.operator, tags.separator], color: "var(--syntax-punct)" },
  { tag: tags.inserted, color: "var(--syntax-string)" },
  { tag: tags.deleted, color: "var(--syntax-number)" },
  { tag: tags.invalid, textDecoration: "underline wavy" },
]);

const theme = EditorView.theme({
  "&": { height: "100%", color: "var(--text)", backgroundColor: "transparent" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": {
    fontFamily: "var(--font-mono)",
    fontSize: "13px",
    lineHeight: "20px",
    fontVariantLigatures: "none",
    overscrollBehavior: "contain",
  },
  ".cm-content": { padding: "10px 0", caretColor: "var(--text)" },
  ".cm-gutters": {
    backgroundColor: "transparent",
    color: "var(--faint)",
    border: "none",
    borderRight: "1px solid var(--hairline)",
  },
  ".cm-activeLine": { backgroundColor: "rgba(var(--accent-rgb), 0.06)" },
  ".cm-activeLineGutter": { backgroundColor: "rgba(var(--accent-rgb), 0.1)", color: "var(--text)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--text)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection":
    { backgroundColor: "rgba(var(--accent-rgb), 0.3) !important" },
  ".cm-selectionMatch": { backgroundColor: "rgba(var(--accent-rgb), 0.16)" },
  ".cm-matchingBracket": { outline: "1px solid rgba(var(--accent-rgb), 0.55)" },
  ".cm-searchMatch": { backgroundColor: "rgba(var(--accent-rgb), 0.22)" },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "rgba(var(--accent-rgb), 0.45)" },
  ".cm-panels": { backgroundColor: "transparent", color: "var(--text)" },
  ".cm-panels-bottom": { borderTop: "1px solid var(--hairline)" },
  ".cm-panel.cm-search": { padding: "8px 10px", display: "flex", flexWrap: "wrap", gap: "6px 8px", alignItems: "center" },
  ".cm-panel.cm-search br": { flexBasis: "100%", height: 0, content: '""' },
  ".cm-panel.cm-search .cm-textfield": {
    height: "26px",
    margin: 0,
    padding: "0 8px",
    border: "1px solid var(--hairline)",
    borderRadius: "6px",
    background: "var(--surface-soft-bg)",
    color: "var(--text)",
    font: "12px var(--font-mono)",
    outline: "none",
  },
  ".cm-panel.cm-search .cm-textfield:focus": { borderColor: "var(--focus-ring)", background: "var(--focus-bg)" },
  ".cm-panel.cm-search .cm-button": {
    height: "26px",
    margin: 0,
    padding: "0 10px",
    border: "1px solid var(--hairline)",
    borderRadius: "6px",
    backgroundImage: "none",
    background: "transparent",
    color: "var(--dim)",
    fontSize: "12px",
  },
  ".cm-panel.cm-search .cm-button:hover": { background: "var(--control-hover-bg)", color: "var(--text)" },
  ".cm-panel.cm-search label": { display: "inline-flex", alignItems: "center", gap: "4px", color: "var(--dim)", fontSize: "12px" },
  ".cm-panel.cm-search [name=close]": {
    position: "absolute",
    top: "6px",
    right: "8px",
    padding: "0 4px",
    border: "none",
    background: "transparent",
    color: "var(--faint)",
    fontSize: "16px",
    cursor: "pointer",
  },
});

/** The editor's interface text in the app's language. */
const phrases = () =>
  EditorState.phrases.of({
  Find: t("查找"),
  Replace: t("替换"),
  next: t("下一个"),
  previous: t("上一个"),
  all: t("全部"),
  "match case": t("区分大小写"),
  regexp: t("正则"),
  "by word": t("全字匹配"),
  replace: t("替换"),
  "replace all": t("全部替换"),
  close: t("关闭"),
  "current match": t("当前匹配"),
  "on line": t("位于第"),
  "replaced $ matches": t("已替换 $ 处"),
  "replaced match on line $": t("已替换第 $ 行的匹配"),
  "Go to line": t("跳转到行"),
  go: t("跳转"),
});

export interface CodeEditorOptions {
  content: string;
  readOnly: boolean;
  onChange: (content: string) => void;
  onSave: () => void;
}

/**
 * A CodeMirror editor with search, undo history and line numbers. Create it
 * once the file's text is known; the text is not replaced afterwards.
 */
export function createCodeEditor(parent: HTMLElement, options: CodeEditorOptions) {
  const language = new Compartment();
  const editable = new Compartment();
  let onChange = options.onChange;
  let onSave = options.onSave;

  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc: options.content,
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        highlightSpecialChars(),
        history(),
        drawSelection(),
        indentOnInput(),
        bracketMatching(),
        rectangularSelection(),
        crosshairCursor(),
        highlightActiveLine(),
        highlightSelectionMatches(),
        search({ top: false }),
        syntaxHighlighting(highlightStyle),
        theme,
        EditorState.tabSize.of(2),
        phrases(),
        language.of([]),
        editable.of(EditorState.readOnly.of(options.readOnly)),
        keymap.of([
          { key: "Mod-s", preventDefault: true, run: () => (onSave(), true) },
          indentWithTab,
          ...defaultKeymap,
          ...historyKeymap,
          ...searchKeymap,
        ]),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) onChange(update.state.doc.toString());
        }),
        EditorView.contentAttributes.of({ spellcheck: "false", "aria-label": t("文件内容") }),
      ],
    }),
  });

  return {
    view,
    setHandlers(handlers: Pick<CodeEditorOptions, "onChange" | "onSave">) {
      onChange = handlers.onChange;
      onSave = handlers.onSave;
    },
    setReadOnly(readOnly: boolean) {
      view.dispatch({ effects: editable.reconfigure(EditorState.readOnly.of(readOnly)) });
    },
    setLanguage(extension: Extension) {
      view.dispatch({ effects: language.reconfigure(extension) });
    },
    destroy() {
      view.destroy();
    },
  };
}
