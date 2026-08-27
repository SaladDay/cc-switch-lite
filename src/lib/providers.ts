import { invoke } from "@tauri-apps/api/core";

import type {
  CurrentProvider,
  AdapterDescriptor,
  CommandError,
  ProviderDraft,
  ProviderRecord,
  ProviderUpdate,
} from "./provider-types";

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
  importLive: (appId: string) =>
    invoke<ProviderRecord>("import_live_provider", { appId }),
  switch: (appId: string, id: string, expectedRevision: number) =>
    invoke<void>("switch_provider", { appId, id, expectedRevision }),
  currentProviders: (appId: string) =>
    invoke<CurrentProvider[]>("current_providers", { appId }),
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
