import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import {
  AnimatePresence,
  motion,
  useIsPresent,
  useReducedMotion,
} from "framer-motion";
import {
  ArrowLeft,
  Check,
  Download,
  LoaderCircle,
  Plus,
  Settings as SettingsIcon,
  Wrench,
} from "lucide-react";

import { DeleteProviderDialog } from "./components/DeleteProviderDialog";
import { McpIcon } from "./components/McpIcon";
import { McpPanel, type McpPanelHandle } from "./components/mcp/McpPanel";
import { ProviderDialog } from "./components/ProviderDialog";
import { AppSwitcher } from "./components/AppSwitcher";
import { SettingsPanel } from "./components/settings/SettingsPanel";
import { SkillsPanel } from "./components/skills/SkillsPanel";
import {
  ProviderList,
  type ProviderListItem,
} from "./components/providers/ProviderList";
import { Button } from "./components/ui/button";
import {
  appDefinition,
  parseCoreAppCatalog,
  supportsFeature,
} from "./lib/apps";
import type {
  AdapterDescriptor,
  AppId,
  CoreAppDescriptor,
  CurrentProvider,
  JsonValue,
  ProviderChanges,
  ProviderRecord,
  SimpleProviderFormDescriptor,
} from "./lib/provider-types";
import { isNativeAdapter, sameAdapterIdentity } from "./lib/provider-types";
import {
  APP_VISIBILITY_STORAGE_KEY,
  THEME_STORAGE_KEY,
  appIsVisible,
  initialAppVisibility,
  initialTheme,
  type Theme,
} from "./lib/preferences";
import { errorMessage, providersApi } from "./lib/providers";

const APP_STORAGE_KEY = "cc-switch-lite:last-app";
const VIEW_STORAGE_KEY = "cc-switch-lite:last-view";
const DRAG_BAR_HEIGHT = 28;
const HEADER_HEIGHT = 64;
type View = "providers" | "mcp" | "skills" | "settings";

function FadePanel({
  children,
  className,
  duration,
}: {
  children: ReactNode;
  className: string;
  duration: number;
}) {
  const elementRef = useRef<HTMLDivElement>(null);
  const isPresent = useIsPresent();
  const reduceMotion = useReducedMotion();

  useLayoutEffect(() => {
    const element = elementRef.current;
    if (!element) return;
    if (isPresent) {
      element.removeAttribute("inert");
      element.removeAttribute("aria-hidden");
    } else {
      element.setAttribute("inert", "");
      element.setAttribute("aria-hidden", "true");
    }
  }, [isPresent]);

  return (
    <motion.div
      ref={elementRef}
      className={className}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: reduceMotion ? 0 : duration }}
    >
      {children}
    </motion.div>
  );
}

function initialApp(): AppId {
  const stored = window.localStorage.getItem(APP_STORAGE_KEY);
  return stored?.trim() || "claude";
}

function initialView(): View {
  const stored = window.localStorage.getItem(VIEW_STORAGE_KEY);
  return stored === "mcp" || stored === "skills" ? stored : "providers";
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
  const [currentView, setCurrentView] = useState<View>(initialView);
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [appVisibility, setAppVisibility] = useState(initialAppVisibility);
  const [appCatalog, setAppCatalog] = useState<CoreAppDescriptor[]>([]);
  const [catalogReady, setCatalogReady] = useState(false);
  const [adapters, setAdapters] = useState<AdapterDescriptor[]>([]);
  const [simpleForms, setSimpleForms] = useState<
    SimpleProviderFormDescriptor[]
  >([]);
  const [providers, setProviders] = useState<ProviderRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [adapterError, setAdapterError] = useState<string | null>(null);
  const [simpleFormError, setSimpleFormError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [currentError, setCurrentError] = useState<string | null>(null);
  const [editing, setEditing] = useState<ProviderRecord | "new" | null>(null);
  const [deleting, setDeleting] = useState<ProviderRecord | null>(null);
  const [mutationBusy, setMutationBusy] = useState(false);
  const [mutationError, setMutationError] = useState<string | null>(null);
  const [liveBusy, setLiveBusy] = useState<string | null>(null);
  const [liveError, setLiveError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [mcpManagementBusy, setMcpManagementBusy] = useState(false);
  const [skillsManagementBusy, setSkillsManagementBusy] = useState(false);
  const [currentProviders, setCurrentProviders] = useState<CurrentProvider[]>(
    [],
  );
  const currentRequestGeneration = useRef(0);
  const activeAppRef = useRef(activeApp);
  activeAppRef.current = activeApp;
  const addProviderButtonRef = useRef<HTMLButtonElement>(null);
  const mcpPanelRef = useRef<McpPanelHandle>(null);
  const settingsButtonRef = useRef<HTMLButtonElement>(null);
  const restoreSettingsFocusRef = useRef(false);
  const deleteButtonRefs = useRef(new Map<string, HTMLButtonElement>());
  const definition = appDefinition(activeApp, appCatalog);
  const visibleApps = appCatalog.filter((app) =>
    appIsVisible(appVisibility, app.id),
  );
  const isClaude = activeApp === "claude";
  const additive =
    appCatalog.find((app) => app.id === activeApp)?.configurationMode ===
    "additive";
  const providerActionsReady =
    catalogReady && supportsFeature(appCatalog, activeApp, "providers");
  const liveActionsReady =
    catalogReady && supportsFeature(appCatalog, activeApp, "liveConfiguration");
  const providerListReady = providerActionsReady || liveActionsReady;
  const currentStateReady = liveActionsReady || providerActionsReady;
  const catalogReadyRef = useRef(catalogReady);
  const providerListReadyRef = useRef(providerListReady);
  catalogReadyRef.current = catalogReady;
  providerListReadyRef.current = providerListReady;
  const currentLabel = "In Use";
  const importLabel = isClaude
    ? "Import Claude Code user configuration"
    : `Import current ${definition.label} configuration`;
  const activeAdapters = adapters.filter((item) => item.appId === activeApp);
  const nativeLiveAdapter = activeAdapters.find((item) =>
    isNativeAdapter(item.reference),
  );
  const adapter = nativeLiveAdapter;
  const simpleForm = simpleForms.find((form) => form.appId === activeApp);
  const supportsMcp = supportsFeature(appCatalog, activeApp, "mcp");
  const supportsSkills = supportsFeature(appCatalog, activeApp, "skills");
  const mcpApps = appCatalog.filter((app) =>
    supportsFeature(appCatalog, app.id, "mcp"),
  );
  const editingAdapter =
    editing === "new"
      ? adapter
      : editing
        ? adapters.find((item) => adapterMatchesProvider(item, editing))
        : undefined;
  const editingForm =
    editing === "new"
      ? simpleForm
      : editing
        ? simpleForms.find((form) => form.appId === editing.appId)
        : undefined;
  const contentTopOffset = DRAG_BAR_HEIGHT + HEADER_HEIGHT;
  const addActionButtonClass =
    "bg-orange-500 hover:bg-orange-600 dark:bg-orange-500 dark:hover:bg-orange-600 text-white shadow-lg shadow-orange-500/30 dark:shadow-orange-500/40 rounded-full w-8 h-8";

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

  const selectApp = useCallback((app: AppId) => {
    setActiveApp(app);
    setEditing(null);
    setDeleting(null);
    setMutationError(null);
    setLiveError(null);
    setCurrentError(null);
    setNotice(null);
    window.localStorage.setItem(APP_STORAGE_KEY, app);
  }, []);

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
      APP_VISIBILITY_STORAGE_KEY,
      JSON.stringify(appVisibility),
    );
    if (appCatalog.length === 0) return;
    const nextVisible = appCatalog.filter((app) =>
      appIsVisible(appVisibility, app.id),
    );
    if (nextVisible.length === 0) {
      setAppVisibility((current) => ({
        ...current,
        [appCatalog[0].id]: true,
      }));
      return;
    }
    if (!appIsVisible(appVisibility, activeApp)) {
      selectApp(nextVisible[0].id);
    }
  }, [activeApp, appCatalog, appVisibility, selectApp]);

  useEffect(() => {
    window.localStorage.setItem(VIEW_STORAGE_KEY, currentView);
    if (currentView === "providers" && restoreSettingsFocusRef.current) {
      restoreSettingsFocusRef.current = false;
      settingsButtonRef.current?.focus();
    }
  }, [currentView]);

  useEffect(() => {
    if (
      !catalogReady ||
      currentView === "providers" ||
      currentView === "settings"
    )
      return;
    if (!supportsFeature(appCatalog, activeApp, currentView)) {
      setCurrentView("providers");
    }
  }, [activeApp, appCatalog, catalogReady, currentView]);

  useEffect(() => {
    const openSettings = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === ",") {
        event.preventDefault();
        if (
          !mutationBusy &&
          liveBusy === null &&
          !mcpManagementBusy &&
          !skillsManagementBusy &&
          editing === null &&
          deleting === null
        )
          setCurrentView("settings");
      }
    };
    window.addEventListener("keydown", openSettings);
    return () => window.removeEventListener("keydown", openSettings);
  }, [
    deleting,
    editing,
    liveBusy,
    mcpManagementBusy,
    mutationBusy,
    skillsManagementBusy,
  ]);

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
          setCurrentView("providers");
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
    providersApi
      .listSimpleForms()
      .then((items) => {
        if (!ignore) {
          setSimpleForms(items);
          setSimpleFormError(null);
        }
      })
      .catch((error: unknown) => {
        if (!ignore) setSimpleFormError(errorMessage(error));
      });
    return () => {
      ignore = true;
    };
  }, []);

  useEffect(() => {
    let ignore = false;
    const app = activeApp;
    setLoading(true);
    setProviders([]);
    setLoadError(null);
    providersApi
      .list(app)
      .then((items) => {
        if (ignore || activeAppRef.current !== app) return;
        if (catalogReadyRef.current && !providerListReadyRef.current) {
          setProviders([]);
        } else {
          setProviders(items);
        }
        setLoadError(null);
      })
      .catch((error: unknown) => {
        if (ignore || activeAppRef.current !== app) return;
        if (catalogReadyRef.current && !providerListReadyRef.current) {
          setLoadError(null);
        } else {
          setLoadError(errorMessage(error));
        }
        setProviders([]);
      })
      .finally(() => {
        if (ignore || activeAppRef.current !== app) return;
        setLoading(false);
      });
    return () => {
      ignore = true;
    };
  }, [activeApp]);

  useEffect(() => {
    if (!catalogReady || providerListReady) return;
    setProviders([]);
    setLoadError(null);
    setLoading(false);
  }, [catalogReady, providerListReady]);

  useEffect(() => {
    const generation = ++currentRequestGeneration.current;
    setCurrentProviders([]);
    setCurrentError(null);
    if (!catalogReady || !currentStateReady) return;

    let ignore = false;
    const app = activeApp;
    providersApi
      .currentProviders(app)
      .then((items) => {
        if (
          !ignore &&
          generation === currentRequestGeneration.current &&
          activeAppRef.current === app
        ) {
          setCurrentProviders(items);
          setCurrentError(null);
        }
      })
      .catch((error: unknown) => {
        if (
          !ignore &&
          generation === currentRequestGeneration.current &&
          activeAppRef.current === app
        ) {
          setCurrentError(errorMessage(error));
        }
      });
    return () => {
      ignore = true;
    };
  }, [activeApp, catalogReady, currentStateReady]);

  useEffect(() => {
    const refreshWhenVisible = () => {
      if (currentStateReady && document.visibilityState === "visible") {
        void refreshCurrent(activeApp);
      }
    };
    window.addEventListener("focus", refreshWhenVisible);
    document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      window.removeEventListener("focus", refreshWhenVisible);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
    };
  }, [activeApp, currentStateReady, refreshCurrent]);

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
        const created = await providersApi.createSimple({
          appId: activeApp,
          name: update.name,
          values: update.values,
          ...(update.presetId ? { presetId: update.presetId } : {}),
        });
        setProviders((current) => [...current, created]);
      } else {
        const updated = await providersApi.updateSimple(activeApp, editing.id, {
          expectedRevision: editing.revision,
          name: update.name,
          values: update.values,
        });
        setProviders((current) =>
          current.map((provider) =>
            provider.id === updated.id ? updated : provider,
          ),
        );
      }
      setEditing(null);
      setNotice(editing === "new" ? "Provider added." : "Provider updated.");
      if (currentStateReady) await refreshCurrent(activeApp);
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
      if (currentStateReady) await refreshCurrent(activeApp);
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

  const importNativeProviders = async () => {
    if (!liveActionsReady) return;
    setLiveBusy("import");
    setLiveError(null);
    setNotice(null);
    try {
      await providersApi.importNative(activeApp);
      if (providerListReady) {
        setProviders(await providersApi.list(activeApp));
      }
      await refreshCurrent(activeApp);
      setNotice(
        isClaude
          ? "Imported the Claude Code user configuration."
          : `Imported the current ${definition.label} configuration.`,
      );
    } catch (error) {
      setLiveError(errorMessage(error));
    } finally {
      setLiveBusy(null);
    }
  };

  const beginImport = () => {
    if (!liveActionsReady) return;
    setLiveError(null);
    if (!nativeLiveAdapter) return;
    void importNativeProviders();
  };

  const switchProvider = async (provider: ProviderRecord) => {
    if (!liveActionsReady) return;
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
    if (!liveActionsReady) return;
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
    const readOnly = provider.liteConfigWritable === false;
    const isCurrent = currentProviders.some(
      (current) =>
        current.id === provider.id && current.revision === provider.revision,
    );
    return {
      provider,
      adapterAvailable: providerAdapter !== undefined,
      canEdit:
        providerActionsReady &&
        providerAdapter !== undefined &&
        !readOnly &&
        provider.liteSimpleEditable === true,
      canSwitch:
        liveActionsReady &&
        providerAdapter !== undefined &&
        !readOnly &&
        currentError === null,
      canRemove:
        liveActionsReady &&
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
      readOnlyLabel: "Not supported in Lite",
      endpoint: providerAdapter ? visibleEndpoint(provider) : "",
      isCurrent,
    };
  });
  const viewTitle =
    currentView === "mcp"
      ? "MCP Server Management"
      : currentView === "skills"
        ? "Skills Management"
        : "Settings";

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
            {currentView === "providers" ? (
              <Button
                ref={settingsButtonRef}
                variant="ghost"
                size="icon"
                disabled={mutationBusy || liveBusy !== null}
                onClick={() => setCurrentView("settings")}
                title="Settings"
                aria-label="Open settings"
                className="hover:bg-black/5 dark:hover:bg-white/5"
              >
                <SettingsIcon className="h-4 w-4" />
              </Button>
            ) : (
              <div className="flex items-center gap-2">
                <Button
                  variant="outline"
                  size="icon"
                  disabled={
                    (currentView === "mcp" && mcpManagementBusy) ||
                    (currentView === "skills" && skillsManagementBusy)
                  }
                  onClick={() => {
                    if (currentView === "settings") {
                      restoreSettingsFocusRef.current = true;
                    }
                    setCurrentView("providers");
                  }}
                  className="mr-2 rounded-lg"
                  aria-label="Back to providers"
                >
                  <ArrowLeft className="h-4 w-4" />
                </Button>
                <h1 className="text-lg font-semibold">{viewTitle}</h1>
              </div>
            )}
          </div>

          <div className="flex min-w-0 flex-1 items-center justify-end gap-1.5">
            <div className="flex min-w-0 flex-1 items-center justify-end overflow-hidden py-4">
              {currentView === "providers" && (
                <AppSwitcher
                  activeApp={activeApp}
                  apps={visibleApps.length > 0 ? visibleApps : appCatalog}
                  disabled={mutationBusy || liveBusy !== null}
                  onSwitch={selectApp}
                />
              )}
            </div>
            <div className="flex shrink-0 items-center py-4">
              <div
                className="flex shrink-0 items-center gap-1.5"
                style={{ WebkitAppRegion: "no-drag" } as CSSProperties}
              >
                {currentView === "providers" && (
                  <>
                    {(supportsSkills || supportsMcp) && (
                      <div className="flex items-center gap-1 rounded-xl bg-muted p-1">
                        {supportsSkills && (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setCurrentView("skills")}
                            className="w-8 px-2 text-muted-foreground hover:bg-black/5 hover:text-foreground dark:hover:bg-white/5"
                            title="Manage Skills"
                            aria-label="Manage Skills"
                          >
                            <Wrench className="h-4 w-4" />
                          </Button>
                        )}
                        {supportsMcp && (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setCurrentView("mcp")}
                            className="w-8 px-2 text-muted-foreground hover:bg-black/5 hover:text-foreground dark:hover:bg-white/5"
                            title="Manage MCP servers"
                            aria-label="Manage MCP servers"
                          >
                            <McpIcon size={16} />
                          </Button>
                        )}
                      </div>
                    )}
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={
                        !liveActionsReady ||
                        !nativeLiveAdapter ||
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
                        !simpleForm ||
                        loading ||
                        mutationBusy ||
                        liveBusy !== null
                      }
                      onClick={() => openEditor("new")}
                      className={`ml-2 ${addActionButtonClass}`}
                      aria-label={`Add ${definition.label} provider`}
                      title="Add new provider"
                    >
                      <Plus className="h-5 w-5" />
                    </Button>
                  </>
                )}
                {currentView === "mcp" && (
                  <>
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={mcpManagementBusy}
                      onClick={() => mcpPanelRef.current?.importExisting()}
                      className="hover:bg-black/5 dark:hover:bg-white/5"
                    >
                      <Download className="mr-2 h-4 w-4" />
                      Import existing
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={mcpManagementBusy}
                      onClick={() => mcpPanelRef.current?.openAdd()}
                      className="hover:bg-black/5 dark:hover:bg-white/5"
                    >
                      <Plus className="mr-2 h-4 w-4" />
                      Add MCP
                    </Button>
                  </>
                )}
              </div>
            </div>
          </div>
        </div>
      </header>

      <main className="flex min-h-0 flex-1 flex-col overflow-y-auto">
        <AnimatePresence mode="wait">
          <FadePanel
            key={currentView}
            className="flex min-h-0 flex-1 flex-col"
            duration={0.2}
          >
            {currentView === "providers" ? (
              <div className="flex min-h-0 flex-1 flex-col overflow-hidden px-6">
                <div className="flex-1 overflow-y-auto overflow-x-hidden px-1 pb-12">
                  <AnimatePresence mode="wait">
                    <FadePanel
                      key={activeApp}
                      className="space-y-4"
                      duration={0.15}
                    >
                      {(catalogError ||
                        adapterError ||
                        simpleFormError ||
                        liveError ||
                        loadError ||
                        currentError) && (
                        <div
                          role="alert"
                          className="rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-600 dark:text-red-300"
                        >
                          {catalogError ||
                            adapterError ||
                            simpleFormError ||
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
                          !simpleForm ||
                          mutationBusy ||
                          liveBusy !== null
                        }
                        importDisabled={
                          !liveActionsReady ||
                          !nativeLiveAdapter ||
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
                    </FadePanel>
                  </AnimatePresence>
                </div>
              </div>
            ) : currentView === "mcp" ? (
              <McpPanel
                ref={mcpPanelRef}
                apps={mcpApps}
                onInteractionBlockedChange={setMcpManagementBusy}
              />
            ) : currentView === "skills" ? (
              <SkillsPanel
                apps={appCatalog}
                onInteractionBlockedChange={setSkillsManagementBusy}
              />
            ) : (
              <SettingsPanel
                apps={appCatalog}
                theme={theme}
                appVisibility={appVisibility}
                onThemeChange={setTheme}
                onAppVisibilityChange={setAppVisibility}
              />
            )}
          </FadePanel>
        </AnimatePresence>
      </main>

      {editing && editingAdapter && editingForm && (
        <ProviderDialog
          key={editing === "new" ? `${activeApp}-new` : editing.id}
          form={editingForm}
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
