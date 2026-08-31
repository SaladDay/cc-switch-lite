import type { McpServer } from "../lib/mcp-types";

export type McpPreset = Pick<
  McpServer,
  "id" | "name" | "server" | "description" | "homepage" | "docs" | "tags"
>;

function npx(packageName: string): { command: string; args: string[] } {
  const windows = globalThis.navigator?.userAgent.includes("Windows") ?? false;
  return windows
    ? { command: "cmd", args: ["/c", "npx", "-y", packageName] }
    : { command: "npx", args: ["-y", packageName] };
}

export const MCP_PRESETS: McpPreset[] = [
  {
    id: "fetch",
    name: "mcp-server-fetch",
    description:
      "Fetch web pages and convert them into model-friendly content.",
    tags: ["web", "stdio"],
    server: { type: "stdio", command: "uvx", args: ["mcp-server-fetch"] },
    homepage: "https://github.com/modelcontextprotocol/servers",
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/fetch",
  },
  {
    id: "time",
    name: "@modelcontextprotocol/server-time",
    description: "Work with time zones and current time.",
    tags: ["time", "stdio"],
    server: { type: "stdio", ...npx("@modelcontextprotocol/server-time") },
    homepage: "https://github.com/modelcontextprotocol/servers",
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/time",
  },
  {
    id: "memory",
    name: "@modelcontextprotocol/server-memory",
    description: "Store knowledge in a local graph.",
    tags: ["memory", "stdio"],
    server: { type: "stdio", ...npx("@modelcontextprotocol/server-memory") },
    homepage: "https://github.com/modelcontextprotocol/servers",
  },
  {
    id: "sequential-thinking",
    name: "@modelcontextprotocol/server-sequential-thinking",
    description: "Break complex work into a sequence of reasoning steps.",
    tags: ["reasoning", "stdio"],
    server: {
      type: "stdio",
      ...npx("@modelcontextprotocol/server-sequential-thinking"),
    },
    homepage: "https://github.com/modelcontextprotocol/servers",
  },
  {
    id: "context7",
    name: "@upstash/context7-mcp",
    description: "Look up current library documentation.",
    tags: ["docs", "stdio"],
    server: { type: "stdio", ...npx("@upstash/context7-mcp") },
    homepage: "https://context7.com",
    docs: "https://github.com/upstash/context7",
  },
];
