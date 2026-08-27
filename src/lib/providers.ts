import { invoke } from "@tauri-apps/api/core";

import type {
  CurrentProvider,
  AdapterDescriptor,
  AdapterReference,
  CommandError,
  ProviderDraft,
  ProviderRecord,
  ProviderUpdate,
} from "./provider-types";
import type {
  InstalledPlugin,
  MarketplaceCatalog,
  PluginCapability,
  RegistryDraft,
  RegistrySource,
} from "./plugin-types";

export const providersApi = {
  listAdapters: () => invoke<AdapterDescriptor[]>("list_provider_adapters"),
  list: (appId: string) =>
    invoke<ProviderRecord[]>("list_providers", { appId }),
  create: (provider: ProviderDraft) =>
    invoke<ProviderRecord>("create_provider", { provider }),
  update: (id: string, provider: ProviderUpdate) =>
    invoke<ProviderRecord>("update_provider", { id, provider }),
  delete: (appId: string, id: string, expectedRevision: number) =>
    invoke<void>("delete_provider", { appId, id, expectedRevision }),
  importLive: (appId: string, adapter: AdapterReference | null = null) =>
    invoke<ProviderRecord>("import_live_provider", { appId, adapter }),
  switch: (appId: string, id: string, expectedRevision: number) =>
    invoke<void>("switch_provider", { appId, id, expectedRevision }),
  currentProviders: (appId: string) =>
    invoke<CurrentProvider[]>("current_providers", { appId }),
};

export const pluginsApi = {
  listRegistries: () => invoke<RegistrySource[]>("list_plugin_registries"),
  saveRegistry: (registry: RegistryDraft) =>
    invoke<RegistrySource>("save_plugin_registry", { registry }),
  removeRegistry: (id: string, expectedRevision: number) =>
    invoke<void>("remove_plugin_registry", { id, expectedRevision }),
  refresh: () => invoke<MarketplaceCatalog>("refresh_plugin_marketplace"),
  listInstalled: () => invoke<InstalledPlugin[]>("list_installed_plugins"),
  install: (
    plugin: {
      registryId: string;
      registryRevision: number;
      pluginId: string;
      version: string;
      manifestSha256: string;
      packageSha256: string;
      publisherKeySha256: string;
    },
    approvedCapabilities: PluginCapability[],
  ) =>
    invoke<InstalledPlugin>("install_plugin", {
      plugin,
      approvedCapabilities,
    }),
  uninstall: (pluginId: string) =>
    invoke<void>("uninstall_plugin", { pluginId }),
};

export function errorMessage(error: unknown): string {
  if (typeof error === "object" && error !== null && "message" in error) {
    const message = (error as CommandError).message;
    if (typeof message === "string") return message;
  }
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return "Something went wrong. Try again.";
}
