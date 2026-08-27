import { useEffect, useState } from "react";
import {
  Asterisk,
  Bot,
  Moon,
  Plus,
  Settings,
  Sun,
  type LucideIcon,
} from "lucide-react";

type AppId = "claude" | "codex";

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

export default function App() {
  const [activeApp, setActiveApp] = useState<AppId>(initialApp);
  const [isDark, setIsDark] = useState(initialDarkMode);
  const definition = APPS.find((app) => app.id === activeApp) ?? APPS[0];
  const ActiveIcon = definition.icon;

  useEffect(() => {
    document.documentElement.classList.toggle("dark", isDark);
    window.localStorage.setItem(THEME_STORAGE_KEY, isDark ? "dark" : "light");
  }, [isDark]);

  const selectApp = (app: AppId) => {
    setActiveApp(app);
    window.localStorage.setItem(APP_STORAGE_KEY, app);
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
          role="tablist"
        >
          {APPS.map((app) => {
            const Icon = app.icon;
            const selected = app.id === activeApp;
            return (
              <button
                key={app.id}
                type="button"
                role="tab"
                aria-selected={selected}
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
            title="Settings are not part of the shell milestone"
          >
            <Settings className="size-4" />
          </button>
          <button
            type="button"
            disabled
            className="ml-2 inline-flex h-10 items-center gap-2 rounded-xl bg-primary px-4 text-sm font-medium text-primary-foreground opacity-60 shadow-sm"
            aria-label={`Add ${definition.label} provider`}
            title="Provider storage is added in the next milestone"
          >
            <Plus className="size-4" />
            Add provider
          </button>
        </div>
      </header>

      <main className="mx-auto flex min-h-[calc(100vh-5rem)] max-w-5xl items-center justify-center px-6 py-12">
        <section
          className="glass-card w-full max-w-xl rounded-2xl px-8 py-14 text-center shadow-card"
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
            The Lite shell is ready. Provider storage and switching will be
            added as isolated, reviewable milestones.
          </p>
        </section>
      </main>
    </div>
  );
}
