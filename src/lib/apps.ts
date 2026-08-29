import type { AppId, JsonValue } from "./provider-types";

export interface AppDefinition {
  id: AppId;
  label: string;
  icon: string;
  emptyTitle: string;
  additive: boolean;
}

export const APPS: AppDefinition[] = [
  {
    id: "claude",
    label: "Claude Code",
    icon: "claude",
    emptyTitle: "Add your first Claude Code provider",
    additive: false,
  },
  {
    id: "claude-desktop",
    label: "Claude Desktop",
    icon: "claude",
    emptyTitle: "Add your first Claude Desktop provider",
    additive: false,
  },
  {
    id: "codex",
    label: "Codex",
    icon: "openai",
    emptyTitle: "Add your first Codex provider",
    additive: false,
  },
  {
    id: "gemini",
    label: "Gemini CLI",
    icon: "gemini",
    emptyTitle: "Add your first Gemini CLI provider",
    additive: false,
  },
  {
    id: "grokbuild",
    label: "Grok Build",
    icon: "grok",
    emptyTitle: "Add your first Grok Build provider",
    additive: false,
  },
  {
    id: "opencode",
    label: "OpenCode",
    icon: "opencode",
    emptyTitle: "Add your first OpenCode provider",
    additive: true,
  },
  {
    id: "openclaw",
    label: "OpenClaw",
    icon: "openclaw",
    emptyTitle: "Add your first OpenClaw provider",
    additive: true,
  },
  {
    id: "hermes",
    label: "Hermes",
    icon: "hermes",
    emptyTitle: "Add your first Hermes provider",
    additive: true,
  },
  {
    id: "pi",
    label: "Pi",
    icon: "pi",
    emptyTitle: "Add your first Pi provider",
    additive: true,
  },
];

export const APP_IDS = APPS.map((app) => app.id);

export function appDefinition(appId: AppId): AppDefinition {
  return APPS.find((app) => app.id === appId) ?? APPS[0];
}

export function isAdditiveApp(appId: AppId): boolean {
  return appDefinition(appId).additive;
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
