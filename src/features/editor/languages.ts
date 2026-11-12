/** Which syntax a remote file is highlighted with, from its name alone. */
export type EditorLanguage =
  | "shell" | "yaml" | "toml" | "ini" | "nginx" | "dockerfile" | "json" | "javascript"
  | "typescript" | "python" | "sql" | "css" | "xml" | "html" | "go" | "rust" | "lua"
  | "ruby" | "c" | "cpp" | "java" | "diff" | "plain";

const BY_EXTENSION: Record<string, EditorLanguage> = {
  sh: "shell", bash: "shell", zsh: "shell", ksh: "shell",
  yaml: "yaml", yml: "yaml",
  toml: "toml",
  ini: "ini", cfg: "ini", conf: "ini", cnf: "ini", properties: "ini", env: "ini", service: "ini",
  timer: "ini", socket: "ini", mount: "ini", desktop: "ini",
  json: "json", jsonc: "json",
  js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "javascript",
  ts: "typescript", tsx: "typescript", mts: "typescript",
  py: "python", pyw: "python",
  sql: "sql",
  css: "css", scss: "css", less: "css",
  xml: "xml", svg: "xml", plist: "xml",
  html: "html", htm: "html",
  go: "go", rs: "rust", lua: "lua", rb: "ruby",
  c: "c", h: "c", cc: "cpp", cpp: "cpp", hpp: "cpp", java: "java",
  diff: "diff", patch: "diff",
};

const BY_NAME: Record<string, EditorLanguage> = {
  dockerfile: "dockerfile", containerfile: "dockerfile",
  ".bashrc": "shell", ".bash_profile": "shell", ".profile": "shell", ".zshrc": "shell",
  ".env": "ini", ".gitconfig": "ini", ".editorconfig": "ini",
  "nginx.conf": "nginx",
  gemfile: "ruby", rakefile: "ruby",
};

export function languageForFile(fileName: string, path = ""): EditorLanguage {
  const name = fileName.toLowerCase();
  if (BY_NAME[name]) return BY_NAME[name];
  if (name.startsWith("dockerfile.") || name.endsWith(".dockerfile")) return "dockerfile";
  if (name.startsWith(".env.")) return "ini";
  // nginx site files are usually named after the site, not *.conf.
  if (/\/nginx\/(?:sites-(?:available|enabled)|conf\.d)\//.test(path)) return "nginx";
  const extension = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1) : "";
  if (extension === "conf" && path.includes("/nginx/")) return "nginx";
  return BY_EXTENSION[extension] ?? "plain";
}
