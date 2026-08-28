export type AppId =
  | "claude"
  | "claude-desktop"
  | "codex"
  | "gemini"
  | "grokbuild"
  | "opencode"
  | "openclaw"
  | "hermes"
  | "pi";

export type JsonValue =
  string | number | boolean | null | JsonValue[] | { [key: string]: JsonValue };

export interface AdapterReference {
  [key: string]: JsonValue;
  pluginId: string;
  pluginVersion: string;
  adapterId: string;
  contractMajor: number;
  schemaVersion: number;
}

export interface ProviderRecord {
  id: string;
  revision: number;
  appId: string;
  adapter: AdapterReference;
  name: string;
  settings: Record<string, JsonValue>;
}

export interface CurrentProvider {
  id: string;
  revision: number;
}

export type FieldKind = "text" | "url" | "secret";

export interface FormField {
  key: string;
  label: string;
  kind: FieldKind;
  required: boolean;
  placeholder: string;
  help: string;
}

export interface AdapterDescriptor {
  appId: string;
  displayName: string;
  reference: AdapterReference;
  fields: FormField[];
}

export interface ProviderDraft {
  appId: string;
  adapter: AdapterReference;
  name: string;
  settings: Record<string, string>;
}

export interface ProviderChanges {
  name: string;
  settings: Record<string, string>;
  adapter?: AdapterReference;
}

export interface ProviderUpdate {
  expectedRevision: number;
  name: string;
  settings: Record<string, string>;
}

export interface CommandError {
  code: string;
  message: string;
}
