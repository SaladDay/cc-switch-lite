import { useCallback, useEffect, useRef, useState } from "react";
import {
  Asterisk,
  Bot,
  Check,
  Download,
  KeyRound,
  LoaderCircle,
  Moon,
  Pencil,
  Plus,
  Store,
  Sun,
  Trash2,
  type LucideIcon,
} from "lucide-react";

import { DeleteProviderDialog } from "./components/DeleteProviderDialog";
import { ImportProviderDialog } from "./components/ImportProviderDialog";
import { MarketplaceDialog } from "./components/MarketplaceDialog";
import { ProviderDialog } from "./components/ProviderDialog";
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
  icon: LucideIcon;
  iconClassName: string;
}

const APPS: AppDefinition[] = [
  {
    id: "claude",
    label: "Claude Code",
    emptyTitle: "Add your first Claude Code provider",
    icon: Asterisk,
    iconClassName: "text-[#d97757]",
  },
  {
    id: "codex",
    label: "Codex",
    emptyTitle: "Add your first Codex provider",
    icon: Bot,
    iconClassName: "text-foreground",
  },
];

const APP_STORAGE_KEY = "cc-switch-lite:last-app";
const THEME_STORAGE_KEY = "cc-switch-lite:theme";

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
  const currentLabel = isClaude ? "User default" : "Current";
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
  const ActiveIcon = definition.icon;

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

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="grid h-20 grid-cols-[1fr_auto_1fr] items-center border-b border-border px-6">
        <div className="flex min-w-0 items-center gap-3">
          <div className="grid size-9 shrink-0 place-items-center rounded-xl bg-primary text-sm font-bold text-primary-foreground shadow-sm">
            CC
          </div>
          <div className="min-w-0">
            <h1 className="truncate text-lg font-semibold tracking-tight">
              CC Switch Lite
            </h1>
            <p className="text-xs text-muted-foreground">
              Provider switching, kept small
            </p>
          </div>
        </div>

        <nav
          className="inline-flex rounded-xl bg-muted p-1"
          aria-label="Applications"
        >
          {APPS.map((app) => {
            const Icon = app.icon;
            const selected = app.id === activeApp;
            return (
              <button
                key={app.id}
                type="button"
                disabled={mutationBusy || liveBusy !== null}
                aria-pressed={selected}
                onClick={() => selectApp(app.id)}
                className={`inline-flex h-9 cursor-pointer items-center gap-2 rounded-lg px-4 text-sm font-medium transition-colors duration-200 disabled:cursor-not-allowed disabled:opacity-60 ${
                  selected
                    ? "bg-background text-foreground shadow-sm"
                    : "text-muted-foreground hover:bg-background/60 hover:text-foreground"
                }`}
              >
                <Icon
                  className={`size-4 ${app.iconClassName}`}
                  strokeWidth={2.2}
                />
                {app.label}
              </button>
            );
          })}
        </nav>

        <div className="flex items-center justify-end gap-2">
          <button
            type="button"
            onClick={() => setIsDark((current) => !current)}
            className="inline-flex size-9 cursor-pointer items-center justify-center rounded-lg text-muted-foreground transition-colors duration-200 hover:bg-muted hover:text-foreground"
            aria-label={isDark ? "Use light theme" : "Use dark theme"}
          >
            {isDark ? <Sun className="size-4" /> : <Moon className="size-4" />}
          </button>
          <button
            type="button"
            disabled={mutationBusy || liveBusy !== null}
            onClick={() => setMarketplaceOpen(true)}
            className="inline-flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-50"
            aria-label="Open plugin marketplace"
            title="Plugin marketplace"
          >
            <Store className="size-4" />
          </button>
          <button
            type="button"
            disabled={!adapter || loading || mutationBusy || liveBusy !== null}
            onClick={beginImport}
            className="ml-2 inline-flex h-10 items-center gap-2 rounded-xl border border-border px-3 text-sm font-medium transition-colors hover:bg-muted disabled:cursor-not-allowed disabled:opacity-50"
            aria-label={importLabel}
          >
            {liveBusy === "import" ? (
              <LoaderCircle className="size-4 animate-spin" />
            ) : (
              <Download className="size-4" />
            )}
            Import
          </button>
          <button
            ref={addProviderButtonRef}
            type="button"
            disabled={!adapter || loading || mutationBusy || liveBusy !== null}
            onClick={() => openEditor("new")}
            className="inline-flex h-10 items-center gap-2 rounded-xl bg-primary px-4 text-sm font-medium text-primary-foreground shadow-sm transition-opacity disabled:opacity-50"
            aria-label={`Add ${definition.label} provider`}
          >
            <Plus className="size-4" />
            Add provider
          </button>
        </div>
      </header>

      <main className="mx-auto min-h-[calc(100vh-5rem)] max-w-5xl px-6 py-10">
        <div className="mb-7 flex items-end justify-between gap-6">
          <div>
            <h2 className="text-2xl font-semibold tracking-tight">
              {definition.label} providers
            </h2>
            <p className="mt-1 text-sm text-muted-foreground">
              Credentials stay in CC Switch Lite until you choose to switch.
            </p>
          </div>
          <span className="rounded-full bg-muted px-3 py-1 text-xs font-medium text-muted-foreground">
            {providers.length}{" "}
            {providers.length === 1 ? "provider" : "providers"}
          </span>
        </div>

        {(adapterError || liveError || loadError || currentError) && (
          <div
            role="alert"
            className="mb-6 rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-600 dark:text-red-300"
          >
            {adapterError || liveError || loadError || currentError}
          </div>
        )}

        {notice && (
          <div
            role="status"
            className="mb-6 flex items-center gap-2 rounded-xl border border-emerald-500/30 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-700 dark:text-emerald-300"
          >
            <Check className="size-4" aria-hidden="true" />
            {notice}
          </div>
        )}

        {loading ? (
          <div
            className="grid min-h-72 place-items-center"
            aria-label="Loading providers"
          >
            <LoaderCircle className="size-6 animate-spin text-muted-foreground" />
          </div>
        ) : providers.length === 0 ? (
          <section
            className="glass-card mx-auto mt-12 w-full max-w-xl rounded-2xl px-8 py-12 text-center shadow-card"
            aria-labelledby="empty-state-title"
          >
            <div className="mx-auto mb-5 grid size-14 place-items-center rounded-2xl bg-muted">
              <ActiveIcon
                className={`size-7 ${definition.iconClassName}`}
                strokeWidth={2}
                aria-hidden="true"
              />
            </div>
            <h2
              id="empty-state-title"
              className="text-xl font-semibold tracking-tight"
            >
              {definition.emptyTitle}
            </h2>
            <p className="mx-auto mt-2 max-w-sm text-sm leading-6 text-muted-foreground">
              Add one manually, or import the API provider from your current
              live configuration.
            </p>
            <div className="mt-6 flex justify-center gap-3">
              <button
                type="button"
                disabled={!adapter || liveBusy !== null}
                onClick={beginImport}
                className="inline-flex h-10 items-center gap-2 rounded-xl border border-border px-4 text-sm font-medium transition-colors hover:bg-muted disabled:opacity-50"
              >
                {liveBusy === "import" ? (
                  <LoaderCircle className="size-4 animate-spin" />
                ) : (
                  <Download className="size-4" />
                )}
                {isClaude ? "Import user default" : "Import current"}
              </button>
              <button
                type="button"
                disabled={!adapter || liveBusy !== null}
                onClick={() => openEditor("new")}
                className="inline-flex h-10 items-center gap-2 rounded-xl bg-primary px-4 text-sm font-medium text-primary-foreground shadow-sm disabled:opacity-50"
              >
                <Plus className="size-4" />
                Add provider
              </button>
            </div>
          </section>
        ) : (
          <section className="grid grid-cols-2 gap-4" aria-label="Providers">
            {providers.map((provider) => {
              const providerAdapter = adapters.find((item) =>
                adapterMatchesProvider(item, provider),
              );
              const endpoint = providerAdapter
                ? visibleEndpoint(providerAdapter, provider)
                : "";
              const isCurrent = currentProviders.some(
                (current) =>
                  current.id === provider.id &&
                  current.revision === provider.revision,
              );
              return (
                <article
                  key={provider.id}
                  className={`glass-card rounded-2xl p-5 shadow-card ${
                    isCurrent ? "ring-1 ring-primary/50" : ""
                  }`}
                >
                  <div className="flex items-start justify-between gap-4">
                    <div className="flex min-w-0 items-start gap-3">
                      <div className="grid size-10 shrink-0 place-items-center rounded-xl bg-muted text-muted-foreground">
                        <KeyRound className="size-5" aria-hidden="true" />
                      </div>
                      <div className="min-w-0">
                        <div className="flex min-w-0 items-center gap-2">
                          <h3 className="truncate font-semibold">
                            {provider.name}
                          </h3>
                          {isCurrent && (
                            <span
                              className="inline-flex shrink-0 items-center gap-1 rounded-full bg-primary/10 px-2 py-0.5 text-[11px] font-medium text-primary"
                              title={
                                isClaude
                                  ? "Project, local, or managed settings can override this user default"
                                  : undefined
                              }
                            >
                              <Check className="size-3" aria-hidden="true" />
                              {currentLabel}
                            </span>
                          )}
                        </div>
                        <p className="mt-1 truncate text-xs text-muted-foreground">
                          {!providerAdapter
                            ? "Adapter unavailable"
                            : endpoint || "Default endpoint"}
                        </p>
                      </div>
                    </div>
                    <div className="flex shrink-0 gap-1">
                      <button
                        type="button"
                        disabled={!providerAdapter || liveBusy !== null}
                        onClick={() => openEditor(provider)}
                        className="inline-flex size-8 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-40"
                        aria-label={`Edit ${provider.name}`}
                      >
                        <Pencil className="size-4" />
                      </button>
                      <button
                        ref={(element) => {
                          if (element)
                            deleteButtonRefs.current.set(provider.id, element);
                          else deleteButtonRefs.current.delete(provider.id);
                        }}
                        type="button"
                        disabled={liveBusy !== null}
                        onClick={() => {
                          setMutationError(null);
                          setDeleting(provider);
                        }}
                        className="inline-flex size-8 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-red-500/10 hover:text-red-600 disabled:cursor-not-allowed disabled:opacity-40"
                        aria-label={`Delete ${provider.name}`}
                      >
                        <Trash2 className="size-4" />
                      </button>
                    </div>
                  </div>
                  <div className="mt-5 flex items-center justify-between gap-3 border-t border-border pt-4">
                    <span className="text-xs text-muted-foreground">
                      Stored locally
                    </span>
                    <button
                      type="button"
                      disabled={
                        !providerAdapter || isCurrent || liveBusy !== null
                      }
                      onClick={() => switchProvider(provider)}
                      className={`inline-flex h-8 min-w-24 items-center justify-center gap-1.5 rounded-lg px-3 text-xs font-medium transition-colors disabled:cursor-not-allowed ${
                        isCurrent
                          ? "bg-primary/10 text-primary"
                          : "bg-primary text-primary-foreground disabled:opacity-40"
                      }`}
                      aria-label={
                        isCurrent
                          ? isClaude
                            ? `${provider.name} is the Claude Code user default`
                            : `${provider.name} is current`
                          : `Switch to ${provider.name}`
                      }
                    >
                      {liveBusy === provider.id ? (
                        <>
                          <LoaderCircle className="size-3.5 animate-spin" />
                          Switching…
                        </>
                      ) : isCurrent ? (
                        <>
                          <Check className="size-3.5" />
                          {currentLabel}
                        </>
                      ) : (
                        "Switch"
                      )}
                    </button>
                  </div>
                </article>
              );
            })}
          </section>
        )}
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
