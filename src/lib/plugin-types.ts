import type { AdapterReference, AppId, FormField } from "./provider-types";

export interface TrustedPublisherKey {
  publisherId: string;
  keyId: string;
  publicKey: string;
}

export interface RegistrySource {
  id: string;
  revision: number;
  label: string;
  indexUrl: string;
  enabled: boolean;
  trustedPublishers: TrustedPublisherKey[];
}

export interface RegistryDraft {
  id?: string;
  expectedRevision?: number;
  label: string;
  indexUrl: string;
  enabled: boolean;
  trustedPublishers: TrustedPublisherKey[];
}

export interface PluginCapability {
  kind:
    | "readClaudeSettings"
    | "writeClaudeSettings"
    | "readCodexConfig"
    | "writeCodexConfig"
    | "readCodexAuth";
}

export interface PluginManifest {
  id: string;
  version: string;
  name: string;
  description: string;
  publisher: { id: string; keyId: string; algorithm: "ed25519" };
  adapters: {
    appId: AppId;
    adapterId: string;
    displayName: string;
    schemaVersion: number;
    fields: FormField[];
  }[];
  capabilities: PluginCapability[];
}

export interface MarketplacePlugin {
  registryId: string;
  registryRevision: number;
  registryLabel: string;
  manifest: PluginManifest;
  manifestSha256: string;
  packageSha256: string;
  publisherKeySha256: string;
  installed?: InstalledPlugin;
  permissions: string[];
}

export interface MarketplaceCatalog {
  plugins: MarketplacePlugin[];
  failures: {
    registryId: string;
    registryLabel: string;
    message: string;
  }[];
}

export interface InstalledPlugin {
  id: string;
  version: string;
  registryId: string;
  packageSha256: string;
  manifestSha256: string;
  publisher: { id: string; keyId: string; algorithm: "ed25519" };
  publisherKeySha256: string;
  grantedCapabilities: PluginCapability[];
}

export interface PluginAdapterChoice {
  appId: AppId;
  reference: AdapterReference;
}
