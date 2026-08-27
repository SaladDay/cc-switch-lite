import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import type { AdapterDescriptor, ProviderRecord } from "./lib/provider-types";

const api = vi.hoisted(() => ({
  listAdapters: vi.fn(),
  list: vi.fn(),
  create: vi.fn(),
  update: vi.fn(),
  delete: vi.fn(),
}));

vi.mock("./lib/providers", async (importOriginal) => {
  const original = await importOriginal<typeof import("./lib/providers")>();
  return { ...original, providersApi: api };
});

const adapters: AdapterDescriptor[] = [
  {
    appId: "claude",
    displayName: "Claude API",
    reference: {
      pluginId: "org.cc-switch.builtin",
      pluginVersion: "0.1.0-alpha.1",
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
      pluginVersion: "0.1.0-alpha.1",
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
  appId: "claude",
  adapter: adapters[0].reference,
  name: "Work",
  settings: { apiKey: "secret", baseUrl: "https://proxy.example.com" },
};

describe("App", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.className = "";
    for (const mock of Object.values(api)) mock.mockReset();
    api.listAdapters.mockResolvedValue(adapters);
    api.list.mockResolvedValue([]);
    api.delete.mockResolvedValue(undefined);
  });

  it("shows only the two applications in the Lite boundary", async () => {
    render(<App />);

    const switcher = within(
      screen.getByRole("navigation", { name: "Applications" }),
    );
    expect(
      switcher.getByRole("button", { name: "Claude Code" }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(switcher.getByRole("button", { name: "Codex" })).toBeVisible();
    expect(switcher.getAllByRole("button")).toHaveLength(2);
    expect(
      await screen.findByRole("heading", {
        name: "Add your first Claude Code provider",
      }),
    ).toBeVisible();
  });

  it("switches the provider list and remembers the selected application", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "Codex" }));

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
        adapterId: "builtin.claude.api-key",
        name: "Work",
        settings: { apiKey: "secret", baseUrl: "" },
      }),
    );
    expect(screen.getByRole("heading", { name: "Work" })).toBeVisible();
  });

  it("edits and deletes a stored provider without switching live config", async () => {
    const user = userEvent.setup();
    api.list.mockResolvedValue([workProvider]);
    api.update.mockResolvedValue({ ...workProvider, name: "Primary" });
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
    expect(api.update).toHaveBeenCalledWith("provider-1", {
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
    await user.click(
      within(deleteDialog).getByRole("button", { name: "Delete provider" }),
    );

    await waitFor(() =>
      expect(api.delete).toHaveBeenCalledWith("claude", "provider-1"),
    );
    expect(
      await screen.findByRole("heading", {
        name: "Add your first Claude Code provider",
      }),
    ).toBeVisible();
  });

  it("applies and remembers the chosen theme", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "Use dark theme" }));

    expect(document.documentElement).toHaveClass("dark");
    expect(window.localStorage.getItem("cc-switch-lite:theme")).toBe("dark");
  });
});
