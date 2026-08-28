import type { CSSProperties } from "react";
import { Terminal } from "lucide-react";

import type { AppId } from "../lib/provider-types";
import { cn } from "../lib/utils";
import { ProviderIcon } from "./ProviderIcon";

interface AppSwitcherProps {
  activeApp: AppId;
  disabled?: boolean;
  onSwitch: (app: AppId) => void;
}

const ALL_APPS: AppId[] = ["claude", "codex"];

const APP_ICON_NAME: Record<AppId, string> = {
  claude: "claude",
  codex: "openai",
};

const APP_DISPLAY_NAME: Record<AppId, string> = {
  claude: "Claude Code",
  codex: "Codex",
};

function AppGlyph({ app, isActive }: { app: AppId; isActive: boolean }) {
  return (
    <span className="relative inline-flex shrink-0">
      <ProviderIcon
        icon={APP_ICON_NAME[app]}
        name={APP_DISPLAY_NAME[app]}
        size={20}
      />
      {app === "claude" && (
        <span
          className={cn(
            "absolute -bottom-0.5 -right-0.5 flex h-[11px] w-[11px] items-center justify-center rounded-[3px] border",
            isActive
              ? "border-border bg-background text-foreground"
              : "border-background bg-muted text-muted-foreground group-hover:bg-background group-hover:text-foreground",
          )}
          aria-hidden="true"
        >
          <Terminal className="h-[8px] w-[8px]" strokeWidth={2.5} />
        </span>
      )}
    </span>
  );
}

export function AppSwitcher({
  activeApp,
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
      {ALL_APPS.map((app) => {
        const isActive = activeApp === app;
        return (
          <button
            key={app}
            type="button"
            disabled={disabled}
            onClick={() => {
              if (!isActive) onSwitch(app);
            }}
            title={APP_DISPLAY_NAME[app]}
            aria-label={APP_DISPLAY_NAME[app]}
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
