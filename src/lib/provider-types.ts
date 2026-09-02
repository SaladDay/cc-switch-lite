export type AppId = string;

export interface CoreAppDescriptor {
  id: AppId;
  displayName: string;
  brandKey: string;
  configurationMode: "switch" | "additive";
  capabilities: string[];
}

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
  websiteUrl?: string;
  notes?: string;
  icon?: string;
  iconColor?: string;
  settings: Record<string, JsonValue>;
  liteConfigWritable?: boolean;
  liteSimpleEditable?: boolean;
  simpleValues?: SimpleProviderValues;
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

export type SimpleProviderField = "baseUrl" | "apiKey" | "model";

export interface SimpleProviderFieldDescriptor {
  key: SimpleProviderField;
  required: boolean;
}

export type SimpleProviderProtocol =
  | "anthropic-messages"
  | "openai-responses"
  | "google-generative-ai"
  | "openai-completions"
  | "openai-chat-completions";

export interface SimpleProviderPreset {
  id: string;
  name: string;
  websiteUrl: string;
  brandKey: string;
  baseUrl: string;
  model: string;
}

export interface SimpleProviderFormDescriptor {
  appId: string;
  defaultProtocol: SimpleProviderProtocol;
  protocolLocked: boolean;
  fields: SimpleProviderFieldDescriptor[];
  presets: SimpleProviderPreset[];
}

export interface SimpleProviderValues {
  baseUrl: string;
  apiKey: string;
  model: string;
}

export interface ProviderChanges {
  name: string;
  values: SimpleProviderValues;
  presetId?: string;
}

export interface SimpleProviderDraft {
  appId: string;
  name: string;
  values: SimpleProviderValues;
  presetId?: string;
}

export interface SimpleProviderUpdate {
  expectedRevision: number;
  name: string;
  values: SimpleProviderValues;
}

export function isNativeAdapter(reference: AdapterReference): boolean {
  return (
    reference.pluginId === "org.cc-switch.builtin" &&
    reference.adapterId.startsWith("builtin.") &&
    reference.adapterId.endsWith(".native")
  );
}

export function sameAdapterIdentity(
  left: AdapterReference,
  right: AdapterReference,
): boolean {
  return (
    left.pluginId === right.pluginId &&
    left.pluginVersion === right.pluginVersion &&
    left.adapterId === right.adapterId &&
    left.contractMajor === right.contractMajor &&
    left.schemaVersion === right.schemaVersion
  );
}

export interface CommandError {
  code: string;
  message: string;
}
