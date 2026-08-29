import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import {
  Check,
  Download,
  LoaderCircle,
  Plus,
  Settings,
  Store,
} from "lucide-react";

import { DeleteProviderDialog } from "./components/DeleteProviderDialog";
import { ImportProviderDialog } from "./components/ImportProviderDialog";
import { MarketplaceDialog } from "./components/MarketplaceDialog";
import { ProviderDialog } from "./components/ProviderDialog";
import { SettingsPage } from "./components/SettingsPage";
import { AppSwitcher } from "./components/AppSwitcher";
import {
  ProviderList,
  type ProviderListItem,
} from "./components/providers/ProviderList";
import { Button } from "./components/ui/button";
import { APPS, appDefinition, parseCoreAppCatalog } from "./lib/apps";
import {
  initialTheme,
  initialVisibleApps,
  THEME_STORAGE_KEY,
  VISIBLE_APPS_STORAGE_KEY,
  type Theme,
} from "./lib/preferences";
import type {
  AdapterDescriptor,
  AdapterReference,
  AppId,
  CoreAppDescriptor,
  CurrentProvider,
  JsonValue,
  ProviderChanges,
  ProviderRecord,
} from "./lib/provider-types";
import { isNativeAdapter, sameAdapterIdentity } from "./lib/provider-types";
import { errorMessage, providersApi } from "./lib/providers";

const APP_STORAGE_KEY = "cc-switch-lite:last-app";
const DRAG_BAR_HEIGHT = 28;
const HEADER_HEIGHT = 64;

function initialApp(): AppId {
  const stored = window.localStorage.getItem(APP_STORAGE_KEY);
  return APPS.some((app) => app.id === stored) ? (stored as AppId) : "claude";
}

export function sameJsonValue(left: JsonValue, right: JsonValue): boolean {
  if (Object.is(left, right)) return true;
  if (Array.isArray(left) || Array.isArray(right)) {
    return (
      Array.isArray(left) &&
      Array.isArray(right) &&
      left.length === right.length &&
      left.every((value, index) => sameJsonValue(value, right[index]))
    );
  }
  if (
    typeof left !== "object" ||
    left === null ||
    typeof right !== "object" ||
    right === null
  )
    return false;

  const leftKeys = Object.keys(left);
  const rightKeys = Object.keys(right);
  return (
    leftKeys.length === rightKeys.length &&
    leftKeys.every(
      (key) =>
        Object.prototype.hasOwnProperty.call(right, key) &&
        sameJsonValue(left[key], right[key]),
    )
  );
}

function endpointCandidate(value: JsonValue): string | undefined {
  if (typeof value === "string") {
    const match = value.match(
      /(?:base_url|baseUrl|baseURL)\s*[:=]\s*["']([^"']+)["']/,
    );
    return match?.[1];
  }
  if (Array.isArray(value)) {
    for (const item of value) {
      const candidate = endpointCandidate(item);
      if (candidate) return candidate;
    }
    return undefined;
  }
  if (typeof value !== "object" || value === null) return undefined;
  for (const key of ["baseUrl", "baseURL", "base_url", "apiBase"]) {
    const candidate = value[key];
    if (typeof candidate === "string" && candidate.trim()) return candidate;
  }
  for (const candidate of Object.values(value)) {
    const endpoint = endpointCandidate(candidate);
    if (endpoint) return endpoint;
  }
  return undefined;
}

function visibleEndpoint(provider: ProviderRecord): string {
  const value = endpointCandidate(provider.settings);
  if (!value) return "";
  try {
    return new URL(value).origin;
  } catch {
    return "Custom endpoint";
  }
}

function adapterMatchesProvider(
  adapter: AdapterDescriptor,
  provider: ProviderRecord,
): boolean {
  return (
    adapter.appId === provider.appId &&
    sameAdapterIdentity(adapter.reference, provider.adapter)
  );
}

export default function App() {
  const [activeApp, setActiveApp] = useState<AppId>(initialApp);
  const [appCatalog, setAppCatalog] = useState<CoreAppDescriptor[]>([]);
  const [catalogReady, setCatalogReady] = useState(false);
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [visibleApps, setVisibleApps] = useState(initialVisibleApps);
  const [adapters, setAdapters] = useState<AdapterDescriptor[]>([]);
  const [providers, setProviders] = useState<ProviderRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [adapterError, setAdapterError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [currentError, setCurrentError] = useState<string | null>(null);
  const [editing, setEditing] = useState<ProviderRecord | "new" | null>(null);
  const [marketplaceOpen, setMarketplaceOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [importDialogOpen, setImportDialogOpen] = useState(false);
  const [deleting, setDeleting] = useState<ProviderRecord | null>(null);
  const [mutationBusy, setMutationBusy] = useState(false);
  const [mutationError, setMutationError] = useState<string | null>(null);
  const [liveBusy, setLiveBusy] = useState<string | null>(null);
  const [liveError, setLiveError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [currentProviders, setCurrentProviders] = useState<CurrentProvider[]>(
    [],
  );
  const currentRequestGeneration = useRef(0);
  const activeAppRef = useRef(activeApp);
  activeAppRef.current = activeApp;
  const addProviderButtonRef = useRef<HTMLButtonElement>(null);
  const deleteButtonRefs = useRef(new Map<string, HTMLButtonElement>());
  const definition = appDefinition(activeApp);
  const isClaude = activeApp === "claude";
  const supportedApps = useMemo(
    () => appCatalog.map((app) => app.id),
    [appCatalog],
  );
  const additive =
    appCatalog.find((app) => app.id === activeApp)?.configurationMode ===
    "additive";
  const providerActionsReady =
    catalogReady && appCatalog.some((app) => app.id === activeApp);
  const currentLabel = "In Use";
  const importLabel = isClaude
    ? "Import Claude Code user configuration"
    : `Import current ${definition.label} configuration`;
  const activeAdapters = adapters.filter((item) => item.appId === activeApp);
  const nativeLiveAdapter = activeAdapters.find((item) =>
    isNativeAdapter(item.reference),
  );
  const pluginLiveAdapters = activeAdapters.filter(
    (item) => item.reference.pluginId !== "org.cc-switch.builtin",
  );
  const liveAdapters = nativeLiveAdapter
    ? [nativeLiveAdapter, ...pluginLiveAdapters]
    : activeAdapters;
  const nativeCreationAdapters = activeAdapters.filter((item) =>
    isNativeAdapter(item.reference),
  );
  const formCreationAdapters = activeAdapters.filter(
    (item) => !isNativeAdapter(item.reference),
  );
  const creationAdapters =
    activeApp === "claude" || activeApp === "codex"
      ? [...formCreationAdapters, ...nativeCreationAdapters]
      : [...nativeCreationAdapters, ...formCreationAdapters];
  const adapter = creationAdapters[0];
  const selectedVisibleApps = supportedApps.filter(
    (appId) => visibleApps[appId],
  );
  const visibleSupportedApps =
    selectedVisibleApps.length > 0
      ? selectedVisibleApps
      : supportedApps.slice(0, 1);
  const editingAdapter =
    editing === "new"
      ? adapter
      : editing
        ? adapters.find((item) => adapterMatchesProvider(item, editing))
        : undefined;
  const contentTopOffset = DRAG_BAR_HEIGHT + HEADER_HEIGHT;
  const addActionButtonClass =
    "bg-orange-500 hover:bg-orange-600 dark:bg-orange-500 dark:hover:bg-orange-600 text-white shadow-lg shadow-orange-500/30 dark:shadow-orange-500/40 rounded-full w-8 h-8";

  const reloadAdapters = useCallback(async () => {
    try {
      setAdapters(await providersApi.listAdapters());
      setAdapterError(null);
    } catch (error) {
      setAdapterError(errorMessage(error));
    }
  }, []);

  const refreshCurrent = useCallback(
    async (app: AppId, showError = true): Promise<boolean> => {
      const generation = ++currentRequestGeneration.current;
      try {
        const providers = await providersApi.currentProviders(app);
        if (
          generation === currentRequestGeneration.current &&
          activeAppRef.current === app
        ) {
          setCurrentProviders(providers);
          setCurrentError(null);
        }
        return true;
      } catch (error) {
        if (
          generation === currentRequestGeneration.current &&
          activeAppRef.current === app
        ) {
          setCurrentProviders([]);
          if (showError) setCurrentError(errorMessage(error));
        }
        return false;
      }
    },
    [],
  );

  useEffect(() => {
    const media = globalThis.matchMedia?.("(prefers-color-scheme: dark)");
    const apply = () => {
      const dark = theme === "dark" || (theme === "system" && media?.matches);
      document.documentElement.classList.toggle("dark", Boolean(dark));
    };
    apply();
    window.localStorage.setItem(THEME_STORAGE_KEY, theme);
    if (theme !== "system" || !media) return;
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);

  useEffect(() => {
    window.localStorage.setItem(
      VISIBLE_APPS_STORAGE_KEY,
      JSON.stringify(visibleApps),
    );
    if (supportedApps.length === 0 || visibleApps[activeApp]) return;
    const next = supportedApps.find((appId) => visibleApps[appId]);
    if (!next) return;
    setActiveApp(next);
    window.localStorage.setItem(APP_STORAGE_KEY, next);
  }, [activeApp, supportedApps, visibleApps]);

  useEffect(() => {
    let ignore = false;
    providersApi
      .supportedApps()
      .then((value) => {
        if (ignore) return;
        const descriptors = parseCoreAppCatalog(value);
        setAppCatalog(descriptors);
        setCatalogReady(true);
        setCatalogError(null);
        if (!descriptors.some((app) => app.id === activeAppRef.current)) {
          setActiveApp(descriptors[0].id);
        }
      })
      .catch((error: unknown) => {
        if (!ignore) {
          setCatalogReady(false);
          setCatalogError(errorMessage(error));
        }
      });
    providersApi
      .listAdapters()
      .then((items) => {
        if (!ignore) {
          setAdapters(items);
          setAdapterError(null);
        }
      })
      .catch((error: unknown) => {
        if (!ignore) setAdapterError(errorMessage(error));
      });
    return () => {
      ignore = true;
    };
  }, []);

  useEffect(() => {
    let ignore = false;
    const app = activeApp;
    const currentGeneration = ++currentRequestGeneration.current;
    setLoading(true);
    setProviders([]);
    setCurrentProviders([]);
    setLoadError(null);
    setCurrentError(null);
    Promise.allSettled([
      providersApi.list(app),
      providersApi.currentProviders(app),
    ]).then(([providerResult, currentResult]) => {
      if (ignore || activeAppRef.current !== app) return;
      if (providerResult.status === "fulfilled") {
        setProviders(providerResult.value);
      } else {
        setProviders([]);
        setLoadError(errorMessage(providerResult.reason));
      }
      if (currentGeneration === currentRequestGeneration.current) {
        if (currentResult.status === "fulfilled") {
          setCurrentProviders(currentResult.value);
          setCurrentError(null);
        } else {
          setCurrentError(errorMessage(currentResult.reason));
        }
      }
      setLoading(false);
    });
    return () => {
      ignore = true;
    };
  }, [activeApp]);

  useEffect(() => {
    const refreshWhenVisible = () => {
      if (document.visibilityState === "visible") {
        void refreshCurrent(activeApp);
      }
    };
    window.addEventListener("focus", refreshWhenVisible);
    document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      window.removeEventListener("focus", refreshWhenVisible);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
    };
  }, [activeApp, refreshCurrent]);

  useEffect(() => {
    const openSettingsShortcut = (event: KeyboardEvent) => {
      if (event.metaKey && event.key === ",") {
        event.preventDefault();
        if (!mutationBusy && liveBusy === null) setSettingsOpen(true);
      }
    };
    window.addEventListener("keydown", openSettingsShortcut);
    return () => window.removeEventListener("keydown", openSettingsShortcut);
  }, [liveBusy, mutationBusy]);

  const selectApp = (app: AppId) => {
    setActiveApp(app);
    setEditing(null);
    setDeleting(null);
    setImportDialogOpen(false);
    setMutationError(null);
    setLiveError(null);
    setCurrentError(null);
    setNotice(null);
    window.localStorage.setItem(APP_STORAGE_KEY, app);
  };

  const openEditor = (provider: ProviderRecord | "new") => {
    if (!providerActionsReady) return;
    setMutationError(null);
    setNotice(null);
    setEditing(provider);
  };

  const saveProvider = async (update: ProviderChanges) => {
    if (!editing || !providerActionsReady) return;
    setMutationBusy(true);
    setMutationError(null);
    try {
      if (editing === "new") {
        const selectedAdapter = update.adapter ?? adapter?.reference;
        if (!selectedAdapter) return;
        const created = await providersApi.create({
          appId: activeApp,
          adapter: selectedAdapter,
          name: update.name,
          settings: update.settings,
        });
        setProviders((current) => [...current, created]);
      } else {
        const updated = await providersApi.update(activeApp, editing.id, {
          expectedRevision: editing.revision,
          name: update.name,
          settings: update.settings,
        });
        setProviders((current) =>
          current.map((provider) =>
            provider.id === updated.id ? updated : provider,
          ),
        );
      }
      setEditing(null);
      setNotice(editing === "new" ? "Provider added." : "Provider updated.");
      await refreshCurrent(activeApp);
    } catch (error) {
      setMutationError(errorMessage(error));
    } finally {
      setMutationBusy(false);
    }
  };

  const deleteProvider = async () => {
    if (!deleting || !providerActionsReady) return;
    setMutationBusy(true);
    setMutationError(null);
    try {
      await providersApi.delete(activeApp, deleting.id, deleting.revision);
      const deletedIndex = providers.findIndex(
        (provider) => provider.id === deleting.id,
      );
      const remaining = providers.filter(
        (provider) => provider.id !== deleting.id,
      );
      const nextProvider =
        remaining[Math.min(Math.max(deletedIndex, 0), remaining.length - 1)];
      setProviders(remaining);
      setDeleting(null);
      await refreshCurrent(activeApp);
      window.setTimeout(() => {
        const target = nextProvider
          ? deleteButtonRefs.current.get(nextProvider.id)
          : addProviderButtonRef.current;
        target?.focus();
      }, 0);
    } catch (error) {
      setMutationError(errorMessage(error));
    } finally {
      setMutationBusy(false);
    }
  };

  const importLiveProvider = async (selectedAdapter?: AdapterReference) => {
    if (!providerActionsReady) return;
    setLiveBusy("import");
    setLiveError(null);
    setNotice(null);
    try {
      if (selectedAdapter && isNativeAdapter(selectedAdapter)) {
        await providersApi.importNative(activeApp);
      } else if (selectedAdapter) {
        await providersApi.importLive(activeApp, selectedAdapter);
      } else {
        await providersApi.importLive(activeApp);
      }
      setProviders(await providersApi.list(activeApp));
      await refreshCurrent(activeApp);
      setNotice(
        isClaude
          ? "Imported the Claude Code user configuration."
          : `Imported the current ${definition.label} configuration.`,
      );
      setImportDialogOpen(false);
    } catch (error) {
      setLiveError(errorMessage(error));
    } finally {
      setLiveBusy(null);
    }
  };

  const beginImport = () => {
    if (!providerActionsReady) return;
    setLiveError(null);
    if (liveAdapters.length > 1) {
      setImportDialogOpen(true);
      return;
    }
    if (liveAdapters.length === 0) return;
    const onlyAdapter = liveAdapters[0].reference;
    void importLiveProvider(
      isNativeAdapter(onlyAdapter) ? onlyAdapter : undefined,
    );
  };

  const switchProvider = async (provider: ProviderRecord) => {
    if (!providerActionsReady) return;
    setLiveBusy(provider.id);
    setLiveError(null);
    setNotice(null);
    try {
      await providersApi.switch(activeApp, provider.id, provider.revision);
      setProviders(await providersApi.list(activeApp));
      await refreshCurrent(activeApp);
      setNotice(
        isClaude
          ? `${provider.name} is now the Claude Code user default. Project, local, or managed settings can override it.`
          : `${provider.name} is now active for ${definition.label}.`,
      );
    } catch (error) {
      setLiveError(errorMessage(error));
    } finally {
      setLiveBusy(null);
    }
  };

  const removeProviderFromLive = async (provider: ProviderRecord) => {
    if (!providerActionsReady) return;
    setLiveBusy(provider.id);
    setLiveError(null);
    setNotice(null);
    try {
      await providersApi.removeFromLive(
        activeApp,
        provider.id,
        provider.revision,
      );
      setProviders(await providersApi.list(activeApp));
      await refreshCurrent(activeApp);
      setNotice(`${provider.name} was removed from ${definition.label}.`);
    } catch (error) {
      setLiveError(errorMessage(error));
    } finally {
      setLiveBusy(null);
    }
  };

  const providerItems: ProviderListItem[] = providers.map((provider) => {
    const providerAdapter = adapters.find((item) =>
      adapterMatchesProvider(item, provider),
    );
    const hermesManaged =
      activeApp === "hermes" &&
      provider.settings._cc_source === "providers_dict";
    const readOnly = hermesManaged || provider.liteConfigWritable === false;
    const isCurrent = currentProviders.some(
      (current) =>
        current.id === provider.id && current.revision === provider.revision,
    );
    return {
      provider,
      adapterAvailable: providerAdapter !== undefined,
      canEdit:
        providerActionsReady && providerAdapter !== undefined && !readOnly,
      canSwitch:
        providerActionsReady &&
        providerAdapter !== undefined &&
        !readOnly &&
        currentError === null,
      canRemove:
        providerActionsReady &&
        providerAdapter !== undefined &&
        isNativeAdapter(providerAdapter.reference) &&
        additive &&
        !readOnly &&
        currentError === null,
      canDelete:
        providerActionsReady &&
        !readOnly &&
        currentError === null &&
        (additive || !isCurrent),
      isAdditive: additive,
      isReadOnly: readOnly,
      readOnlyLabel: hermesManaged ? "Hermes managed" : "Not supported in Lite",
      endpoint: providerAdapter ? visibleEndpoint(provider) : "",
      isCurrent,
    };
  });

  return (
    <div
      className="flex h-screen flex-col overflow-hidden bg-background pb-4 text-foreground selection:bg-primary/30"
      style={{ overflowX: "hidden", paddingTop: contentTopOffset }}
    >
      <div
        className="fixed left-0 right-0 top-0 z-[70]"
        data-tauri-drag-region
        style={{ height: DRAG_BAR_HEIGHT }}
      />
      <header
        className="fixed z-50 w-full bg-background/80 backdrop-blur-md transition-all duration-300"
        data-tauri-drag-region
        style={{ top: DRAG_BAR_HEIGHT, height: HEADER_HEIGHT }}
      >
        <div
          className="flex h-full items-center justify-between gap-2 px-6"
          data-tauri-drag-region
        >
          <div
            className="flex items-center gap-1"
            style={{ WebkitAppRegion: "no-drag" } as CSSProperties}
          >
            <Button
              variant="ghost"
              size="icon"
              disabled={mutationBusy || liveBusy !== null}
              onClick={() => setMarketplaceOpen(true)}
              title="Plugin marketplace"
              aria-label="Open plugin marketplace"
              className="hover:bg-black/5 dark:hover:bg-white/5"
            >
              <Store className="h-4 w-4" />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              disabled={mutationBusy || liveBusy !== null}
              onClick={() => setSettingsOpen(true)}
              title="Settings"
              aria-label="Open settings"
              className="hover:bg-black/5 dark:hover:bg-white/5"
            >
              <Settings className="h-4 w-4" />
            </Button>
          </div>

          <div className="flex min-w-0 flex-1 items-center justify-end gap-1.5">
            <div className="flex min-w-0 flex-1 items-center justify-end overflow-hidden py-4">
              <AppSwitcher
                activeApp={activeApp}
                apps={visibleSupportedApps}
                disabled={mutationBusy || liveBusy !== null}
                onSwitch={selectApp}
              />
            </div>
            <div className="flex shrink-0 items-center py-4">
              <div
                className="flex shrink-0 items-center gap-1.5"
                style={{ WebkitAppRegion: "no-drag" } as CSSProperties}
              >
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={
                    !providerActionsReady ||
                    liveAdapters.length === 0 ||
                    loading ||
                    mutationBusy ||
                    liveBusy !== null
                  }
                  onClick={beginImport}
                  className="hover:bg-black/5 dark:hover:bg-white/5"
                  aria-label={importLabel}
                >
                  {liveBusy === "import" ? (
                    <LoaderCircle className="mr-2 h-4 w-4 animate-spin" />
                  ) : (
                    <Download className="mr-2 h-4 w-4" />
                  )}
                  Import
                </Button>
                <Button
                  ref={addProviderButtonRef}
                  size="icon"
                  disabled={
                    !providerActionsReady ||
                    !adapter ||
                    loading ||
                    mutationBusy ||
                    liveBusy !== null
                  }
                  onClick={() => openEditor("new")}
                  className={`ml-2 ${addActionButtonClass}`}
                  aria-label={`Add ${definition.label} provider`}
                >
                  <Plus className="h-5 w-5" />
                </Button>
              </div>
            </div>
          </div>
        </div>
      </header>

      <main className="flex min-h-0 flex-1 flex-col overflow-y-auto animate-fade-in">
        <div className="flex min-h-0 flex-1 flex-col overflow-hidden px-6">
          <div className="flex-1 overflow-y-auto overflow-x-hidden px-1 pb-12">
            <div className="space-y-4">
              {(catalogError ||
                adapterError ||
                liveError ||
                loadError ||
                currentError) && (
                <div
                  role="alert"
                  className="rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-600 dark:text-red-300"
                >
                  {catalogError ||
                    adapterError ||
                    liveError ||
                    loadError ||
                    currentError}
                </div>
              )}

              {notice && (
                <div
                  role="status"
                  className="flex items-center gap-2 rounded-xl border border-emerald-500/30 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-700 dark:text-emerald-300"
                >
                  <Check className="size-4" aria-hidden="true" />
                  {notice}
                </div>
              )}

              <ProviderList
                appId={activeApp}
                items={providerItems}
                isLoading={loading}
                emptyTitle={definition.emptyTitle}
                currentLabel={currentLabel}
                importLabel={
                  isClaude ? "Import user default" : "Import current"
                }
                disabled={
                  !providerActionsReady ||
                  !adapter ||
                  mutationBusy ||
                  liveBusy !== null
                }
                importDisabled={
                  !providerActionsReady ||
                  liveAdapters.length === 0 ||
                  mutationBusy ||
                  liveBusy !== null
                }
                busy={mutationBusy || liveBusy !== null}
                importing={liveBusy === "import"}
                switchingId={liveBusy}
                onCreate={() => openEditor("new")}
                onImport={beginImport}
                onSwitch={switchProvider}
                onRemove={removeProviderFromLive}
                onEdit={openEditor}
                onDelete={(provider) => {
                  setMutationError(null);
                  setDeleting(provider);
                }}
                setDeleteButtonRef={(providerId, element) => {
                  if (element)
                    deleteButtonRefs.current.set(providerId, element);
                  else deleteButtonRefs.current.delete(providerId);
                }}
              />
            </div>
          </div>
        </div>
      </main>

      {editing && editingAdapter && (
        <ProviderDialog
          key={editing === "new" ? `${activeApp}-new` : editing.id}
          adapters={editing === "new" ? creationAdapters : [editingAdapter]}
          provider={editing === "new" ? undefined : editing}
          busy={mutationBusy}
          error={mutationError}
          onCancel={() => setEditing(null)}
          onSave={saveProvider}
        />
      )}

      {settingsOpen && (
        <SettingsPage
          theme={theme}
          visibleApps={visibleApps}
          supportedApps={supportedApps}
          onThemeChange={setTheme}
          onVisibleAppsChange={setVisibleApps}
          onOpenMarketplace={() => {
            setSettingsOpen(false);
            setMarketplaceOpen(true);
          }}
          onClose={() => setSettingsOpen(false)}
        />
      )}

      {marketplaceOpen && (
        <MarketplaceDialog
          onCancel={() => setMarketplaceOpen(false)}
          onChanged={() => void reloadAdapters()}
        />
      )}

      {importDialogOpen && (
        <ImportProviderDialog
          adapters={liveAdapters}
          busy={liveBusy === "import"}
          error={liveError}
          onCancel={() => setImportDialogOpen(false)}
          onImport={(selectedAdapter) =>
            void importLiveProvider(selectedAdapter)
          }
        />
      )}

      {deleting && (
        <DeleteProviderDialog
          provider={deleting}
          busy={mutationBusy}
          error={mutationError}
          onCancel={() => setDeleting(null)}
          onConfirm={deleteProvider}
        />
      )}
    </div>
  );
}
