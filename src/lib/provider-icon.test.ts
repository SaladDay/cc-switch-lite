import { describe, expect, it } from "vitest";

import { resolveProviderIcon } from "./provider-icon";

describe("resolveProviderIcon", () => {
  it("keeps the full app's GrokBuild legacy icon rule", () => {
    expect(resolveProviderIcon("grokbuild", "grok", "")).toBeUndefined();
    expect(resolveProviderIcon("grokbuild", "grok", "currentColor")).toBe(
      "grok",
    );
    expect(resolveProviderIcon("codex", "grok", "")).toBe("grok");
  });
});
