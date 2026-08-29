import type { AppId, CoreAppDescriptor, JsonValue } from "./provider-types";

export interface AppDefinition {
  id: AppId;
  label: string;
  icon: string;
  emptyTitle: string;
}

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

export const APP_IDS = APPS.map((app) => app.id);

export function parseCoreAppCatalog(value: unknown): CoreAppDescriptor[] {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error("Core returned an invalid application catalog");
  }

  const knownIds = new Set<string>(APP_IDS);
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
      !knownIds.has(id) ||
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
      id: id as AppId,
      displayName,
      brandKey,
      configurationMode,
      capabilities,
    };
  });
}

export function appDefinition(appId: AppId): AppDefinition {
  return APPS.find((app) => app.id === appId) ?? APPS[0];
}

const NATIVE_SETTINGS_TEMPLATES: Record<AppId, Record<string, JsonValue>> = {
  claude: { env: {} },
  "claude-desktop": {
    env: { ANTHROPIC_BASE_URL: "", ANTHROPIC_AUTH_TOKEN: "" },
  },
  codex: { auth: {}, config: "" },
  gemini: {
    env: {
      GOOGLE_GEMINI_BASE_URL: "",
      GEMINI_API_KEY: "",
      GEMINI_MODEL: "gemini-3.6-flash",
    },
  },
  grokbuild: {
    config:
      '[models]\ndefault = "grok-4.5"\n\n[model."grok-4.5"]\nmodel = "grok-4.5"\nbase_url = ""\nname = "Custom"\napi_key = ""\napi_backend = "responses"\ncontext_window = 500000\n',
  },
  opencode: {
    npm: "@ai-sdk/openai-compatible",
    options: { baseURL: "", apiKey: "", setCacheKey: true },
    models: {},
  },
  openclaw: {
    baseUrl: "",
    apiKey: "",
    api: "openai-completions",
    models: [],
  },
  hermes: {
    name: "",
    base_url: "",
    api_key: "",
    api_mode: "chat_completions",
    models: [],
  },
  pi: { baseUrl: "", apiKey: "", models: [] },
};

export function nativeSettingsTemplate(
  appId: string,
): Record<string, JsonValue> {
  return NATIVE_SETTINGS_TEMPLATES[appId as AppId] ?? {};
}
