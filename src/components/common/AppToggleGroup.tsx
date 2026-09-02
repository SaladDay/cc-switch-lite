import { AlertTriangle, LoaderCircle } from "lucide-react";

import { appDefinition } from "../../lib/apps";
import type { AppId, CoreAppDescriptor } from "../../lib/provider-types";
import { cn } from "../../lib/utils";
import { ProviderIcon } from "../ProviderIcon";
import { appActiveClass } from "./app-management-style";

export interface AppToggleState {
  enabled: boolean;
  disabled?: boolean;
  pending?: boolean;
  warning?: boolean;
  title?: string;
}

interface AppToggleGroupProps {
  apps: CoreAppDescriptor[];
  stateFor: (appId: AppId) => AppToggleState;
  onToggle: (appId: AppId, enabled: boolean) => void;
  ariaLabel: (
    app: CoreAppDescriptor,
    state: AppToggleState,
    label: string,
  ) => string;
  disabled?: boolean;
}

export function AppToggleGroup({
  apps,
  stateFor,
  onToggle,
  ariaLabel,
  disabled = false,
}: AppToggleGroupProps) {
  return (
    <div className="min-w-0 flex-shrink overflow-x-auto">
      <div className="flex w-max items-center gap-1.5">
        {apps.map((app) => {
          const definition = appDefinition(app.id, [app]);
          const state = stateFor(app.id);
          return (
            <button
              key={app.id}
              type="button"
              onClick={() => onToggle(app.id, !state.enabled)}
              disabled={disabled || state.disabled || state.pending}
              aria-label={ariaLabel(app, state, definition.label)}
              aria-pressed={state.enabled}
              aria-busy={state.pending || undefined}
              title={state.title || definition.label}
              className={cn(
                "relative flex h-7 w-7 items-center justify-center rounded-lg transition-all disabled:cursor-not-allowed",
                state.warning
                  ? "bg-amber-500/15 opacity-100"
                  : state.enabled
                    ? appActiveClass(app.id)
                    : "opacity-35 hover:opacity-70",
              )}
            >
              {state.pending ? (
                <LoaderCircle className="h-4 w-4 animate-spin" />
              ) : (
                <ProviderIcon
                  icon={definition.icon}
                  name={definition.label}
                  size={17}
                />
              )}
              {state.warning && !state.pending && (
                <AlertTriangle
                  className="absolute -right-1 -top-1 h-3 w-3 text-amber-500"
                  aria-hidden="true"
                />
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
