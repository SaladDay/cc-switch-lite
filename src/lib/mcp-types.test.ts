import { describe, expect, it } from "vitest";

import {
  formToServer,
  specToForm,
  textToPairs,
  type McpServer,
} from "./mcp-types";

const server: McpServer = {
  id: "example",
  name: "Example",
  server: {
    type: "stdio",
    command: "old",
    args: ["old"],
    futureOption: { keep: true },
  },
  apps: { claude: true },
  description: "Keep metadata",
  tags: ["keep"],
};

describe("MCP simple form", () => {
  it("changes owned fields while preserving unknown server data and metadata", () => {
    const form = specToForm(server);
    form.command = "new";
    form.args = "first\nsecond";

    const updated = formToServer(form, server);

    expect(updated.server).toEqual({
      type: "stdio",
      command: "new",
      args: ["first", "second"],
      futureOption: { keep: true },
    });
    expect(updated.description).toBe("Keep metadata");
    expect(updated.tags).toEqual(["keep"]);
  });

  it("removes incompatible transport fields when the transport changes", () => {
    const form = specToForm(server);
    form.transport = "http";
    form.url = "https://example.com/mcp";

    const updated = formToServer(form, server);

    expect(updated.server).toEqual({
      type: "http",
      url: "https://example.com/mcp",
      futureOption: { keep: true },
    });
  });

  it("rejects malformed and duplicate key-value rows", () => {
    expect(() => textToPairs("TOKEN", "environment variable")).toThrow(
      "KEY=VALUE",
    );
    expect(() =>
      textToPairs("TOKEN=one\nTOKEN=two", "environment variable"),
    ).toThrow("Duplicate");
  });

  it("preserves untouched empty arguments and credential whitespace", () => {
    const spaced: McpServer = {
      ...server,
      server: {
        ...server.server,
        args: ["", " --flag "],
        env: { TOKEN: " abc " },
      },
    };
    const form = specToForm(spaced);
    form.name = "Renamed";

    const updated = formToServer(form, spaced);

    expect(updated.server.args).toEqual(["", " --flag "]);
    expect(updated.server.env).toEqual({ TOKEN: " abc " });
    expect(textToPairs("TOKEN= abc ", "environment variable")).toEqual({
      TOKEN: " abc ",
    });
  });
});
