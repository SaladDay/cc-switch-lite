import type { Ref } from "react";

import type { AppId, ProviderRecord } from "../../lib/provider-types";
import { cn } from "../../lib/utils";
import { ProviderIcon } from "../ProviderIcon";
import { ProviderActions } from "./ProviderActions";

export interface ProviderListItem {
  provider: ProviderRecord;
  endpoint: string;
  adapterAvailable: boolean;
  isCurrent: boolean;
}

interface ProviderCardProps extends ProviderListItem {
  appId: AppId;
  currentLabel: string;
  busy: boolean;
  switching: boolean;
  deleteButtonRef?: Ref<HTMLButtonElement>;
  onSwitch: (provider: ProviderRecord) => void;
  onEdit: (provider: ProviderRecord) => void;
  onDelete: (provider: ProviderRecord) => void;
}

export function ProviderCard({
  provider,
  endpoint,
  adapterAvailable,
  isCurrent,
  appId,
  currentLabel,
  busy,
  switching,
  deleteButtonRef,
  onSwitch,
  onEdit,
  onDelete,
}: ProviderCardProps) {
  const displayEndpoint = adapterAvailable
    ? endpoint || "Default endpoint"
    : "Adapter unavailable";
  const currentAriaLabel =
    appId === "claude"
      ? `${provider.name} is the Claude Code user default`
      : `${provider.name} is current`;

  return (
    <div
      className={cn(
        "relative overflow-hidden rounded-xl border border-border p-4 transition-all duration-300",
        "bg-card text-card-foreground group",
        "hover:border-border-active",
        isCurrent && "border-blue-500/60 shadow-sm shadow-blue-500/10",
        !isCurrent && "hover:shadow-sm",
      )}
    >
      <div
        className={cn(
          "pointer-events-none absolute inset-0 bg-gradient-to-r from-blue-500/10 to-transparent transition-opacity duration-500",
          isCurrent ? "opacity-100" : "opacity-0",
        )}
      />
      <div className="relative flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <div className="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-lg border border-border bg-muted transition-transform duration-300 group-hover:scale-105">
            <ProviderIcon
              icon={appId === "claude" ? "claude" : "openai"}
              name={provider.name}
              size={20}
            />
          </div>

          <div className="min-w-0 flex-1 space-y-1">
            <div className="flex min-h-7 flex-wrap items-center gap-2">
              <h3
                className="min-w-0 max-w-full truncate text-base font-semibold leading-none"
                title={provider.name}
              >
                {provider.name}
              </h3>
            </div>
            <p
              className="inline-flex max-w-full items-center overflow-hidden text-left text-sm text-muted-foreground"
              title={displayEndpoint}
            >
              <span className="min-w-0 truncate">{displayEndpoint}</span>
            </p>
          </div>
        </div>

        <div className="ml-auto flex min-w-0 items-center gap-3">
          <span className="text-xs text-muted-foreground">Stored locally</span>
          <div className="flex flex-shrink-0 items-center gap-1.5 opacity-0 pointer-events-none transition-opacity duration-200 group-hover:opacity-100 group-hover:pointer-events-auto group-focus-within:opacity-100 group-focus-within:pointer-events-auto">
            <ProviderActions
              providerName={provider.name}
              isCurrent={isCurrent}
              currentLabel={currentLabel}
              currentAriaLabel={currentAriaLabel}
              canEdit={adapterAvailable}
              busy={busy}
              switching={switching}
              deleteButtonRef={deleteButtonRef}
              onSwitch={() => onSwitch(provider)}
              onEdit={() => onEdit(provider)}
              onDelete={() => onDelete(provider)}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
