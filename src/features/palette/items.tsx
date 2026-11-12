import {
  Activity,
  ArrowRightLeft,
  Clock3,
  Code2,
  Columns2,
  FolderTree,
  Languages,
  Network,
  Plus,
  Radio,
  RefreshCw,
  Rows2,
  Server,
  Settings,
  Sparkles,
  SquareCode,
  SquareTerminal,
  X,
} from "lucide-react";
import type { PaletteItem } from "../../components/CommandPalette";
import { msg, t } from "../../i18n";
import type { ServerRecord, Snippet } from "../../types";
import { TERMINAL_ENCODINGS } from "../shell/encodings";
import { isLocalServerId } from "../shell/local";
import { splitShortcutLabel } from "../shell/panes";
import type { ShellTab, SidePanel } from "../shell/types";

export interface PaletteContext {
  servers: ServerRecord[];
  /** One per tab in the tab bar. */
  tabs: ShellTab[];
  activeGroupId: string | null;
  activeTab: ShellTab | null;
  snippets: Snippet[];
  broadcasting: boolean;
  connect: (server: ServerRecord) => void;
  openLocal: () => void;
  activateGroup: (groupId: string) => void;
  newServer: () => void;
  openSettings: () => void;
  toggleConnections: () => void;
  toggleFiles: () => void;
  toggleSidePanel: (panel: SidePanel) => void;
  split: (direction: "row" | "column") => void;
  reconnect: () => void;
  closeTab: () => void;
  toggleBroadcast: () => void;
  setEncoding: (encoding: string) => void;
  sendSnippet: (snippet: Snippet) => void;
}

const PANELS: { panel: SidePanel; title: string; keywords: string; icon: typeof Activity; remote: boolean }[] = [
  { panel: "status", title: msg("监控面板"), keywords: "status monitor cpu memory 状态", icon: Activity, remote: true },
  { panel: "history", title: msg("命令历史"), keywords: "history commands", icon: Clock3, remote: false },
  { panel: "snippets", title: msg("命令片段"), keywords: "snippets", icon: SquareCode, remote: false },
  { panel: "forwards", title: msg("端口转发"), keywords: "port forward tunnel socks", icon: Network, remote: true },
  { panel: "ai", title: msg("AI 助手"), keywords: "ai assistant chat", icon: Sparkles, remote: true },
];

/** Everything the command palette offers, in the order shown before typing. */
export function paletteItems(context: PaletteContext): PaletteItem[] {
  const items: PaletteItem[] = [];
  const { activeTab, activeGroupId } = context;
  const local = isLocalServerId(activeTab?.serverId);

  for (const tab of context.tabs) {
    if (tab.id === activeGroupId) continue;
    items.push({
      id: `tab:${tab.id}`,
      section: t("标签"),
      title: t("切换到 {0}", tab.title),
      detail: tab.subtitle,
      keywords: "tab switch",
      icon: <ArrowRightLeft size={14} />,
      run: () => context.activateGroup(tab.id),
    });
  }

  for (const server of context.servers) {
    items.push({
      id: `server:${server.id}`,
      section: t("连接"),
      title: t("连接 {0}", server.name),
      detail: `${server.username}@${server.host}${server.port === 22 ? "" : `:${server.port}`}${
        server.group ? ` · ${server.group}` : ""
      }`,
      keywords: `ssh connect ${server.tags.join(" ")}`,
      icon: <Server size={14} />,
      run: () => context.connect(server),
    });
  }
  items.push({
    id: "local",
    section: t("连接"),
    title: t("打开本地终端"),
    keywords: "local shell terminal 本机",
    icon: <SquareTerminal size={14} />,
    run: context.openLocal,
  });

  if (activeTab) {
    for (const snippet of context.snippets) {
      items.push({
        id: `snippet:${snippet.id}`,
        section: t("片段"),
        title: snippet.name,
        detail: snippet.command,
        keywords: `snippet ${snippet.description}`,
        icon: <Code2 size={14} />,
        run: () => context.sendSnippet(snippet),
      });
    }
  }

  const action = (item: Omit<PaletteItem, "section">) => items.push({ section: t("操作"), ...item });
  if (activeTab) {
    for (const { panel, title, keywords, icon: Icon, remote } of PANELS) {
      if (remote && local) continue;
      action({ id: `panel:${panel}`, title: t("显示/隐藏{0}", t(title)), keywords: `panel ${keywords}`, icon: <Icon size={14} />, run: () => context.toggleSidePanel(panel) });
    }
    if (!local) {
      action({ id: "files", title: t("显示/隐藏文件面板"), keywords: "files sftp 文件", icon: <FolderTree size={14} />, run: context.toggleFiles });
    }
    action({ id: "split:row", title: t("向右分屏"), keywords: "split", shortcut: splitShortcutLabel.row, icon: <Columns2 size={14} />, run: () => context.split("row") });
    action({ id: "split:column", title: t("向下分屏"), keywords: "split", shortcut: splitShortcutLabel.column, icon: <Rows2 size={14} />, run: () => context.split("column") });
    action({ id: "reconnect", title: t("重新连接当前标签"), keywords: "reconnect", icon: <RefreshCw size={14} />, run: context.reconnect });
    action({
      id: "broadcast",
      title: context.broadcasting ? t("停止广播输入") : t("开启广播输入"),
      keywords: "broadcast",
      icon: <Radio size={14} />,
      run: context.toggleBroadcast,
    });
    action({ id: "close", title: t("关闭当前标签"), keywords: "close tab", icon: <X size={14} />, run: context.closeTab });
    const current = (activeTab.encoding ?? "UTF-8").toLowerCase();
    for (const encoding of TERMINAL_ENCODINGS) {
      if (encoding.value.toLowerCase() === current) continue;
      action({
        id: `encoding:${encoding.value}`,
        title: t("终端编码切换为 {0}", t(encoding.label)),
        keywords: "encoding charset 编码 乱码",
        icon: <Languages size={14} />,
        run: () => context.setEncoding(encoding.value),
      });
    }
  }
  action({ id: "connections", title: t("显示/隐藏连接管理"), keywords: "connections servers", icon: <Server size={14} />, run: context.toggleConnections });
  action({ id: "new-server", title: t("新建服务器"), keywords: "new add server", icon: <Plus size={14} />, run: context.newServer });
  action({ id: "settings", title: t("打开设置"), keywords: "settings preferences", icon: <Settings size={14} />, run: context.openSettings });
  return items;
}
