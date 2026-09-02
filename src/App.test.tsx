import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import App, { sameJsonValue } from "./App";
import type { McpServer } from "./lib/mcp-types";
import type { InstalledSkill } from "./lib/skill-types";
import type {
  AdapterDescriptor,
  AppId,
  CoreAppDescriptor,
  CurrentProvider,
  ProviderRecord,
  SimpleProviderFormDescriptor,
} from "./lib/provider-types";

const api = vi.hoisted(() => ({
  supportedApps: vi.fn(),
  listAdapters: vi.fn(),
  listSimpleForms: vi.fn(),
  list: vi.fn(),
  createSimple: vi.fn(),
  updateSimple: vi.fn(),
  delete: vi.fn(),
  importNative: vi.fn(),
  switch: vi.fn(),
  removeFromLive: vi.fn(),
  currentProviders: vi.fn(),
}));

const mcp = vi.hoisted(() => ({
  list: vi.fn(),
  upsert: vi.fn(),
  toggle: vi.fn(),
  delete: vi.fn(),
  importExisting: vi.fn(),
}));

const skills = vi.hoisted(() => ({
  list: vi.fn(),
  toggle: vi.fn(),
}));

vi.mock("./lib/providers", async (importOriginal) => {
  const original = await importOriginal<typeof import("./lib/providers")>();
  return { ...original, providersApi: api };
});

vi.mock("./lib/mcp", () => ({ mcpApi: mcp }));
vi.mock("./lib/skills", () => ({ skillsApi: skills }));

const adapters: AdapterDescriptor[] = [
  {
    appId: "claude",
    displayName: "Claude API",
    reference: {
      pluginId: "org.cc-switch.builtin",
      pluginVersion: "0.1.0",
      adapterId: "builtin.claude.api-key",
      contractMajor: 1,
      schemaVersion: 1,
    },
    fields: [
      {
        key: "baseUrl",
        label: "Base URL",
        kind: "url",
        required: false,
        placeholder: "https://api.anthropic.com",
        help: "Leave empty to use the default endpoint.",
      },
      {
        key: "apiKey",
        label: "API key",
        kind: "secret",
        required: true,
        placeholder: "sk-ant-…",
        help: "Stored privately.",
      },
    ],
  },
  {
    appId: "codex",
    displayName: "OpenAI API",
    reference: {
      pluginId: "org.cc-switch.builtin",
      pluginVersion: "0.1.0",
      adapterId: "builtin.codex.api-key",
      contractMajor: 1,
      schemaVersion: 1,
    },
    fields: [
      {
        key: "apiKey",
        label: "API key",
        kind: "secret",
        required: true,
        placeholder: "sk-…",
        help: "Stored privately.",
      },
    ],
  },
];

const workProvider: ProviderRecord = {
  id: "provider-1",
  revision: 1,
  appId: "claude",
  adapter: adapters[0].reference,
  name: "Work",
  settings: { apiKey: "secret", baseUrl: "https://proxy.example.com" },
  simpleValues: {
    apiKey: "secret",
    baseUrl: "https://proxy.example.com",
    model: "",
  },
  liteConfigWritable: true,
  liteSimpleEditable: true,
};

const contextServer: McpServer = {
  id: "context7",
  name: "Context7",
  server: {
    type: "stdio",
    command: "npx",
    args: ["-y", "@upstash/context7-mcp"],
  },
  apps: {
    claude: true,
    codex: false,
    gemini: false,
    grokbuild: false,
    opencode: false,
    hermes: false,
  },
  tags: ["docs"],
  revision: 1,
};

const demoSkill: InstalledSkill = {
  id: "demo",
  name: "Demo Skill",
  description: "A configured Skill",
  directory: "demo",
  apps: [
    {
      app: "claude",
      selected: true,
      enabled: true,
      writable: true,
      canEnable: true,
      canDisable: true,
      reason: null,
    },
    {
      app: "codex",
      selected: false,
      enabled: false,
      writable: false,
      canEnable: false,
      canDisable: false,
      reason: "nativeConflict",
    },
  ],
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

function current(provider: ProviderRecord) {
  return { id: provider.id, revision: provider.revision };
}

function nativeAdapter(appId: string): AdapterDescriptor {
  return {
    appId,
    displayName: "Native configuration",
    reference: {
      pluginId: "org.cc-switch.builtin",
      pluginVersion: "0.1.0",
      adapterId: `builtin.${appId}.native`,
      contractMajor: 1,
      schemaVersion: 1,
    },
    fields: [],
  };
}

function coreApps(appIds: AppId[]): CoreAppDescriptor[] {
  const additive = new Set<AppId>(["opencode", "openclaw", "hermes", "pi"]);
  const mcp = new Set<AppId>([
    "claude",
    "codex",
    "gemini",
    "grokbuild",
    "opencode",
    "hermes",
  ]);
  const skills = new Set<AppId>([
    "claude",
    "codex",
    "gemini",
    "grokbuild",
    "opencode",
    "hermes",
    "pi",
  ]);
  return appIds.map((id) => {
    const capabilities = ["provider-management", "live-configuration"];
    if (mcp.has(id)) capabilities.push("mcp");
    if (skills.has(id)) capabilities.push("skills");
    return {
      id,
      displayName: id,
      brandKey: id,
      configurationMode: additive.has(id) ? "additive" : "switch",
      capabilities,
    };
  });
}

function simpleForms(appIds: AppId[]): SimpleProviderFormDescriptor[] {
  return appIds.map((appId) => ({
    appId,
    defaultProtocol:
      appId === "claude" || appId === "claude-desktop"
        ? "anthropic-messages"
        : "openai-completions",
    protocolLocked: appId === "claude",
    fields: [
      { key: "baseUrl", required: appId !== "claude" },
      { key: "apiKey", required: true },
      ...(appId === "claude-desktop"
        ? []
        : [{ key: "model" as const, required: false }]),
    ],
    presets:
      appId === "claude"
        ? [
            {
              id: "kimi",
              name: "Kimi",
              websiteUrl: "https://platform.moonshot.cn",
              brandKey: "kimi",
              baseUrl: "https://api.moonshot.cn/anthropic",
              model: "kimi-k2.5",
            },
          ]
        : [
            {
              id: "example",
              name: "Example",
              websiteUrl: "https://example.com",
              brandKey: "example",
              baseUrl: "https://api.example.com",
              model: "example-model",
            },
          ],
  }));
}

describe("App", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.className = "";
    for (const mock of Object.values(api)) mock.mockReset();
    for (const mock of Object.values(mcp)) mock.mockReset();
    for (const mock of Object.values(skills)) mock.mockReset();
    const appIds = [
      "claude",
      "claude-desktop",
      "codex",
      "gemini",
      "grokbuild",
      "opencode",
      "openclaw",
      "hermes",
      "pi",
    ];
    api.listAdapters.mockResolvedValue([
      ...appIds.map(nativeAdapter),
      ...adapters,
    ]);
    api.listSimpleForms.mockResolvedValue(simpleForms(appIds));
    api.supportedApps.mockResolvedValue(coreApps(appIds));
    api.list.mockResolvedValue([]);
    api.delete.mockResolvedValue(undefined);
    api.importNative.mockResolvedValue([]);
    api.switch.mockResolvedValue(undefined);
    api.removeFromLive.mockResolvedValue(undefined);
    api.currentProviders.mockResolvedValue([]);
    mcp.list.mockResolvedValue([]);
    mcp.upsert.mockResolvedValue(undefined);
    mcp.toggle.mockResolvedValue(undefined);
    mcp.delete.mockResolvedValue(undefined);
    mcp.importExisting.mockResolvedValue({
      newServers: 0,
      enabledApps: 0,
      disabledApps: 0,
      failedApps: [],
    });
    skills.list.mockResolvedValue([]);
    skills.toggle.mockResolvedValue(undefined);
  });

  it("shows every application from the shared core boundary", async () => {
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Add your first Claude Code provider",
      }),
    ).toBeVisible();

    const switcher = within(
      screen.getByRole("navigation", { name: "Applications" }),
    );
    expect(
      switcher.getByRole("button", { name: "Claude Code" }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(switcher.getByRole("button", { name: "Codex" })).toBeVisible();
    expect(switcher.getByRole("button", { name: "Pi" })).toBeVisible();
    expect(switcher.getAllByRole("button")).toHaveLength(9);
  });

  it("uses core capabilities for the Lite feature navigation", async () => {
    const user = userEvent.setup();
    render(<App />);

    await screen.findByRole("heading", {
      name: "Add your first Claude Code provider",
    });
    await user.click(screen.getByRole("button", { name: "Manage Skills" }));
    expect(screen.getByRole("heading", { name: "Skills" })).toBeVisible();
    expect(
      await screen.findByRole("heading", { name: "No installed Skills" }),
    ).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Back to providers" }));
    await user.click(
      screen.getByRole("button", { name: "Manage MCP servers" }),
    );
    expect(screen.getByRole("heading", { name: "MCP Servers" })).toBeVisible();
    expect(
      await screen.findByRole("heading", { name: "No MCP servers" }),
    ).toBeVisible();
  });

  it("switches installed Skills through the core-backed application state", async () => {
    const user = userEvent.setup();
    skills.list.mockResolvedValue([demoSkill]);
    render(<App />);

    await screen.findByRole("heading", {
      name: "Add your first Claude Code provider",
    });
    await user.click(screen.getByRole("button", { name: "Manage Skills" }));
    expect(await screen.findByText("Demo Skill")).toBeVisible();

    const claude = screen.getByRole("button", {
      name: "Disable Demo Skill for Claude Code",
    });
    expect(claude).toHaveAttribute("aria-pressed", "true");
    await user.click(claude);

    await waitFor(() =>
      expect(skills.toggle).toHaveBeenCalledWith("demo", "claude", false),
    );
    expect(
      screen.getByRole("button", {
        name: /Enable Demo Skill for Codex.*unmanaged entry/,
      }),
    ).toBeDisabled();
  });

  it("blocks MCP actions until the initial catalog has loaded", async () => {
    const user = userEvent.setup();
    const initial = deferred<McpServer[]>();
    mcp.list.mockReturnValueOnce(initial.promise);
    render(<App />);

    await screen.findByRole("heading", {
      name: "Add your first Claude Code provider",
    });
    await user.click(
      screen.getByRole("button", { name: "Manage MCP servers" }),
    );
    const add = screen.getByRole("button", { name: "Add MCP" });
    await waitFor(() => expect(add).toBeDisabled());
    await user.click(add);
    expect(
      screen.queryByRole("heading", { name: "Add MCP server" }),
    ).not.toBeInTheDocument();

    initial.resolve([]);
    await waitFor(() => expect(add).toBeEnabled());
  });

  it("manages shared MCP application switches from the family-style panel", async () => {
    const user = userEvent.setup();
    const enabledServer = {
      ...contextServer,
      apps: { ...contextServer.apps, codex: true },
      revision: 2,
    };
    mcp.list
      .mockResolvedValueOnce([contextServer])
      .mockResolvedValue([enabledServer]);
    render(<App />);

    await screen.findByRole("heading", {
      name: "Add your first Claude Code provider",
    });
    await user.click(
      screen.getByRole("button", { name: "Manage MCP servers" }),
    );
    expect(await screen.findByText("Context7")).toBeVisible();

    await user.click(
      screen.getByRole("button", { name: "Enable Context7 for Codex" }),
    );
    await waitFor(() =>
      expect(mcp.toggle).toHaveBeenCalledWith("context7", "codex", true, 1),
    );
    await waitFor(() => expect(mcp.list).toHaveBeenCalledTimes(2));
    expect(
      screen.getByRole("button", { name: "Disable Context7 for Codex" }),
    ).toHaveAttribute("aria-pressed", "true");
  });

  it("adds an MCP preset without exposing a raw configuration editor", async () => {
    const user = userEvent.setup();
    render(<App />);

    await screen.findByRole("heading", {
      name: "Add your first Claude Code provider",
    });
    await user.click(
      screen.getByRole("button", { name: "Manage MCP servers" }),
    );
    await screen.findByRole("heading", { name: "No MCP servers" });
    await user.click(screen.getByRole("button", { name: "Add MCP" }));
    expect(
      screen.getByRole("heading", { name: "Add MCP server" }),
    ).toBeVisible();
    expect(screen.queryByText("JSON configuration")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "context7" }));
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await waitFor(() => expect(mcp.upsert).toHaveBeenCalledTimes(1));
    expect(mcp.upsert.mock.calls[0][0]).toMatchObject({
      id: "context7",
      server: {
        type: "stdio",
        command: expect.any(String),
      },
      apps: {
        claude: true,
        codex: true,
        gemini: true,
        grokbuild: true,
        opencode: false,
        hermes: false,
      },
    });
  });

  it("hides feature entries not declared by core", async () => {
    const native = nativeAdapter("openclaw");
    window.localStorage.setItem("cc-switch-lite:last-app", "openclaw");
    api.supportedApps.mockResolvedValue(coreApps(["openclaw"]));
    api.listAdapters.mockResolvedValue([native]);
    render(<App />);

    await screen.findByRole("heading", {
      name: "Add your first OpenClaw provider",
    });
    expect(
      screen.queryByRole("button", { name: "Manage Skills" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Manage MCP servers" }),
    ).not.toBeInTheDocument();
  });

  it("disables provider and live actions when core declares neither capability", async () => {
    const native = nativeAdapter("claude");
    const catalog = coreApps(["claude"]);
    catalog[0].capabilities = [];
    api.supportedApps.mockResolvedValue(catalog);
    api.listAdapters.mockResolvedValue([native]);
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Add your first Claude Code provider",
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Add Claude Code provider" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", {
        name: "Import Claude Code user configuration",
      }),
    ).toBeDisabled();
    expect(api.currentProviders).not.toHaveBeenCalled();
  });

  it("keeps provider CRUD separate from the live configuration capability", async () => {
    const native = nativeAdapter("claude");
    const catalog = coreApps(["claude"]);
    catalog[0].capabilities = ["provider-management"];
    api.supportedApps.mockResolvedValue(catalog);
    api.listAdapters.mockResolvedValue([native]);
    api.list.mockResolvedValue([
      { ...workProvider, adapter: native.reference },
    ]);
    render(<App />);

    expect(await screen.findByRole("heading", { name: "Work" })).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Add Claude Code provider" }),
    ).toBeEnabled();
    expect(screen.getByRole("button", { name: "Edit Work" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Delete Work" })).toBeEnabled();
    expect(
      screen.getByRole("button", {
        name: "Import Claude Code user configuration",
      }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Switch to Work" }),
    ).toBeDisabled();
  });

  it("keeps live configuration actions independent from provider CRUD", async () => {
    const user = userEvent.setup();
    const native = nativeAdapter("claude");
    const catalog = coreApps(["claude"]);
    catalog[0].capabilities = ["live-configuration"];
    api.supportedApps.mockResolvedValue(catalog);
    api.listAdapters.mockResolvedValue([native]);
    api.list.mockResolvedValue([
      { ...workProvider, adapter: native.reference },
    ]);
    render(<App />);

    expect(await screen.findByRole("heading", { name: "Work" })).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Add Claude Code provider" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Switch to Work" }),
    ).toBeEnabled();
    expect(screen.getByRole("button", { name: "Edit Work" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Delete Work" })).toBeDisabled();
    const importButton = screen.getByRole("button", {
      name: "Import Claude Code user configuration",
    });
    expect(importButton).toBeEnabled();

    await user.click(importButton);
    await waitFor(() =>
      expect(api.importNative).toHaveBeenCalledWith("claude"),
    );
  });

  it("allows additive provider deletion without live configuration access", async () => {
    const native = nativeAdapter("pi");
    const catalog = coreApps(["pi"]);
    catalog[0].capabilities = ["provider-management"];
    window.localStorage.setItem("cc-switch-lite:last-app", "pi");
    api.supportedApps.mockResolvedValue(catalog);
    api.listAdapters.mockResolvedValue([native]);
    api.list.mockResolvedValue([
      {
        ...workProvider,
        appId: "pi",
        adapter: native.reference,
        name: "Pi Work",
      },
    ]);
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Pi Work" }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Delete Pi Work" }),
    ).toBeEnabled();
    expect(
      screen.getByRole("button", { name: "Add Pi Work to configuration" }),
    ).toBeDisabled();
    expect(api.currentProviders).toHaveBeenCalledWith("pi");
  });

  it("closes a stored feature view when the core catalog is unavailable", async () => {
    window.localStorage.setItem("cc-switch-lite:last-view", "mcp");
    api.supportedApps.mockRejectedValue(new Error("catalog unavailable"));
    render(<App />);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "catalog unavailable",
    );
    expect(
      screen.queryByRole("heading", { name: "MCP Servers" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText("MCP Servers placeholder"),
    ).not.toBeInTheDocument();
  });

  it("uses the core response as the application membership source", async () => {
    api.supportedApps.mockResolvedValue(coreApps(["pi"]));
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Add your first Pi provider",
      }),
    ).toBeVisible();
    const switcher = within(
      screen.getByRole("navigation", { name: "Applications" }),
    );
    expect(switcher.getAllByRole("button")).toHaveLength(1);
    expect(switcher.getByRole("button", { name: "Pi" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(api.list).toHaveBeenLastCalledWith("pi");
  });

  it("accepts a new application declared by core without a Lite whitelist", async () => {
    const futureApp: CoreAppDescriptor = {
      id: "future-agent",
      displayName: "Future Agent",
      brandKey: "future-agent",
      configurationMode: "switch",
      capabilities: ["provider-management", "live-configuration"],
    };
    api.supportedApps.mockResolvedValue([futureApp]);
    api.listAdapters.mockResolvedValue([nativeAdapter(futureApp.id)]);
    render(<App />);

    expect(
      await screen.findByRole("heading", {
        name: "Add your first Future Agent provider",
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Future Agent" }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(api.list).toHaveBeenLastCalledWith("future-agent");
  });

  it("uses the core configuration mode instead of UI presentation metadata", async () => {
    const native = nativeAdapter("pi");
    const catalog = coreApps(["pi"]);
    catalog[0].configurationMode = "switch";
    window.localStorage.setItem("cc-switch-lite:last-app", "pi");
    api.supportedApps.mockResolvedValue(catalog);
    api.listAdapters.mockResolvedValue([native]);
    api.list.mockResolvedValue([
      {
        ...workProvider,
        id: "pi-provider",
        appId: "pi",
        adapter: native.reference,
        name: "Pi Work",
        settings: { baseUrl: "https://pi.example.com/v1" },
      },
    ]);
    render(<App />);

    expect(
      await screen.findByRole("button", { name: "Switch to Pi Work" }),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", {
        name: "Add Pi Work to configuration",
      }),
    ).not.toBeInTheDocument();
  });

  it("fails closed when the core application catalog is unavailable", async () => {
    api.supportedApps.mockRejectedValue(new Error("Catalog unavailable"));
    api.list.mockResolvedValue([workProvider]);
    render(<App />);

    expect(await screen.findByRole("heading", { name: "Work" })).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent("Catalog unavailable");
    expect(
      screen.getByRole("button", { name: "Switch to Work" }),
    ).toBeDisabled();
    expect(screen.getByRole("button", { name: "Edit Work" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Delete Work" })).toBeDisabled();
    expect(api.delete).not.toHaveBeenCalled();
  });

  it("keeps a valid core catalog when adapter loading fails", async () => {
    api.listAdapters.mockRejectedValue(new Error("Adapters unavailable"));
    api.list.mockResolvedValue([workProvider]);
    render(<App />);

    expect(await screen.findByRole("heading", { name: "Work" })).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent("Adapters unavailable");
    expect(screen.getByRole("button", { name: "Delete Work" })).toBeEnabled();
    const switcher = within(
      screen.getByRole("navigation", { name: "Applications" }),
    );
    expect(switcher.getAllByRole("button")).toHaveLength(9);
  });

  it("rejects an unknown core configuration mode", async () => {
    api.supportedApps.mockResolvedValue([
      { ...coreApps(["claude"])[0], configurationMode: "stacked" },
    ]);
    api.list.mockResolvedValue([workProvider]);
    render(<App />);

    expect(await screen.findByRole("heading", { name: "Work" })).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Core returned an invalid application catalog",
    );
    expect(screen.getByRole("button", { name: "Delete Work" })).toBeDisabled();
  });

  it("switches the provider list and remembers the selected application", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(await screen.findByRole("button", { name: "Codex" }));

    expect(
      await screen.findByRole("heading", {
        name: "Add your first Codex provider",
      }),
    ).toBeVisible();
    expect(api.list).toHaveBeenLastCalledWith("codex");
    expect(window.localStorage.getItem("cc-switch-lite:last-app")).toBe(
      "codex",
    );
  });

  it("ignores a stale current-provider response after changing applications", async () => {
    const user = userEvent.setup();
    const claudeCurrent = deferred<CurrentProvider[]>();
    const codexProvider: ProviderRecord = {
      ...workProvider,
      id: "codex-provider",
      appId: "codex",
      adapter: adapters[1].reference,
      name: "Codex Work",
    };
    api.list.mockImplementation((appId: string) =>
      Promise.resolve(appId === "codex" ? [codexProvider] : [workProvider]),
    );
    api.currentProviders.mockImplementation((appId: string) =>
      appId === "claude"
        ? claudeCurrent.promise
        : Promise.resolve([current(codexProvider)]),
    );
    render(<App />);

    await user.click(await screen.findByRole("button", { name: "Codex" }));
    expect(
      await screen.findByRole("button", { name: "Codex Work is current" }),
    ).toBeDisabled();

    claudeCurrent.resolve([current(workProvider)]);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Codex Work is current" }),
      ).toBeDisabled(),
    );
    expect(screen.queryByRole("heading", { name: "Work" })).toBeNull();
  });

  it("creates a provider from the core-backed simple form", async () => {
    const user = userEvent.setup();
    api.createSimple.mockResolvedValue(workProvider);
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Add Claude Code provider" }),
    );
    const dialog = screen.getByRole("dialog", { name: "Add provider" });
    await user.type(within(dialog).getByLabelText("Provider name"), "Work");
    await user.type(within(dialog).getByLabelText("API key"), "secret");
    await user.click(
      within(dialog).getByRole("button", { name: "Save provider" }),
    );

    await waitFor(() =>
      expect(api.createSimple).toHaveBeenCalledWith({
        appId: "claude",
        name: "Work",
        values: { apiKey: "secret", baseUrl: "", model: "" },
      }),
    );
    expect(screen.getByRole("heading", { name: "Work" })).toBeVisible();
  });

  it("keeps Claude on one simple Anthropic Messages form", async () => {
    const user = userEvent.setup();
    const native = nativeAdapter("claude");
    api.listAdapters.mockResolvedValue([native, ...adapters]);
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Add Claude Code provider" }),
    );
    expect(screen.getByText("Provider preset")).toBeVisible();
    expect(screen.getByLabelText("API key")).toBeVisible();
    expect(screen.getByText(/Anthropic Messages/)).toBeVisible();
    expect(screen.queryByLabelText("Adapter")).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText("Configuration JSON"),
    ).not.toBeInTheDocument();
  });

  it("fills public preset values without replacing an entered API key", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Add Claude Code provider" }),
    );
    const dialog = screen.getByRole("dialog", { name: "Add provider" });
    await user.type(within(dialog).getByLabelText("API key"), "private-key");
    await user.click(within(dialog).getByRole("button", { name: "Kimi" }));

    expect(within(dialog).getByLabelText("Provider name")).toHaveValue("Kimi");
    expect(within(dialog).getByLabelText(/Base URL/)).toHaveValue(
      "https://api.moonshot.cn/anthropic",
    );
    expect(within(dialog).getByLabelText("API key")).toHaveValue("private-key");
    expect(within(dialog).queryByLabelText(/Protocol/)).not.toBeInTheDocument();
  });

  it("keeps an existing Grok environment credential when the API key is empty", async () => {
    const user = userEvent.setup();
    const native = nativeAdapter("grokbuild");
    const provider: ProviderRecord = {
      ...workProvider,
      id: "grok-env",
      appId: "grokbuild",
      adapter: native.reference,
      name: "Grok environment",
      settings: { config: 'env_key = "XAI_API_KEY"' },
      simpleValues: {
        baseUrl: "https://api.x.ai/v1",
        apiKey: "",
        model: "grok-4",
      },
    };
    api.supportedApps.mockResolvedValue(coreApps(["grokbuild"]));
    api.listAdapters.mockResolvedValue([native]);
    api.list.mockResolvedValue([provider]);
    api.updateSimple.mockResolvedValue({ ...provider, revision: 2 });
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Edit Grok environment" }),
    );
    const dialog = screen.getByRole("dialog", { name: "Edit provider" });
    expect(within(dialog).getByLabelText(/^API key/)).not.toBeRequired();
    expect(
      within(dialog).getByText(/keep the existing environment credential/i),
    ).toBeVisible();
    await user.click(
      within(dialog).getByRole("button", { name: "Save provider" }),
    );

    await waitFor(() =>
      expect(api.updateSimple).toHaveBeenCalledWith("grokbuild", "grok-env", {
        expectedRevision: 1,
        name: "Grok environment",
        values: {
          baseUrl: "https://api.x.ai/v1",
          apiKey: "",
          model: "grok-4",
        },
      }),
    );
  });

  it("edits and deletes a stored provider without switching live config", async () => {
    const user = userEvent.setup();
    api.list.mockResolvedValue([workProvider]);
    api.updateSimple.mockResolvedValue({
      ...workProvider,
      revision: 2,
      name: "Primary",
    });
    render(<App />);

    await user.click(await screen.findByRole("button", { name: "Edit Work" }));
    const editDialog = screen.getByRole("dialog", { name: "Edit provider" });
    await user.clear(within(editDialog).getByLabelText("Provider name"));
    await user.type(
      within(editDialog).getByLabelText("Provider name"),
      "Primary",
    );
    await user.click(
      within(editDialog).getByRole("button", { name: "Save provider" }),
    );

    expect(
      await screen.findByRole("heading", { name: "Primary" }),
    ).toBeVisible();
    expect(api.updateSimple).toHaveBeenCalledWith("claude", "provider-1", {
      expectedRevision: 1,
      name: "Primary",
      values: {
        apiKey: "secret",
        baseUrl: "https://proxy.example.com",
        model: "",
      },
    });

    await user.click(screen.getByRole("button", { name: "Delete Primary" }));
    const deleteDialog = screen.getByRole("alertdialog", {
      name: "Delete provider?",
    });
    expect(
      within(deleteDialog).getByText(
        /will be removed from the provider catalog/,
      ),
    ).toHaveClass("break-words");
    await user.click(
      within(deleteDialog).getByRole("button", { name: "Delete provider" }),
    );

    await waitFor(() =>
      expect(api.delete).toHaveBeenCalledWith("claude", "provider-1", 2),
    );
    expect(
      await screen.findByRole("heading", {
        name: "Add your first Claude Code provider",
      }),
    ).toBeVisible();
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Add Claude Code provider" }),
      ).toHaveFocus(),
    );
  });

  it("offers reactivation after editing the current provider", async () => {
    const user = userEvent.setup();
    let active = true;
    api.list.mockResolvedValue([workProvider]);
    api.currentProviders.mockImplementation(() =>
      Promise.resolve(active ? [current(workProvider)] : []),
    );
    api.updateSimple.mockImplementation(() => {
      active = false;
      return Promise.resolve({
        ...workProvider,
        revision: 2,
        name: "Primary",
      });
    });
    render(<App />);

    expect(
      await screen.findByRole("button", {
        name: "Work is the Claude Code user default",
      }),
    ).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Edit Work" }));
    const dialog = screen.getByRole("dialog", { name: "Edit provider" });
    await user.clear(within(dialog).getByLabelText("Provider name"));
    await user.type(within(dialog).getByLabelText("Provider name"), "Primary");
    await user.click(
      within(dialog).getByRole("button", { name: "Save provider" }),
    );

    expect(
      await screen.findByRole("button", { name: "Switch to Primary" }),
    ).toBeEnabled();
  });

  it("imports the current live provider through the host", async () => {
    const user = userEvent.setup();
    const native = nativeAdapter("claude");
    api.listAdapters.mockResolvedValue([native, ...adapters]);
    api.importNative.mockResolvedValue([
      { ...workProvider, adapter: native.reference },
    ]);
    api.list.mockResolvedValueOnce([]).mockResolvedValue([workProvider]);
    api.currentProviders
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([current(workProvider)]);
    render(<App />);

    await user.click(
      await screen.findByRole("button", {
        name: "Import Claude Code user configuration",
      }),
    );

    await waitFor(() =>
      expect(api.importNative).toHaveBeenCalledWith("claude"),
    );
    expect(await screen.findByRole("heading", { name: "Work" })).toBeVisible();
    expect(screen.getAllByText("In Use")[0]).toBeVisible();
    expect(screen.getByRole("status")).toHaveTextContent(
      "Imported the Claude Code user configuration.",
    );
  });

  it("switches a stored provider and marks it current", async () => {
    const user = userEvent.setup();
    api.list.mockResolvedValue([workProvider]);
    api.currentProviders
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([current(workProvider)]);
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Switch to Work" }),
    );

    await waitFor(() =>
      expect(api.switch).toHaveBeenCalledWith("claude", "provider-1", 1),
    );
    expect(
      screen.getByRole("button", {
        name: "Work is the Claude Code user default",
      }),
    ).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent(
      "Work is now the Claude Code user default. Project, local, or managed settings can override it.",
    );
  });

  it("does not mark a different provider revision as current", async () => {
    api.list.mockResolvedValue([workProvider]);
    api.currentProviders.mockResolvedValue([
      { id: workProvider.id, revision: workProvider.revision + 1 },
    ]);

    render(<App />);

    expect(
      await screen.findByRole("button", { name: "Switch to Work" }),
    ).toBeEnabled();
  });

  it("clears a current-provider error after a successful focus refresh", async () => {
    api.currentProviders
      .mockRejectedValueOnce(new Error("Current unavailable"))
      .mockResolvedValueOnce([]);
    render(<App />);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Current unavailable",
    );

    window.dispatchEvent(new Event("focus"));

    await waitFor(() =>
      expect(screen.queryByText("Current unavailable")).toBeNull(),
    );
  });

  it("does not interpret settings when the owning adapter is unavailable", async () => {
    const unavailable = {
      ...workProvider,
      adapter: { ...workProvider.adapter, pluginVersion: "0.0.9" },
      settings: { baseUrl: "must-not-be-rendered" },
    };
    api.list.mockResolvedValue([unavailable]);
    render(<App />);

    expect(await screen.findByText("Adapter unavailable")).toBeVisible();
    expect(screen.queryByText("must-not-be-rendered")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Edit Work" })).toBeDisabled();
  });

  it("ignores opaque fields when matching an adapter identity", async () => {
    const futureIdentity = {
      ...workProvider,
      adapter: {
        ...workProvider.adapter,
        futureAdapterField: { mode: "opaque" },
      },
      settings: { baseUrl: "must-not-be-rendered" },
    };
    api.list.mockResolvedValue([futureIdentity]);
    render(<App />);

    expect(
      await screen.findByRole("button", { name: "Edit Work" }),
    ).toBeEnabled();
    expect(screen.queryByText("Adapter unavailable")).not.toBeInTheDocument();
  });

  it("matches adapter identities without Object.hasOwn", () => {
    const hasOwn = Object.hasOwn;
    let matches = false;
    Object.defineProperty(Object, "hasOwn", {
      configurable: true,
      value: undefined,
    });
    try {
      matches = sameJsonValue(adapters[0].reference, workProvider.adapter);
    } finally {
      Object.defineProperty(Object, "hasOwn", {
        configurable: true,
        value: hasOwn,
      });
    }
    expect(matches).toBe(true);
  });

  it("shows only a credential-free endpoint origin", async () => {
    api.list.mockResolvedValue([
      {
        ...workProvider,
        settings: {
          apiKey: "secret",
          baseUrl:
            "https://user:password@proxy.example.com/v1?api_key=token#private",
        },
      },
    ]);
    render(<App />);

    expect(await screen.findByText("https://proxy.example.com")).toBeVisible();
    expect(document.body).not.toHaveTextContent("password");
    expect(document.body).not.toHaveTextContent("api_key");
    expect(document.body).not.toHaveTextContent("token");
  });

  it("imports the complete native provider batch", async () => {
    const user = userEvent.setup();
    const native = nativeAdapter("claude");
    const imported = {
      ...workProvider,
      adapter: native.reference,
    };
    api.listAdapters.mockResolvedValue([native, ...adapters]);
    api.importNative.mockResolvedValue([imported]);
    api.list.mockResolvedValueOnce([]).mockResolvedValue([imported]);
    render(<App />);

    await user.click(
      await screen.findByRole("button", {
        name: "Import Claude Code user configuration",
      }),
    );

    await waitFor(() =>
      expect(api.importNative).toHaveBeenCalledWith("claude"),
    );
    expect(await screen.findByRole("heading", { name: "Work" })).toBeVisible();
  });

  it("creates native configuration for an application without a legacy form", async () => {
    const user = userEvent.setup();
    const native = nativeAdapter("gemini");
    api.supportedApps.mockResolvedValue(coreApps(["gemini"]));
    api.listAdapters.mockResolvedValue([native]);
    api.createSimple.mockImplementation((draft) =>
      Promise.resolve({
        ...workProvider,
        id: "gemini-new",
        appId: draft.appId,
        name: draft.name,
        simpleValues: draft.values,
      }),
    );
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Add Gemini CLI provider" }),
    );
    await user.type(screen.getByLabelText("Provider name"), "Gemini Work");
    await user.type(
      screen.getByLabelText("Base URL"),
      "https://gemini.example.com",
    );
    await user.type(screen.getByLabelText("API key"), "secret");
    await user.type(screen.getByLabelText(/Model/), "gemini-test");
    await user.click(screen.getByRole("button", { name: "Save provider" }));

    await waitFor(() =>
      expect(api.createSimple).toHaveBeenCalledWith({
        appId: "gemini",
        name: "Gemini Work",
        values: {
          baseUrl: "https://gemini.example.com",
          apiKey: "secret",
          model: "gemini-test",
        },
      }),
    );
  });

  it("adds and removes an additive native provider without deleting it", async () => {
    const user = userEvent.setup();
    const native = nativeAdapter("pi");
    const provider: ProviderRecord = {
      ...workProvider,
      id: "pi-provider",
      appId: "pi",
      adapter: native.reference,
      name: "Pi Work",
      settings: { baseURL: "https://pi.example.com/v1" },
    };
    api.supportedApps.mockResolvedValue(coreApps(["pi"]));
    api.listAdapters.mockResolvedValue([native]);
    api.list.mockResolvedValue([provider]);
    let inConfig = false;
    api.currentProviders.mockImplementation((appId: string) =>
      Promise.resolve(appId === "pi" && inConfig ? [current(provider)] : []),
    );
    api.switch.mockImplementation(() => {
      inConfig = true;
      return Promise.resolve();
    });
    api.removeFromLive.mockImplementation(() => {
      inConfig = false;
      return Promise.resolve();
    });
    render(<App />);

    await user.click(
      await screen.findByRole("button", {
        name: "Add Pi Work to configuration",
      }),
    );
    await waitFor(() =>
      expect(api.switch).toHaveBeenCalledWith("pi", "pi-provider", 1),
    );

    await user.click(
      await screen.findByRole("button", {
        name: "Remove Pi Work from configuration",
      }),
    );
    await waitFor(() =>
      expect(api.removeFromLive).toHaveBeenCalledWith("pi", "pi-provider", 1),
    );
    expect(api.delete).not.toHaveBeenCalled();
  });

  it("keeps Hermes dictionary providers visibly read-only", async () => {
    const native = nativeAdapter("hermes");
    const provider: ProviderRecord = {
      ...workProvider,
      id: "managed",
      appId: "hermes",
      adapter: native.reference,
      name: "Managed",
      settings: {
        _cc_source: "providers_dict",
        base_url: "https://hermes.example.com",
      },
      liteConfigWritable: false,
    };
    api.supportedApps.mockResolvedValue(coreApps(["hermes"]));
    api.listAdapters.mockResolvedValue([native]);
    api.list.mockResolvedValue([provider]);
    api.currentProviders.mockResolvedValue([current(provider)]);
    render(<App />);

    expect(await screen.findByText("Not supported in Lite")).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Managed: Not supported in Lite" }),
    ).toBeDisabled();
    expect(screen.getByRole("button", { name: "Edit Managed" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Delete Managed" }),
    ).toBeDisabled();
  });

  it("keeps full-version-only native providers visible and read-only", async () => {
    const native = nativeAdapter("opencode");
    const provider: ProviderRecord = {
      ...workProvider,
      id: "omo",
      appId: "opencode",
      adapter: native.reference,
      name: "Oh My OpenCode",
      settings: { npm: "special" },
      liteConfigWritable: false,
    };
    api.supportedApps.mockResolvedValue(coreApps(["opencode"]));
    api.listAdapters.mockResolvedValue([native]);
    api.list.mockResolvedValue([provider]);
    render(<App />);

    expect(await screen.findByText("Not supported in Lite")).toBeVisible();
    expect(
      screen.getByRole("button", {
        name: "Oh My OpenCode: Not supported in Lite",
      }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Edit Oh My OpenCode" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Delete Oh My OpenCode" }),
    ).toBeDisabled();
  });

  it("keeps the dialog fallback modal and closes it with Escape", async () => {
    const user = userEvent.setup();
    render(<App />);

    const trigger = await screen.findByRole("button", {
      name: "Add Claude Code provider",
    });
    await user.click(trigger);
    const providerDialog = screen.getByRole("dialog", {
      name: "Add provider",
    });
    expect(providerDialog).toHaveAttribute("aria-modal", "true");
    expect(providerDialog).toHaveAccessibleDescription(
      /Simple direct configuration · Anthropic Messages/,
    );
    expect(document.querySelector("header")).toHaveAttribute("inert");
    expect(document.querySelector("main")).toHaveAttribute(
      "aria-hidden",
      "true",
    );

    await user.keyboard("{Escape}");

    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "Add provider" }),
      ).not.toBeInTheDocument(),
    );
    expect(document.querySelector("header")).not.toHaveAttribute("inert");
    expect(trigger).toHaveFocus();
  });

  it("provides minimal local settings without a plugin marketplace", async () => {
    const user = userEvent.setup();
    api.list.mockResolvedValue([workProvider]);
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Switch to Work" }),
    );
    expect(await screen.findByRole("status")).toHaveTextContent(
      "Work is now the Claude Code user default",
    );
    const settingsButton = screen.getByRole("button", {
      name: "Open settings",
    });
    await user.click(settingsButton);
    expect(screen.getByRole("heading", { name: "Settings" })).toBeVisible();
    expect(screen.getByRole("tab", { name: "General" })).toHaveFocus();

    await user.click(screen.getByRole("button", { name: "Dark" }));
    expect(document.documentElement).toHaveClass("dark");
    expect(window.localStorage.getItem("cc-switch-lite:theme")).toBe("dark");

    await user.click(
      screen.getByRole("button", {
        name: "Hide Claude Code in application switcher",
      }),
    );
    await user.click(screen.getByRole("tab", { name: "About" }));
    expect(
      screen.getByRole("heading", { name: "CC Switch Lite" }),
    ).toBeVisible();
    expect(screen.getByText("Version 0.1.0-alpha.1")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Back to providers" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Open settings" }),
      ).toHaveFocus(),
    );
    expect(
      within(
        screen.getByRole("navigation", { name: "Applications" }),
      ).queryByRole("button", { name: "Claude Code" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Open plugin marketplace" }),
    ).not.toBeInTheDocument();
  });
});
