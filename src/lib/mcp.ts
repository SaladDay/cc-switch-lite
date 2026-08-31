import { invoke } from "@tauri-apps/api/core";

import type { McpImportReport, McpServer } from "./mcp-types";

export const mcpApi = {
  list: () => invoke<McpServer[]>("list_mcp_servers"),
  upsert: (server: McpServer) => invoke<void>("upsert_mcp_server", { server }),
  toggle: (
    serverId: string,
    appId: string,
    enabled: boolean,
    expectedRevision: number,
  ) =>
    invoke<void>("toggle_mcp_app", {
      serverId,
      appId,
      enabled,
      expectedRevision,
    }),
  delete: (id: string, expectedRevision: number) =>
    invoke<void>("delete_mcp_server", { id, expectedRevision }),
  importExisting: () => invoke<McpImportReport>("import_mcp_from_apps"),
};
