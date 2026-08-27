import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import App from "./App";

describe("App", () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.className = "";
  });

  it("shows only the two applications in the Lite boundary", () => {
    render(<App />);

    const switcher = within(
      screen.getByRole("navigation", { name: "Applications" }),
    );
    expect(
      switcher.getByRole("button", { name: "Claude Code" }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(switcher.getByRole("button", { name: "Codex" })).toBeVisible();
    expect(switcher.getAllByRole("button")).toHaveLength(2);
  });

  it("switches the empty state and remembers the selected application", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "Codex" }));

    expect(
      screen.getByRole("heading", { name: "Add your first Codex provider" }),
    ).toBeVisible();
    expect(window.localStorage.getItem("cc-switch-lite:last-app")).toBe(
      "codex",
    );
  });

  it("applies and remembers the chosen theme", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "Use dark theme" }));

    expect(document.documentElement).toHaveClass("dark");
    expect(window.localStorage.getItem("cc-switch-lite:theme")).toBe("dark");
  });
});
