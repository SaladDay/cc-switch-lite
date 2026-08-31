import type { AppId, JsonValue } from "./provider-types";

export type McpTransport = "stdio" | "http" | "sse";

export interface McpServerSpec {
  [key: string]: JsonValue | undefined;
  type?: McpTransport;
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  cwd?: string;
  url?: string;
  headers?: Record<string, string>;
}

export interface McpServer {
  id: string;
  name: string;
  server: McpServerSpec;
  apps: Record<AppId, boolean>;
  description?: string;
  homepage?: string;
  docs?: string;
  tags: string[];
  revision?: number;
}

export interface McpImportReport {
  newServers: number;
  enabledApps: number;
  disabledApps: number;
  failedApps: string[];
}

export interface McpFormValue {
  id: string;
  name: string;
  transport: McpTransport;
  command: string;
  args: string;
  cwd: string;
  env: string;
  url: string;
  headers: string;
  apps: Record<AppId, boolean>;
}

export function specToForm(server: McpServer): McpFormValue {
  const transport =
    server.server.type ??
    (typeof server.server.url === "string" ? "sse" : "stdio");
  return {
    id: server.id,
    name: server.name,
    transport,
    command:
      typeof server.server.command === "string" ? server.server.command : "",
    args: Array.isArray(server.server.args)
      ? server.server.args.join("\n")
      : "",
    cwd: typeof server.server.cwd === "string" ? server.server.cwd : "",
    env: pairsToText(server.server.env),
    url: typeof server.server.url === "string" ? server.server.url : "",
    headers: pairsToText(server.server.headers),
    apps: { ...server.apps },
  };
}

export function formToServer(
  value: McpFormValue,
  initial?: McpServer,
): McpServer {
  const server: McpServerSpec = { ...(initial?.server ?? {}) };
  const original = initial ? specToForm(initial) : undefined;
  const sameTransport = original?.transport === value.transport;
  if (!sameTransport) {
    for (const key of [
      "type",
      "command",
      "args",
      "env",
      "cwd",
      "url",
      "headers",
    ]) {
      delete server[key];
    }
    server.type = value.transport;
  }
  if (value.transport === "stdio") {
    if (!sameTransport || value.command !== original?.command) {
      server.command = value.command.trim();
    }
    if (!sameTransport || value.args !== original?.args) {
      delete server.args;
      const args = argsFromText(value.args);
      if (args.length > 0) server.args = args;
    }
    if (!sameTransport || value.cwd !== original?.cwd) {
      delete server.cwd;
      const cwd = value.cwd.trim();
      if (cwd) server.cwd = cwd;
    }
    if (!sameTransport || value.env !== original?.env) {
      delete server.env;
      const env = textToPairs(value.env, "environment variable");
      if (Object.keys(env).length > 0) server.env = env;
    }
  } else {
    if (!sameTransport || value.url !== original?.url) {
      server.url = value.url.trim();
    }
    if (!sameTransport || value.headers !== original?.headers) {
      delete server.headers;
      const headers = textToPairs(value.headers, "header");
      if (Object.keys(headers).length > 0) server.headers = headers;
    }
  }
  return {
    ...(initial ?? { tags: [] }),
    id: value.id.trim(),
    name: value.name.trim() || value.id.trim(),
    server,
    apps: { ...(initial?.apps ?? {}), ...value.apps },
  };
}

export function textToPairs(
  text: string,
  label: string,
): Record<string, string> {
  const result: Record<string, string> = {};
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim()) continue;
    const separator = line.indexOf("=");
    const key = separator < 0 ? "" : line.slice(0, separator).trim();
    const value = separator < 0 ? "" : line.slice(separator + 1);
    if (!key) throw new Error(`Each ${label} must use KEY=VALUE.`);
    if (Object.prototype.hasOwnProperty.call(result, key)) {
      throw new Error(`Duplicate ${label}: ${key}`);
    }
    result[key] = value;
  }
  return result;
}

function argsFromText(text: string): string[] {
  return text === "" ? [] : text.split(/\r?\n/);
}

function pairsToText(value: JsonValue | undefined): string {
  if (!value || Array.isArray(value) || typeof value !== "object") return "";
  return Object.entries(value)
    .filter((entry): entry is [string, string] => typeof entry[1] === "string")
    .map(([key, item]) => `${key}=${item}`)
    .join("\n");
}
