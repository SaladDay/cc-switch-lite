import type { CSSProperties } from "react";
import { Monitor, Terminal } from "lucide-react";

import { appDefinition } from "../lib/apps";
import type { AppId, CoreAppDescriptor } from "../lib/provider-types";
import { cn } from "../lib/utils";
import { ProviderIcon } from "./ProviderIcon";

interface AppSwitcherProps {
  activeApp: AppId;
  apps: CoreAppDescriptor[];
  disabled?: boolean;
  onSwitch: (app: AppId) => void;
}

function AppGlyph({
  app,
  isActive,
}: {
  app: CoreAppDescriptor;
  isActive: boolean;
}) {
  const definition = appDefinition(app.id, [app]);
  const BadgeIcon =
    app.id === "claude"
      ? Terminal
      : app.id === "claude-desktop"
        ? Monitor
        : null;
  return (
    <span className="relative inline-flex shrink-0">
      <ProviderIcon icon={definition.icon} name={definition.label} size={20} />
      {BadgeIcon && (
        <span
          className={cn(
            "absolute -bottom-0.5 -right-0.5 flex h-[11px] w-[11px] items-center justify-center rounded-[3px] border",
            isActive
              ? "border-border bg-background text-foreground"
              : "border-background bg-muted text-muted-foreground group-hover:bg-background group-hover:text-foreground",
          )}
          aria-hidden="true"
        >
          <BadgeIcon className="h-[8px] w-[8px]" strokeWidth={2.5} />
        </span>
      )}
    </span>
  );
}

export function AppSwitcher({
  activeApp,
  apps,
  disabled = false,
  onSwitch,
}: AppSwitcherProps) {
  return (
    <div
      role="navigation"
      aria-label="Applications"
      className="inline-flex gap-1 rounded-xl bg-muted p-1"
      style={{ WebkitAppRegion: "no-drag" } as CSSProperties}
    >
      {apps.map((app) => {
        const isActive = activeApp === app.id;
        const label = appDefinition(app.id, [app]).label;
        return (
          <button
            key={app.id}
            type="button"
            disabled={disabled}
            onClick={() => {
              if (!isActive) onSwitch(app.id);
            }}
            title={label}
            aria-label={label}
            aria-pressed={isActive}
            className={cn(
              "group inline-flex h-8 items-center rounded-md px-3 text-sm font-medium transition-all duration-200 disabled:pointer-events-none disabled:opacity-50",
              isActive
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:bg-background/50 hover:text-foreground",
            )}
          >
            <AppGlyph app={app} isActive={isActive} />
          </button>
        );
      })}
    </div>
  );
}
