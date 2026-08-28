import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import App, { sameJsonValue } from "./App";
import type {
  AdapterDescriptor,
  CurrentProvider,
  ProviderRecord,
} from "./lib/provider-types";
import type { MarketplacePlugin } from "./lib/plugin-types";

const api = vi.hoisted(() => ({
  supportedApps: vi.fn(),
  listAdapters: vi.fn(),
  list: vi.fn(),
  create: vi.fn(),
  update: vi.fn(),
  delete: vi.fn(),
  importLive: vi.fn(),
  switch: vi.fn(),
  currentProviders: vi.fn(),
}));

const pluginApi = vi.hoisted(() => ({
  listRegistries: vi.fn(),
  saveRegistry: vi.fn(),
  removeRegistry: vi.fn(),
  refresh: vi.fn(),
  listInstalled: vi.fn(),
  install: vi.fn(),
  uninstall: vi.fn(),
}));

vi.mock("./lib/providers", async (importOriginal) => {
  const original = await importOriginal<typeof import("./lib/providers")>();
  return { ...original, providersApi: api, pluginsApi: pluginApi };
});

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
};

const marketplacePlugin: MarketplacePlugin = {
  registryId: "registry-1",
  registryRevision: 3,
  registryLabel: "Community",
  manifestSha256: "a".repeat(64),
  packageSha256: "b".repeat(64),
  publisherKeySha256: "d".repeat(64),
  permissions: [
    "Read Claude Code user settings",
    "Change Claude Code provider routing",
  ],
  manifest: {
    id: "dev.example.claude",
    version: "1.0.0",
    name: "Example Claude adapter",
    description: "Adds an example provider form.",
    publisher: {
      id: "dev.example",
      keyId: "release-1",
      algorithm: "ed25519",
    },
    adapters: [],
    capabilities: [
      { kind: "readClaudeSettings" },
      { kind: "writeClaudeSettings" },
    ],
  },
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

describe("App", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.className = "";
    for (const mock of Object.values(api)) mock.mockReset();
    api.listAdapters.mockResolvedValue(adapters);
    api.supportedApps.mockResolvedValue([
      "claude",
      "claude-desktop",
      "codex",
      "gemini",
      "grokbuild",
      "opencode",
      "openclaw",
      "hermes",
      "pi",
    ]);
    api.list.mockResolvedValue([]);
    api.delete.mockResolvedValue(undefined);
    api.switch.mockResolvedValue(undefined);
    api.currentProviders.mockResolvedValue([]);
    for (const mock of Object.values(pluginApi)) mock.mockReset();
    pluginApi.listRegistries.mockResolvedValue([]);
    pluginApi.listInstalled.mockResolvedValue([]);
    pluginApi.refresh.mockResolvedValue({
      plugins: [],
      failures: [],
    });
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

  it("uses the core response as the application membership source", async () => {
    api.supportedApps.mockResolvedValue(["pi"]);
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

  it("announces the plugin marketplace loading state", async () => {
    const user = userEvent.setup();
    pluginApi.listRegistries.mockImplementation(
      () => new Promise<never>(() => undefined),
    );
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Open plugin marketplace" }),
    );

    expect(
      within(
        screen.getByRole("dialog", { name: "Plugin marketplace" }),
      ).getByRole("status"),
    ).toHaveTextContent("Loading plugin marketplace");
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

  it("creates a provider from the adapter-driven form", async () => {
    const user = userEvent.setup();
    api.create.mockResolvedValue(workProvider);
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
      expect(api.create).toHaveBeenCalledWith({
        appId: "claude",
        adapter: adapters[0].reference,
        name: "Work",
        settings: { apiKey: "secret", baseUrl: "" },
      }),
    );
    expect(screen.getByRole("heading", { name: "Work" })).toBeVisible();
  });

  it("creates a native provider from configuration JSON", async () => {
    const user = userEvent.setup();
    const nativeAdapter: AdapterDescriptor = {
      appId: "claude",
      displayName: "Native configuration",
      reference: {
        ...adapters[0].reference,
        adapterId: "builtin.claude.native",
      },
      fields: [],
    };
    const nativeProvider: ProviderRecord = {
      ...workProvider,
      adapter: nativeAdapter.reference,
      settings: { env: { ANTHROPIC_API_KEY: "secret" } },
    };
    api.listAdapters.mockResolvedValue([nativeAdapter]);
    api.create.mockResolvedValue(nativeProvider);
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Add Claude Code provider" }),
    );
    const dialog = screen.getByRole("dialog", { name: "Add provider" });
    await user.type(within(dialog).getByLabelText("Provider name"), "Work");
    const configuration = within(dialog).getByLabelText("Configuration JSON");
    fireEvent.change(configuration, {
      target: {
        value: JSON.stringify({ env: { ANTHROPIC_API_KEY: "secret" } }),
      },
    });
    await user.click(
      within(dialog).getByRole("button", { name: "Save provider" }),
    );

    await waitFor(() =>
      expect(api.create).toHaveBeenCalledWith({
        appId: "claude",
        adapter: nativeAdapter.reference,
        name: "Work",
        settings: { env: { ANTHROPIC_API_KEY: "secret" } },
      }),
    );
  });

  it("creates a provider with an installed plugin adapter", async () => {
    const user = userEvent.setup();
    const pluginAdapter: AdapterDescriptor = {
      ...adapters[0],
      displayName: "Example Claude adapter",
      reference: {
        ...adapters[0].reference,
        pluginId: "dev.example.claude",
        pluginVersion: "1.0.0",
        adapterId: "example.claude",
      },
    };
    api.listAdapters.mockResolvedValue([...adapters, pluginAdapter]);
    api.create.mockResolvedValue({
      ...workProvider,
      adapter: pluginAdapter.reference,
    });
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Add Claude Code provider" }),
    );
    const dialog = screen.getByRole("dialog", { name: "Add provider" });
    await user.selectOptions(within(dialog).getByLabelText("Adapter"), "1");
    await user.type(within(dialog).getByLabelText("Provider name"), "Plugin");
    await user.type(within(dialog).getByLabelText("API key"), "secret");
    await user.click(
      within(dialog).getByRole("button", { name: "Save provider" }),
    );

    await waitFor(() =>
      expect(api.create).toHaveBeenCalledWith(
        expect.objectContaining({ adapter: pluginAdapter.reference }),
      ),
    );
  });

  it("requires explicit permission approval before installing a plugin", async () => {
    const user = userEvent.setup();
    pluginApi.refresh.mockResolvedValue({
      plugins: [marketplacePlugin],
      failures: [],
    });
    pluginApi.install.mockResolvedValue({
      id: marketplacePlugin.manifest.id,
      version: marketplacePlugin.manifest.version,
      registryId: marketplacePlugin.registryId,
      packageSha256: marketplacePlugin.packageSha256,
      manifestSha256: marketplacePlugin.manifestSha256,
      publisher: marketplacePlugin.manifest.publisher,
      publisherKeySha256: marketplacePlugin.publisherKeySha256,
      grantedCapabilities: marketplacePlugin.manifest.capabilities,
    });
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Open plugin marketplace" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Plugin marketplace",
    });
    expect(dialog).toHaveAccessibleDescription(
      "Signed provider adapters from sources you trust.",
    );
    const install = within(dialog).getByRole("button", { name: "Install" });
    expect(install).toBeDisabled();
    await user.click(
      within(dialog).getByLabelText(
        "Approve exactly these permissions for this signed version",
      ),
    );
    await user.click(install);

    await waitFor(() =>
      expect(pluginApi.install).toHaveBeenCalledWith(
        {
          registryId: marketplacePlugin.registryId,
          registryRevision: marketplacePlugin.registryRevision,
          pluginId: marketplacePlugin.manifest.id,
          version: marketplacePlugin.manifest.version,
          manifestSha256: marketplacePlugin.manifestSha256,
          packageSha256: marketplacePlugin.packageSha256,
          publisherKeySha256: marketplacePlugin.publisherKeySha256,
        },
        marketplacePlugin.manifest.capabilities,
      ),
    );
  });

  it("clears permission approval when the signed manifest changes", async () => {
    const user = userEvent.setup();
    const changed = {
      ...marketplacePlugin,
      manifestSha256: "c".repeat(64),
    };
    pluginApi.refresh
      .mockResolvedValueOnce({ plugins: [marketplacePlugin], failures: [] })
      .mockResolvedValueOnce({ plugins: [changed], failures: [] });
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Open plugin marketplace" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Plugin marketplace",
    });
    const approval = within(dialog).getByLabelText(
      "Approve exactly these permissions for this signed version",
    );
    await user.click(approval);
    expect(approval).toBeChecked();

    await user.click(within(dialog).getByRole("button", { name: "Refresh" }));

    await waitFor(() => expect(pluginApi.refresh).toHaveBeenCalledTimes(2));
    expect(
      within(dialog).getByLabelText(
        "Approve exactly these permissions for this signed version",
      ),
    ).not.toBeChecked();
    expect(
      within(dialog).getByRole("button", { name: "Install" }),
    ).toBeDisabled();
  });

  it("does not treat a different source as a plugin update", async () => {
    const user = userEvent.setup();
    const installed = {
      id: marketplacePlugin.manifest.id,
      version: "0.9.0",
      registryId: "registry-other",
      packageSha256: "e".repeat(64),
      manifestSha256: "f".repeat(64),
      publisher: marketplacePlugin.manifest.publisher,
      publisherKeySha256: marketplacePlugin.publisherKeySha256,
      grantedCapabilities: marketplacePlugin.manifest.capabilities,
    };
    pluginApi.listInstalled.mockResolvedValue([installed]);
    pluginApi.refresh.mockResolvedValue({
      plugins: [{ ...marketplacePlugin, installed }],
      failures: [],
    });
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Open plugin marketplace" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Plugin marketplace",
    });

    expect(
      within(dialog).getByRole("button", { name: "ID collision" }),
    ).toBeDisabled();
    expect(
      within(dialog).queryByLabelText(
        "Approve exactly these permissions for this signed version",
      ),
    ).not.toBeInTheDocument();
  });

  it("can remove an installed plugin after its source disappears", async () => {
    const user = userEvent.setup();
    pluginApi.listInstalled.mockResolvedValue([
      {
        id: marketplacePlugin.manifest.id,
        version: marketplacePlugin.manifest.version,
        registryId: marketplacePlugin.registryId,
        packageSha256: marketplacePlugin.packageSha256,
        manifestSha256: marketplacePlugin.manifestSha256,
        publisher: marketplacePlugin.manifest.publisher,
        publisherKeySha256: marketplacePlugin.publisherKeySha256,
        grantedCapabilities: marketplacePlugin.manifest.capabilities,
      },
    ]);
    render(<App />);

    await user.click(
      await screen.findByRole("button", { name: "Open plugin marketplace" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Plugin marketplace",
    });
    expect(
      within(dialog).getByText(marketplacePlugin.manifest.id),
    ).toBeVisible();

    await user.click(within(dialog).getByRole("button", { name: "Remove" }));

    await waitFor(() =>
      expect(pluginApi.uninstall).toHaveBeenCalledWith(
        marketplacePlugin.manifest.id,
      ),
    );
  });

  it("edits and deletes a stored provider without switching live config", async () => {
    const user = userEvent.setup();
    api.list.mockResolvedValue([workProvider]);
    api.update.mockResolvedValue({
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
    expect(api.update).toHaveBeenCalledWith("claude", "provider-1", {
      expectedRevision: 1,
      name: "Primary",
      settings: {
        apiKey: "secret",
        baseUrl: "https://proxy.example.com",
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

  it("imports the current live provider through the host", async () => {
    const user = userEvent.setup();
    api.importLive.mockResolvedValue(workProvider);
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

    await waitFor(() => expect(api.importLive).toHaveBeenCalledWith("claude"));
    expect(await screen.findByRole("heading", { name: "Work" })).toBeVisible();
    expect(screen.getAllByText("In Use")[0]).toBeVisible();
    expect(screen.getByRole("status")).toHaveTextContent(
      "Imported the Claude Code user configuration.",
    );
  });

  it("imports through a selected plugin adapter", async () => {
    const user = userEvent.setup();
    const pluginAdapter: AdapterDescriptor = {
      ...adapters[0],
      displayName: "Example Claude adapter",
      reference: {
        ...adapters[0].reference,
        pluginId: marketplacePlugin.manifest.id,
        pluginVersion: marketplacePlugin.manifest.version,
        adapterId: "example.claude",
      },
    };
    const imported = { ...workProvider, adapter: pluginAdapter.reference };
    api.listAdapters.mockResolvedValue([...adapters, pluginAdapter]);
    api.importLive.mockResolvedValue(imported);
    api.list.mockResolvedValueOnce([]).mockResolvedValue([imported]);
    render(<App />);

    await user.click(
      await screen.findByRole("button", {
        name: "Import Claude Code user configuration",
      }),
    );
    const dialog = screen.getByRole("dialog", { name: "Import provider" });
    await user.selectOptions(within(dialog).getByLabelText("Adapter"), "1");
    await user.click(
      within(dialog).getByRole("button", { name: "Import provider" }),
    );

    await waitFor(() =>
      expect(api.importLive).toHaveBeenCalledWith(
        "claude",
        pluginAdapter.reference,
      ),
    );
    expect(await screen.findByRole("heading", { name: "Work" })).toBeVisible();
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

  it("treats an adapter with opaque identity fields as unavailable", async () => {
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

    expect(await screen.findByText("Adapter unavailable")).toBeVisible();
    expect(screen.queryByText("must-not-be-rendered")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Edit Work" })).toBeDisabled();
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
      /Claude API\. Credentials remain in CC Switch Lite/,
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

  it("applies and remembers the chosen theme", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "Use dark theme" }));

    expect(document.documentElement).toHaveClass("dark");
    expect(window.localStorage.getItem("cc-switch-lite:theme")).toBe("dark");
  });
});
