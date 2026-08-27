import { useEffect, useRef, useState } from "react";
import {
  Asterisk,
  Bot,
  KeyRound,
  LoaderCircle,
  Moon,
  Pencil,
  Plus,
  Settings,
  Sun,
  Trash2,
  type LucideIcon,
} from "lucide-react";

import { DeleteProviderDialog } from "./components/DeleteProviderDialog";
import { ProviderDialog } from "./components/ProviderDialog";
import type {
  AdapterDescriptor,
  AppId,
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

function sameJsonValue(left: JsonValue, right: JsonValue): boolean {
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
        Object.hasOwn(right, key) && sameJsonValue(left[key], right[key]),
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
  const [editing, setEditing] = useState<ProviderRecord | "new" | null>(null);
  const [deleting, setDeleting] = useState<ProviderRecord | null>(null);
  const [mutationBusy, setMutationBusy] = useState(false);
  const [mutationError, setMutationError] = useState<string | null>(null);
  const addProviderButtonRef = useRef<HTMLButtonElement>(null);
  const deleteButtonRefs = useRef(new Map<string, HTMLButtonElement>());
  const definition = APPS.find((app) => app.id === activeApp) ?? APPS[0];
  const adapter = adapters.find((item) => item.appId === activeApp);
  const editingAdapter =
    editing === "new"
      ? adapter
      : editing
        ? adapters.find((item) => adapterMatchesProvider(item, editing))
        : undefined;
  const ActiveIcon = definition.icon;

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
    setLoading(true);
    setProviders([]);
    setLoadError(null);
    providersApi
      .list(activeApp)
      .then((items) => {
        if (!ignore) setProviders(items);
      })
      .catch((error: unknown) => {
        if (!ignore) {
          setProviders([]);
          setLoadError(errorMessage(error));
        }
      })
      .finally(() => {
        if (!ignore) setLoading(false);
      });
    return () => {
      ignore = true;
    };
  }, [activeApp]);

  const selectApp = (app: AppId) => {
    setActiveApp(app);
    setEditing(null);
    setDeleting(null);
    setMutationError(null);
    window.localStorage.setItem(APP_STORAGE_KEY, app);
  };

  const openEditor = (provider: ProviderRecord | "new") => {
    setMutationError(null);
    setEditing(provider);
  };

  const saveProvider = async (update: ProviderChanges) => {
    if (!editing) return;
    setMutationBusy(true);
    setMutationError(null);
    try {
      if (editing === "new") {
        if (!adapter) return;
        const created = await providersApi.create({
          appId: activeApp,
          adapter: adapter.reference,
          ...update,
        });
        setProviders((current) => [...current, created]);
      } else {
        const updated = await providersApi.update(editing.id, {
          ...update,
          expectedRevision: editing.revision,
        });
        setProviders((current) =>
          current.map((provider) =>
            provider.id === updated.id ? updated : provider,
          ),
        );
      }
      setEditing(null);
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
                aria-pressed={selected}
                onClick={() => selectApp(app.id)}
                className={`inline-flex h-9 cursor-pointer items-center gap-2 rounded-lg px-4 text-sm font-medium transition-colors duration-200 ${
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
            disabled
            className="inline-flex size-9 items-center justify-center rounded-lg text-muted-foreground opacity-50"
            aria-label="Settings are not available yet"
            title="Settings are added with the plugin marketplace"
          >
            <Settings className="size-4" />
          </button>
          <button
            ref={addProviderButtonRef}
            type="button"
            disabled={!adapter}
            onClick={() => openEditor("new")}
            className="ml-2 inline-flex h-10 items-center gap-2 rounded-xl bg-primary px-4 text-sm font-medium text-primary-foreground shadow-sm transition-opacity disabled:opacity-50"
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

        {(adapterError || loadError) && (
          <div
            role="alert"
            className="mb-6 rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-600 dark:text-red-300"
          >
            {adapterError || loadError}
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
              Save a provider here. Live configuration is not changed until a
              later switching step.
            </p>
            <button
              type="button"
              disabled={!adapter}
              onClick={() => openEditor("new")}
              className="mt-6 inline-flex h-10 items-center gap-2 rounded-xl bg-primary px-4 text-sm font-medium text-primary-foreground shadow-sm disabled:opacity-50"
            >
              <Plus className="size-4" />
              Add provider
            </button>
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
              return (
                <article
                  key={provider.id}
                  className="glass-card rounded-2xl p-5 shadow-card"
                >
                  <div className="flex items-start justify-between gap-4">
                    <div className="flex min-w-0 items-start gap-3">
                      <div className="grid size-10 shrink-0 place-items-center rounded-xl bg-muted text-muted-foreground">
                        <KeyRound className="size-5" aria-hidden="true" />
                      </div>
                      <div className="min-w-0">
                        <h3 className="truncate font-semibold">
                          {provider.name}
                        </h3>
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
                        disabled={
                          !adapters.some((item) =>
                            adapterMatchesProvider(item, provider),
                          )
                        }
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
                        onClick={() => {
                          setMutationError(null);
                          setDeleting(provider);
                        }}
                        className="inline-flex size-8 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-red-500/10 hover:text-red-600"
                        aria-label={`Delete ${provider.name}`}
                      >
                        <Trash2 className="size-4" />
                      </button>
                    </div>
                  </div>
                  <div className="mt-5 border-t border-border pt-4 text-xs text-muted-foreground">
                    Stored locally · Switching comes next
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
          adapter={editingAdapter}
          provider={editing === "new" ? undefined : editing}
          busy={mutationBusy}
          error={mutationError}
          onCancel={() => setEditing(null)}
          onSave={saveProvider}
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
