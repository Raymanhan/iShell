import type { ServerRecord } from "../../types";

/** The server id of local terminal tabs (see `src-tauri/src/local_shell.rs`). */
export const LOCAL_SERVER_ID = "local";

/**
 * A stand-in server record for opening, cloning and splitting local
 * terminals like any other. Never saved or listed.
 */
export const LOCAL_SERVER: ServerRecord = {
  id: LOCAL_SERVER_ID,
  name: "本地终端",
  host: "localhost",
  port: 0,
  username: "",
  group: "",
  tags: [],
  authType: "key",
  color: "#8b95a7",
  notes: "",
  encoding: "UTF-8",
  sortOrder: 0,
  createdAt: 0,
  updatedAt: 0,
};

export function isLocalServerId(serverId: string | null | undefined): boolean {
  return serverId === LOCAL_SERVER_ID;
}
