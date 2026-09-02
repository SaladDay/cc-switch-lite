import { useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { Monitor, MoreHorizontal, Terminal } from "lucide-react";

import { appDefinition } from "../lib/apps";
import type { AppId, CoreAppDescriptor } from "../lib/provider-types";
import { cn } from "../lib/utils";
import { ProviderIcon } from "./ProviderIcon";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";

const APP_BADGE_ICON: Record<
  string,
  { icon: typeof Terminal; offsetY?: number }
> = {
  claude: { icon: Terminal },
  "claude-desktop": { icon: Monitor, offsetY: 0.5 },
};

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
  const badgeConfig = APP_BADGE_ICON[app.id];
  const BadgeIcon = badgeConfig?.icon;
  return (
    <span className="relative inline-flex shrink-0">
      <ProviderIcon icon={definition.icon} name={definition.label} size={20} />
      {BadgeIcon && (
        <span
          className={cn(
            "absolute -bottom-0.5 -right-0.5 flex items-center justify-center rounded-[3px] border h-[11px] w-[11px]",
            isActive
              ? "bg-background border-border text-foreground"
              : "bg-muted border-background text-muted-foreground group-hover:bg-background group-hover:text-foreground",
          )}
          aria-hidden="true"
        >
          <BadgeIcon
            className="h-[8px] w-[8px]"
            strokeWidth={2.5}
            style={
              badgeConfig.offsetY
                ? { transform: `translateY(${badgeConfig.offsetY}px)` }
                : undefined
            }
          />
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
  const rootRef = useRef<HTMLDivElement>(null);
  const appButtonRefs = useRef(new Map<AppId, HTMLButtonElement>());
  const moreButtonRef = useRef<HTMLButtonElement>(null);
  const pendingFocusRef = useRef<AppId | "more" | null>(null);
  const [moreOpen, setMoreOpen] = useState(false);
  const appCount = apps.length;
  const [visibleCount, setVisibleCount] = useState(appCount);

  const visibleIdsForCount = (count: number): Set<AppId> => {
    const visible = apps.slice(0, Math.max(1, count));
    const active = apps.find((app) => app.id === activeApp);
    if (active && !visible.some((app) => app.id === activeApp)) {
      visible[visible.length - 1] = active;
    }
    return new Set(visible.map((app) => app.id));
  };

  const preserveFocus = (nextVisibleCount: number) => {
    const focused = document.activeElement;
    if (!(focused instanceof HTMLElement)) return;

    const focusedApp = focused.dataset.appId;
    const focusedInRoot = rootRef.current?.contains(focused) ?? false;
    const focusedInOverflow = Boolean(
      focused.closest('[data-app-switcher-overflow="true"]'),
    );
    if (!focusedInRoot && !focusedInOverflow) return;

    const nextVisibleIds = visibleIdsForCount(nextVisibleCount);
    if (focusedInOverflow) {
      if (focusedApp && nextVisibleIds.has(focusedApp)) {
        pendingFocusRef.current = focusedApp;
      }
    } else if (focusedApp && !nextVisibleIds.has(focusedApp)) {
      pendingFocusRef.current = "more";
    } else if (
      focused === moreButtonRef.current &&
      nextVisibleCount >= appCount
    ) {
      pendingFocusRef.current = activeApp;
    }
  };

  const handleSwitch = (app: AppId) => {
    if (app === activeApp) return;
    onSwitch(app);
  };

  useLayoutEffect(() => {
    const root = rootRef.current;
    const slot = root?.parentElement;
    if (!root || !slot) return;

    const compute = () => {
      const sample = root.querySelector("button");
      if (!sample) return;
      const itemWidth = sample.offsetWidth;
      if (itemWidth <= 0) {
        preserveFocus(appCount);
        setVisibleCount(appCount);
        return;
      }
      const rootStyle = window.getComputedStyle(root);
      const gap = parseFloat(rootStyle.columnGap) || 0;
      const padding =
        (parseFloat(rootStyle.paddingLeft) || 0) +
        (parseFloat(rootStyle.paddingRight) || 0);
      const available = slot.clientWidth;
      const widthAll = padding + appCount * itemWidth + (appCount - 1) * gap;
      if (widthAll <= available) {
        preserveFocus(appCount);
        setVisibleCount(appCount);
        return;
      }
      const fit = Math.floor(
        (available - padding - itemWidth) / (itemWidth + gap),
      );
      const nextVisibleCount = Math.max(1, Math.min(appCount - 1, fit));
      preserveFocus(nextVisibleCount);
      setVisibleCount(nextVisibleCount);
    };

    compute();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(compute);
    observer.observe(slot);
    return () => observer.disconnect();
  }, [activeApp, appCount]);

  useLayoutEffect(() => {
    const target = pendingFocusRef.current;
    if (!target) return;
    pendingFocusRef.current = null;
    if (target === "more") {
      moreButtonRef.current?.focus();
    } else {
      appButtonRefs.current.get(target)?.focus();
    }
  }, [visibleCount]);

  const visibleList = apps.slice(0, Math.max(1, visibleCount));
  const active = apps.find((app) => app.id === activeApp);
  if (active && !visibleList.some((app) => app.id === activeApp)) {
    visibleList[visibleList.length - 1] = active;
  }
  const visibleIds = new Set(visibleList.map((app) => app.id));
  const overflowList = apps.filter((app) => !visibleIds.has(app.id));

  useLayoutEffect(() => {
    if (overflowList.length === 0 && moreOpen) setMoreOpen(false);
  }, [moreOpen, overflowList.length]);

  return (
    <div
      ref={rootRef}
      role="navigation"
      aria-label="Applications"
      className="inline-flex bg-muted rounded-xl p-1 gap-1"
      style={{ WebkitAppRegion: "no-drag" } as CSSProperties}
    >
      {visibleList.map((app) => {
        const isActive = activeApp === app.id;
        const label = appDefinition(app.id, [app]).label;
        return (
          <button
            key={app.id}
            ref={(element) => {
              if (element) appButtonRefs.current.set(app.id, element);
              else appButtonRefs.current.delete(app.id);
            }}
            type="button"
            data-app-id={app.id}
            disabled={disabled}
            onClick={() => handleSwitch(app.id)}
            title={label}
            aria-label={label}
            aria-pressed={isActive}
            className={cn(
              "group inline-flex items-center px-3 h-8 rounded-md text-sm font-medium transition-all duration-200 disabled:pointer-events-none disabled:opacity-50",
              isActive
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground hover:bg-background/50",
            )}
          >
            <AppGlyph app={app} isActive={isActive} />
          </button>
        );
      })}
      {overflowList.length > 0 && (
        <Popover open={moreOpen} onOpenChange={setMoreOpen}>
          <PopoverTrigger asChild>
            <button
              ref={moreButtonRef}
              type="button"
              disabled={disabled}
              title="More applications"
              aria-label="More applications"
              className={cn(
                "inline-flex items-center px-3 h-8 rounded-md transition-all duration-200 disabled:pointer-events-none disabled:opacity-50",
                moreOpen
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground hover:bg-background/50",
              )}
            >
              <MoreHorizontal size={20} className="shrink-0" />
            </button>
          </PopoverTrigger>
          <PopoverContent
            side="bottom"
            align="end"
            sideOffset={6}
            className="z-[100] w-56 p-1"
            aria-label="More applications"
            data-app-switcher-overflow="true"
          >
            {overflowList.map((app) => {
              const label = appDefinition(app.id, [app]).label;
              return (
                <button
                  key={app.id}
                  type="button"
                  data-app-id={app.id}
                  disabled={disabled}
                  onClick={() => {
                    setMoreOpen(false);
                    handleSwitch(app.id);
                  }}
                  className="group flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
                >
                  <AppGlyph app={app} isActive={false} />
                  <span className="truncate">{label}</span>
                </button>
              );
            })}
          </PopoverContent>
        </Popover>
      )}
    </div>
  );
}
