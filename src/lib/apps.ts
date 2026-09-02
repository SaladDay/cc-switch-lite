import type { AppId, CoreAppDescriptor } from "./provider-types";

export interface AppDefinition {
  id: AppId;
  label: string;
  icon: string;
  emptyTitle: string;
}

export type LiteFeature = "providers" | "liveConfiguration" | "mcp" | "skills";

const FEATURE_CAPABILITIES: Record<LiteFeature, string> = {
  providers: "provider-management",
  liveConfiguration: "live-configuration",
  mcp: "mcp",
  skills: "skills",
};

export const APPS: AppDefinition[] = [
  {
    id: "claude",
    label: "Claude Code",
    icon: "claude",
    emptyTitle: "Add your first Claude Code provider",
  },
  {
    id: "claude-desktop",
    label: "Claude Desktop",
    icon: "claude",
    emptyTitle: "Add your first Claude Desktop provider",
  },
  {
    id: "codex",
    label: "Codex",
    icon: "openai",
    emptyTitle: "Add your first Codex provider",
  },
  {
    id: "gemini",
    label: "Gemini CLI",
    icon: "gemini",
    emptyTitle: "Add your first Gemini CLI provider",
  },
  {
    id: "grokbuild",
    label: "Grok Build",
    icon: "grok",
    emptyTitle: "Add your first Grok Build provider",
  },
  {
    id: "opencode",
    label: "OpenCode",
    icon: "opencode",
    emptyTitle: "Add your first OpenCode provider",
  },
  {
    id: "openclaw",
    label: "OpenClaw",
    icon: "openclaw",
    emptyTitle: "Add your first OpenClaw provider",
  },
  {
    id: "hermes",
    label: "Hermes",
    icon: "hermes",
    emptyTitle: "Add your first Hermes provider",
  },
  {
    id: "pi",
    label: "Pi",
    icon: "pi",
    emptyTitle: "Add your first Pi provider",
  },
];

export function parseCoreAppCatalog(value: unknown): CoreAppDescriptor[] {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error("Core returned an invalid application catalog");
  }

  const seenIds = new Set<string>();
  return value.map((candidate) => {
    if (
      typeof candidate !== "object" ||
      candidate === null ||
      Array.isArray(candidate)
    ) {
      throw new Error("Core returned an invalid application catalog");
    }

    const descriptor = candidate as Record<string, unknown>;
    const { id, displayName, brandKey, configurationMode, capabilities } =
      descriptor;
    if (
      typeof id !== "string" ||
      id.trim() === "" ||
      seenIds.has(id) ||
      typeof displayName !== "string" ||
      typeof brandKey !== "string" ||
      (configurationMode !== "switch" && configurationMode !== "additive") ||
      !Array.isArray(capabilities) ||
      !capabilities.every((capability) => typeof capability === "string")
    ) {
      throw new Error("Core returned an invalid application catalog");
    }
    seenIds.add(id);

    return {
      id,
      displayName,
      brandKey,
      configurationMode,
      capabilities,
    };
  });
}

export function appDefinition(
  appId: AppId,
  catalog: CoreAppDescriptor[] = [],
): AppDefinition {
  const known = APPS.find((app) => app.id === appId);
  if (known) return known;
  const descriptor = catalog.find((app) => app.id === appId);
  const label = descriptor?.displayName ?? appId;
  return {
    id: appId,
    label,
    icon: descriptor?.brandKey ?? appId,
    emptyTitle: `Add your first ${label} provider`,
  };
}

export function supportsFeature(
  catalog: CoreAppDescriptor[],
  appId: AppId,
  feature: LiteFeature,
): boolean {
  return (
    catalog
      .find((descriptor) => descriptor.id === appId)
      ?.capabilities.includes(FEATURE_CAPABILITIES[feature]) ?? false
  );
}
