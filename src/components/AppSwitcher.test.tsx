import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { CoreAppDescriptor } from "../lib/provider-types";
import { AppSwitcher } from "./AppSwitcher";

const apps: CoreAppDescriptor[] = [
  {
    id: "claude",
    displayName: "Claude Code",
    brandKey: "claude",
    configurationMode: "switch",
    capabilities: [],
  },
  {
    id: "codex",
    displayName: "Codex",
    brandKey: "openai",
    configurationMode: "switch",
    capabilities: [],
  },
  {
    id: "gemini",
    displayName: "Gemini",
    brandKey: "gemini",
    configurationMode: "switch",
    capabilities: [],
  },
  {
    id: "pi",
    displayName: "Pi",
    brandKey: "pi",
    configurationMode: "additive",
    capabilities: [],
  },
];

let availableWidth = 100;
const resizeObservers = new Set<TestResizeObserver>();

class TestResizeObserver implements ResizeObserver {
  readonly callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    resizeObservers.add(this);
  }

  disconnect() {
    resizeObservers.delete(this);
  }
  observe() {}
  unobserve() {}
}

function triggerResize() {
  act(() => {
    for (const item of [...resizeObservers]) item.callback([], item);
  });
}

describe("AppSwitcher", () => {
  beforeEach(() => {
    availableWidth = 100;
    resizeObservers.clear();
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(40);
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(
      () => availableWidth,
    );
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("moves applications that do not fit into the labelled overflow popover", async () => {
    const user = userEvent.setup();
    render(
      <div>
        <AppSwitcher
          activeApp="claude"
          apps={apps}
          onSwitch={() => undefined}
        />
      </div>,
    );

    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Codex" }),
      ).not.toBeInTheDocument(),
    );
    await user.click(screen.getByRole("button", { name: "More applications" }));

    expect(
      await screen.findByRole("dialog", { name: "More applications" }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Codex" })).toBeVisible();
  });

  it("keeps the active application visible when it would otherwise overflow", async () => {
    render(
      <div>
        <AppSwitcher activeApp="codex" apps={apps} onSwitch={() => undefined} />
      </div>,
    );

    expect(await screen.findByRole("button", { name: "Codex" })).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Claude Code" }),
    ).not.toBeInTheDocument();
  });

  it("preserves keyboard focus when applications enter or leave overflow", async () => {
    availableWidth = 200;
    render(
      <div>
        <AppSwitcher
          activeApp="claude"
          apps={apps}
          onSwitch={() => undefined}
        />
      </div>,
    );
    const gemini = await screen.findByRole("button", { name: "Gemini CLI" });
    gemini.focus();

    availableWidth = 80;
    triggerResize();
    const more = await screen.findByRole("button", {
      name: "More applications",
    });
    expect(more).toHaveFocus();

    availableWidth = 200;
    triggerResize();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Claude Code" })).toHaveFocus(),
    );
  });

  it("restores focus from the open popover and does not reopen after resizing", async () => {
    const user = userEvent.setup();
    render(
      <div>
        <AppSwitcher
          activeApp="claude"
          apps={apps}
          onSwitch={() => undefined}
        />
      </div>,
    );
    await user.click(screen.getByRole("button", { name: "More applications" }));
    const overflowCodex = await screen.findByRole("button", { name: "Codex" });
    overflowCodex.focus();

    availableWidth = 200;
    triggerResize();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Codex" })).toHaveFocus(),
    );
    expect(
      screen.queryByRole("dialog", { name: "More applications" }),
    ).not.toBeInTheDocument();

    availableWidth = 80;
    triggerResize();
    await screen.findByRole("button", { name: "More applications" });
    expect(
      screen.queryByRole("dialog", { name: "More applications" }),
    ).not.toBeInTheDocument();
  });

  it("keeps focus on an overflow application that remains in the popover", async () => {
    const user = userEvent.setup();
    render(
      <div>
        <AppSwitcher
          activeApp="claude"
          apps={apps}
          onSwitch={() => undefined}
        />
      </div>,
    );
    await user.click(screen.getByRole("button", { name: "More applications" }));
    const gemini = await screen.findByRole("button", { name: "Gemini CLI" });
    gemini.focus();

    availableWidth = 140;
    triggerResize();

    expect(gemini).toHaveFocus();
    expect(
      screen.getByRole("dialog", { name: "More applications" }),
    ).toBeVisible();
  });

  it("disables applications inside an already open overflow popover", async () => {
    const user = userEvent.setup();
    const onSwitch = vi.fn();
    const { rerender } = render(
      <div>
        <AppSwitcher activeApp="claude" apps={apps} onSwitch={onSwitch} />
      </div>,
    );
    await user.click(screen.getByRole("button", { name: "More applications" }));

    rerender(
      <div>
        <AppSwitcher
          activeApp="claude"
          apps={apps}
          disabled
          onSwitch={onSwitch}
        />
      </div>,
    );
    const codex = await screen.findByRole("button", { name: "Codex" });
    expect(codex).toBeDisabled();
    await user.click(codex);
    expect(onSwitch).not.toHaveBeenCalled();
  });
});
