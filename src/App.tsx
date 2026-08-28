import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import {
  Check,
  Download,
  LoaderCircle,
  Moon,
  Plus,
  Store,
  Sun,
} from "lucide-react";

import { DeleteProviderDialog } from "./components/DeleteProviderDialog";
import { ImportProviderDialog } from "./components/ImportProviderDialog";
import { MarketplaceDialog } from "./components/MarketplaceDialog";
import { ProviderDialog } from "./components/ProviderDialog";
import { AppSwitcher } from "./components/AppSwitcher";
import {
  ProviderList,
  type ProviderListItem,
} from "./components/providers/ProviderList";
import { Button } from "./components/ui/button";
import type {
  AdapterDescriptor,
  AdapterReference,
  AppId,
  CurrentProvider,
  JsonValue,
  ProviderChanges,
  ProviderRecord,
} from "./lib/provider-types";
import { errorMessage, providersApi } from "./lib/providers";

interface AppDefinition {
  id: AppId;
  label: string;
  emptyTitle: string;
}

const APPS: AppDefinition[] = [
  {
    id: "claude",
    label: "Claude Code",
    emptyTitle: "Add your first Claude Code provider",
  },
  {
    id: "codex",
    label: "Codex",
    emptyTitle: "Add your first Codex provider",
  },
];

const APP_STORAGE_KEY = "cc-switch-lite:last-app";
const THEME_STORAGE_KEY = "cc-switch-lite:theme";
const DRAG_BAR_HEIGHT = 28;
const HEADER_HEIGHT = 64;

function initialApp(): AppId {
  const stored = window.localStorage.getItem(APP_STORAGE_KEY);
  return stored === "codex" ? "codex" : "claude";
}

function initialDarkMode(): boolean {
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
  if (stored) return stored === "dark";
  return (
    globalThis.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false
  );
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

function visibleEndpoint(
  adapter: AdapterDescriptor,
  provider: ProviderRecord,
): string {
  const field = adapter.fields.find(
    (candidate) => candidate.key === "baseUrl" && candidate.kind !== "secret",
  );
  if (!field) return "";
  const value = provider.settings.baseUrl;
  if (typeof value !== "string" || value === "") return "";
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
    sameJsonValue(adapter.reference, provider.adapter)
  );
}

export default function App() {
  const [activeApp, setActiveApp] = useState<AppId>(initialApp);
  const [isDark, setIsDark] = useState(initialDarkMode);
  const [adapters, setAdapters] = useState<AdapterDescriptor[]>([]);
  const [providers, setProviders] = useState<ProviderRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [adapterError, setAdapterError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [currentError, setCurrentError] = useState<string | null>(null);
  const [editing, setEditing] = useState<ProviderRecord | "new" | null>(null);
  const [marketplaceOpen, setMarketplaceOpen] = useState(false);
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
  const definition = APPS.find((app) => app.id === activeApp) ?? APPS[0];
  const isClaude = activeApp === "claude";
  const currentLabel = "In Use";
  const importLabel = isClaude
    ? "Import Claude Code user configuration"
    : `Import current ${definition.label} configuration`;
  const activeAdapters = adapters.filter((item) => item.appId === activeApp);
  const adapter =
    activeAdapters.find(
      (item) => item.reference.pluginId === "org.cc-switch.builtin",
    ) ?? activeAdapters[0];
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
    document.documentElement.classList.toggle("dark", isDark);
    window.localStorage.setItem(THEME_STORAGE_KEY, isDark ? "dark" : "light");
  }, [isDark]);

  useEffect(() => {
    let ignore = false;
    providersApi
      .listAdapters()
      .then((items) => {
        if (!ignore) setAdapters(items);
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
    setMutationError(null);
    setNotice(null);
    setEditing(provider);
  };

  const saveProvider = async (update: ProviderChanges) => {
    if (!editing) return;
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
        const updated = await providersApi.update(editing.id, {
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
    if (!deleting) return;
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
    setLiveBusy("import");
    setLiveError(null);
    setNotice(null);
    try {
      const imported = selectedAdapter
        ? await providersApi.importLive(activeApp, selectedAdapter)
        : await providersApi.importLive(activeApp);
      setProviders((current) => [...current, imported]);
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
    setLiveError(null);
    if (activeAdapters.length > 1) {
      setImportDialogOpen(true);
      return;
    }
    void importLiveProvider();
  };

  const switchProvider = async (provider: ProviderRecord) => {
    setLiveBusy(provider.id);
    setLiveError(null);
    setNotice(null);
    try {
      await providersApi.switch(activeApp, provider.id, provider.revision);
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

  const providerItems: ProviderListItem[] = providers.map((provider) => {
    const providerAdapter = adapters.find((item) =>
      adapterMatchesProvider(item, provider),
    );
    return {
      provider,
      adapterAvailable: providerAdapter !== undefined,
      endpoint: providerAdapter
        ? visibleEndpoint(providerAdapter, provider)
        : "",
      isCurrent: currentProviders.some(
        (current) =>
          current.id === provider.id && current.revision === provider.revision,
      ),
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
              onClick={() => setIsDark((current) => !current)}
              title={isDark ? "Use light theme" : "Use dark theme"}
              aria-label={isDark ? "Use light theme" : "Use dark theme"}
              className="hover:bg-black/5 dark:hover:bg-white/5"
            >
              {isDark ? (
                <Sun className="h-4 w-4" />
              ) : (
                <Moon className="h-4 w-4" />
              )}
            </Button>
          </div>

          <div className="flex min-w-0 flex-1 items-center justify-end gap-1.5">
            <div className="flex min-w-0 flex-1 items-center justify-end overflow-hidden py-4">
              <AppSwitcher
                activeApp={activeApp}
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
                    !adapter || loading || mutationBusy || liveBusy !== null
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
                    !adapter || loading || mutationBusy || liveBusy !== null
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
              {(adapterError || liveError || loadError || currentError) && (
                <div
                  role="alert"
                  className="rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-600 dark:text-red-300"
                >
                  {adapterError || liveError || loadError || currentError}
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
                disabled={!adapter || mutationBusy || liveBusy !== null}
                busy={mutationBusy || liveBusy !== null}
                importing={liveBusy === "import"}
                switchingId={liveBusy}
                onCreate={() => openEditor("new")}
                onImport={beginImport}
                onSwitch={switchProvider}
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
          adapters={editing === "new" ? activeAdapters : [editingAdapter]}
          provider={editing === "new" ? undefined : editing}
          busy={mutationBusy}
          error={mutationError}
          onCancel={() => setEditing(null)}
          onSave={saveProvider}
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
          adapters={activeAdapters}
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
