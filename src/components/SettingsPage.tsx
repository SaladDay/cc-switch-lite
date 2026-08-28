import { useState } from "react";
import { Database, Monitor, Moon, Package, Store, Sun } from "lucide-react";

import packageInfo from "../../package.json";
import { APPS } from "../lib/apps";
import type { Theme, VisibleApps } from "../lib/preferences";
import type { AppId } from "../lib/provider-types";
import { cn } from "../lib/utils";
import { FullScreenPanel } from "./FullScreenPanel";
import { ProviderIcon } from "./ProviderIcon";
import { Button } from "./ui/button";

interface SettingsPageProps {
  theme: Theme;
  visibleApps: VisibleApps;
  supportedApps: AppId[];
  onThemeChange: (theme: Theme) => void;
  onVisibleAppsChange: (visibleApps: VisibleApps) => void;
  onOpenMarketplace: () => void;
  onClose: () => void;
}

export function SettingsPage({
  theme,
  visibleApps,
  supportedApps,
  onThemeChange,
  onVisibleAppsChange,
  onOpenMarketplace,
  onClose,
}: SettingsPageProps) {
  const [tab, setTab] = useState<"general" | "about">("general");
  const supported = new Set(supportedApps);
  const availableApps = APPS.filter((app) => supported.has(app.id));
  const visibleCount = availableApps.filter(
    (app) => visibleApps[app.id],
  ).length;

  const toggleApp = (appId: AppId) => {
    if (visibleApps[appId] && visibleCount <= 1) return;
    onVisibleAppsChange({
      ...visibleApps,
      [appId]: !visibleApps[appId],
    });
  };

  return (
    <FullScreenPanel
      title="Settings"
      titleId="settings-page-title"
      description="CC Switch Lite preferences"
      closeLabel="Close settings"
      busy={false}
      onClose={onClose}
      contentClassName="h-full"
    >
      <div className="mx-auto flex h-full w-full max-w-4xl flex-col">
        <div className="mb-6 grid w-full grid-cols-2 rounded-lg border border-border bg-muted/50 p-1">
          {(["general", "about"] as const).map((item) => (
            <button
              key={item}
              type="button"
              onClick={() => setTab(item)}
              aria-pressed={tab === item}
              className={cn(
                "h-9 rounded-md text-sm font-medium capitalize transition-all",
                tab === item
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {item}
            </button>
          ))}
        </div>

        {tab === "general" ? (
          <div className="space-y-8 pb-8">
            <SettingsSection
              title="Appearance"
              description="Use a light, dark, or system-matched theme."
            >
              <div className="inline-flex gap-1 rounded-md border border-border bg-background p-1">
                <ThemeButton
                  active={theme === "light"}
                  icon={Sun}
                  onClick={() => onThemeChange("light")}
                >
                  Light
                </ThemeButton>
                <ThemeButton
                  active={theme === "dark"}
                  icon={Moon}
                  onClick={() => onThemeChange("dark")}
                >
                  Dark
                </ThemeButton>
                <ThemeButton
                  active={theme === "system"}
                  icon={Monitor}
                  onClick={() => onThemeChange("system")}
                >
                  System
                </ThemeButton>
              </div>
            </SettingsSection>

            <SettingsSection
              title="Visible applications"
              description="Choose which applications appear in the provider switcher. At least one stays visible."
            >
              <div className="flex flex-wrap gap-1 rounded-md border border-border bg-background p-1">
                {availableApps.map((app) => {
                  const active = visibleApps[app.id];
                  return (
                    <Button
                      key={app.id}
                      type="button"
                      size="sm"
                      variant={active ? "default" : "ghost"}
                      disabled={active && visibleCount <= 1}
                      aria-pressed={active}
                      onClick={() => toggleApp(app.id)}
                      className={cn(
                        "min-w-[104px] gap-1.5 px-3",
                        active
                          ? "shadow-sm"
                          : "text-muted-foreground hover:bg-muted hover:text-foreground",
                      )}
                    >
                      <ProviderIcon
                        icon={app.icon}
                        name={app.label}
                        size={14}
                      />
                      {app.label}
                    </Button>
                  );
                })}
              </div>
            </SettingsSection>

            <SettingsSection
              title="Plugin marketplace"
              description="Install signed provider adapters and manage trusted registry sources."
            >
              <Button
                type="button"
                variant="outline"
                onClick={onOpenMarketplace}
              >
                <Store className="h-4 w-4" />
                Open marketplace
              </Button>
            </SettingsSection>

            <SettingsSection
              title="Provider storage"
              description="CC Switch and CC Switch Lite use the same provider catalog."
            >
              <div className="flex items-center gap-3 rounded-lg border border-border bg-muted/40 px-4 py-3">
                <Database className="h-4 w-4 shrink-0 text-muted-foreground" />
                <code className="truncate text-xs">
                  ~/.cc-switch/cc-switch.db
                </code>
              </div>
            </SettingsSection>
          </div>
        ) : (
          <div className="rounded-xl border border-border bg-card p-6">
            <div className="flex items-start gap-4">
              <div className="flex h-11 w-11 items-center justify-center rounded-xl bg-muted">
                <Package className="h-5 w-5" />
              </div>
              <div className="min-w-0 space-y-1">
                <h3 className="text-base font-semibold">CC Switch Lite</h3>
                <p className="text-sm text-muted-foreground">
                  A focused provider configuration editor built on
                  cc-switch-core.
                </p>
                <p className="pt-2 text-xs text-muted-foreground">
                  Version {packageInfo.version}
                </p>
              </div>
            </div>
          </div>
        )}
      </div>
    </FullScreenPanel>
  );
}

function SettingsSection({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-3">
      <header className="space-y-1">
        <h3 className="text-sm font-medium">{title}</h3>
        <p className="text-xs text-muted-foreground">{description}</p>
      </header>
      {children}
    </section>
  );
}

function ThemeButton({
  active,
  icon: Icon,
  onClick,
  children,
}: {
  active: boolean;
  icon: React.ComponentType<{ className?: string }>;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <Button
      type="button"
      size="sm"
      variant={active ? "default" : "ghost"}
      aria-pressed={active}
      onClick={onClick}
      className={cn(
        "min-w-[96px] gap-1.5",
        !active && "text-muted-foreground hover:bg-muted hover:text-foreground",
      )}
    >
      <Icon className="h-3.5 w-3.5" />
      {children}
    </Button>
  );
}
