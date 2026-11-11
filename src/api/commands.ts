/**
 * Every backend command, by name, with its arguments and result. `command()`
 * in `./tauri` takes its types from here, so a misspelt name, a missing or
 * misnamed argument, or a wrong result type fails the type check.
 *
 * Keep in step with `generate_handler!` in `src-tauri/src/lib.rs`;
 * `tests/commandMap.test.mjs` checks that the two list the same commands.
 * Argument names are the Rust parameter names in camelCase.
 */
import type { Channel } from "@tauri-apps/api/core";
import type {
  AiConfig,
  AiConfigInput,
  AiConversation,
  AiEvent,
  AiHistorySummary,
  AiModel,
} from "../features/ai/types";
import type {
  NetworkSample,
  PortForward,
  PortForwardInput,
  PortForwardStatus,
  ServerInput,
  ServerRecord,
  ServerStatus,
  SftpEntry,
  Snippet,
  SnippetInput,
} from "../types";

type None = Record<string, never>;

/** Servers are flattened, each optionally with its encrypted password. */
export interface ConnectionExportPayload {
  folders: string[];
  servers: (ServerRecord & { encryptedSecret?: unknown })[];
}

export interface ConnectionImportPayload {
  folders: string[];
  servers: (ServerInput & { password?: string | null })[];
}

export interface ChatInput {
  requestId: string;
  serverId: string;
  sessionId: string | null;
  model: string;
  effort: string;
  manualModel: boolean;
  manualEffort: boolean;
  permission: string;
  cwd: string | null;
  messages: Record<string, unknown>[];
  /** Other servers the task may operate on, each through its open terminal. */
  targets?: { serverId: string; sessionId: string }[];
}

export interface ChatResult {
  messages: Record<string, unknown>[];
  stopped: boolean;
  error: string | null;
  contextTokens: number;
}

export interface TextFile {
  content: string;
  revision: string;
}

export interface CommandMap {
  // AI assistant
  ai_get_config: { args: None; result: AiConfig };
  ai_save_config: { args: { input: AiConfigInput }; result: AiConfig };
  ai_list_models: { args: { input: AiConfigInput | null }; result: AiModel[] };
  ai_list_history: { args: None; result: AiHistorySummary[] };
  ai_load_history: { args: { id: string }; result: AiConversation };
  ai_save_history: { args: { input: AiConversation }; result: void };
  ai_delete_history: { args: { id: string }; result: void };
  ai_chat: { args: { input: ChatInput; onEvent: Channel<AiEvent> }; result: ChatResult };
  ai_suggest_command: {
    args: {
      input: { serverId: string; sessionId: string; model: string; effort: string; request: string };
    };
    result: string;
  };
  ai_cancel: { args: { requestId: string }; result: void };
  ai_approve: { args: { requestId: string; approvalId: string; allow: boolean }; result: void };

  // Servers and local data
  list_servers: { args: None; result: ServerRecord[] };
  save_server: { args: { input: ServerInput; password: string | null }; result: ServerRecord };
  reorder_servers: {
    args: { items: { id: string; group: string; sortOrder: number }[] };
    result: void;
  };
  export_connections: {
    args: {
      path: string;
      payload: ConnectionExportPayload;
      asZip: boolean;
      passphrase: string | null;
    };
    result: void;
  };
  import_connections: {
    args: { path: string; passphrase: string | null };
    result: ConnectionImportPayload;
  };
  delete_server: { args: { id: string }; result: void };
  test_server_connection: { args: { input: ServerInput; password: string | null }; result: void };
  invalidate_connection: { args: { id: string }; result: void };
  list_command_history: { args: { serverId: string | null }; result: string[] };
  save_command_history: { args: { serverId: string | null; commandText: string }; result: void };
  secret_storage: { args: None; result: "keychain" | "file" };
  list_snippets: { args: None; result: Snippet[] };
  save_snippet: { args: { input: SnippetInput }; result: Snippet };
  delete_snippet: { args: { id: string }; result: void };

  // Port forwards
  list_port_forwards: { args: None; result: PortForward[] };
  save_port_forward: { args: { input: PortForwardInput }; result: PortForward };
  delete_port_forward: { args: { id: string }; result: void };
  start_port_forward: { args: { id: string }; result: void };
  stop_port_forward: { args: { id: string }; result: void };
  port_forward_statuses: { args: None; result: PortForwardStatus[] };

  // Monitoring
  fetch_server_status: { args: { id: string; includeDisk: boolean | null }; result: ServerStatus };
  fetch_network_sample: { args: { id: string }; result: NetworkSample };
  start_process_monitor: { args: { id: string; consumerId: string }; result: void };
  stop_process_monitor: { args: { id: string; consumerId: string }; result: void };
  start_tail_monitor: {
    args: { id: string; path: string; initialLines: number; viewerId: string };
    result: void;
  };
  stop_tail_monitor: { args: { viewerId: string }; result: void };

  // Files
  sftp_list: { args: { id: string; path: string }; result: SftpEntry[] };
  sftp_resolve_directory: { args: { id: string; path: string }; result: string };
  sftp_download: { args: { id: string; path: string; transferId: string }; result: string };
  sftp_download_folder: {
    args: { id: string; path: string; transferId: string; mode: string };
    result: string;
  };
  sftp_upload: {
    args: {
      id: string;
      localPath: string;
      remoteDir: string;
      transferId: string;
      replaceExistingFolder?: boolean | null;
    };
    result: string;
  };
  cancel_upload: { args: { transferId: string }; result: void };
  cancel_download: { args: { transferId: string }; result: void };
  sftp_mkdir: { args: { id: string; path: string }; result: void };
  sftp_remove: { args: { id: string; path: string; isDir: boolean }; result: void };
  sftp_rename: { args: { id: string; from: string; to: string }; result: void };
  sftp_read_text_file: { args: { id: string; path: string }; result: TextFile };
  sftp_write_text_file: {
    args: { id: string; path: string; content: string; expectedRevision: string | null };
    result: string;
  };

  // Terminals
  open_terminal: { args: { id: string }; result: string };
  /** Returns the encoding to decode the session's output with. */
  attach_terminal: { args: { sessionId: string; onOutput: Channel<ArrayBuffer> }; result: string };
  detach_terminal: { args: { sessionId: string; channelId: number }; result: void };
  terminal_input: { args: { sessionId: string; data: string }; result: void };
  terminal_resize: { args: { sessionId: string; cols: number; rows: number }; result: void };
  /** Returns the encoding's canonical name. */
  terminal_set_encoding: { args: { sessionId: string; encoding: string }; result: string };
  close_terminal: { args: { sessionId: string }; result: void };
}

export type CommandName = keyof CommandMap;
export type CommandArgs<K extends CommandName> = CommandMap[K]["args"];
export type CommandResult<K extends CommandName> = CommandMap[K]["result"];
