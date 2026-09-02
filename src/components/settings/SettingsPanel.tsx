import { useState } from "react";
import { Monitor, Moon, Package, Sun } from "lucide-react";

import packageInfo from "../../../package.json";
import { appDefinition } from "../../lib/apps";
import {
  appIsVisible,
  type AppVisibility,
  type Theme,
} from "../../lib/preferences";
import type { CoreAppDescriptor } from "../../lib/provider-types";
import { cn } from "../../lib/utils";
import { ProviderIcon } from "../ProviderIcon";
import { Button } from "../ui/button";

interface SettingsPanelProps {
  apps: CoreAppDescriptor[];
  theme: Theme;
  appVisibility: AppVisibility;
  onThemeChange: (theme: Theme) => void;
  onAppVisibilityChange: (visibility: AppVisibility) => void;
}

type SettingsTab = "general" | "about";
const SETTINGS_TABS: SettingsTab[] = ["general", "about"];

export function SettingsPanel({
  apps,
  theme,
  appVisibility,
  onThemeChange,
  onAppVisibilityChange,
}: SettingsPanelProps) {
  const [tab, setTab] = useState<SettingsTab>("general");
  const visibleCount = apps.filter((app) =>
    appIsVisible(appVisibility, app.id),
  ).length;

  const toggleApp = (appId: string) => {
    const visible = appIsVisible(appVisibility, appId);
    if (visible && visibleCount <= 1) return;
    onAppVisibilityChange({ ...appVisibility, [appId]: !visible });
  };

  const moveTab = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    current: SettingsTab,
  ) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const index = SETTINGS_TABS.indexOf(current);
    const next =
      event.key === "Home"
        ? SETTINGS_TABS[0]
        : event.key === "End"
          ? SETTINGS_TABS.at(-1)!
          : SETTINGS_TABS[
              (index +
                (event.key === "ArrowRight" ? 1 : -1) +
                SETTINGS_TABS.length) %
                SETTINGS_TABS.length
            ];
    setTab(next);
    document.getElementById(`settings-${next}-tab`)?.focus();
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-12">
      <div className="mx-auto w-full max-w-4xl">
        <div
          role="tablist"
          aria-label="Settings sections"
          className="mb-6 grid w-full grid-cols-2 rounded-lg border border-white/10 p-1 glass"
        >
          {SETTINGS_TABS.map((item) => (
            <button
              key={item}
              id={`settings-${item}-tab`}
              type="button"
              role="tab"
              aria-selected={tab === item}
              aria-controls={`settings-${item}-panel`}
              tabIndex={tab === item ? 0 : -1}
              autoFocus={item === "general"}
              onClick={() => setTab(item)}
              onKeyDown={(event) => moveTab(event, item)}
              className={cn(
                "h-9 rounded-md text-sm font-medium capitalize transition-all",
                tab === item
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {item === "general" ? "General" : "About"}
            </button>
          ))}
        </div>

        {tab === "general" ? (
          <div
            id="settings-general-panel"
            role="tabpanel"
            aria-labelledby="settings-general-tab"
            className="space-y-8"
          >
            <SettingsSection
              title="Appearance"
              description="Use a light, dark, or system-matched theme."
            >
              <div className="inline-flex gap-1 rounded-md border border-border-default bg-background p-1">
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
              <div className="flex flex-wrap gap-1 rounded-md border border-border-default bg-background p-1">
                {apps.map((app) => {
                  const definition = appDefinition(app.id, [app]);
                  const visible = appIsVisible(appVisibility, app.id);
                  return (
                    <Button
                      key={app.id}
                      type="button"
                      size="sm"
                      variant={visible ? "default" : "ghost"}
                      disabled={visible && visibleCount <= 1}
                      aria-pressed={visible}
                      aria-label={`${visible ? "Hide" : "Show"} ${definition.label} in application switcher`}
                      onClick={() => toggleApp(app.id)}
                      className={cn(
                        "min-w-[104px] gap-1.5 px-3",
                        !visible &&
                          "text-muted-foreground hover:bg-muted hover:text-foreground",
                      )}
                    >
                      <ProviderIcon
                        icon={definition.icon}
                        name={definition.label}
                        size={14}
                      />
                      {definition.label}
                    </Button>
                  );
                })}
              </div>
            </SettingsSection>
          </div>
        ) : (
          <div
            id="settings-about-panel"
            role="tabpanel"
            aria-labelledby="settings-about-tab"
            className="rounded-xl border border-border-default bg-card p-6"
          >
            <div className="flex items-start gap-4">
              <div className="flex h-11 w-11 items-center justify-center rounded-xl bg-muted">
                <Package className="h-5 w-5" aria-hidden="true" />
              </div>
              <div className="min-w-0 space-y-1">
                <h2 className="text-base font-semibold">CC Switch Lite</h2>
                <p className="text-sm text-muted-foreground">
                  A focused configuration switcher built on cc-switch-core.
                </p>
                <p className="pt-2 text-xs text-muted-foreground">
                  Version {packageInfo.version}
                </p>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
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
      <div className="space-y-1">
        <h2 className="text-sm font-medium">{title}</h2>
        <p className="text-xs text-muted-foreground">{description}</p>
      </div>
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
