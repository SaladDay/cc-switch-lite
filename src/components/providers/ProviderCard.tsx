import type { Ref } from "react";

import { appDefinition } from "../../lib/apps";
import type { AppId, ProviderRecord } from "../../lib/provider-types";
import { cn } from "../../lib/utils";
import { ProviderIcon } from "../ProviderIcon";
import { ProviderActions } from "./ProviderActions";

export interface ProviderListItem {
  provider: ProviderRecord;
  endpoint: string;
  adapterAvailable: boolean;
  canEdit: boolean;
  canSwitch: boolean;
  canRemove: boolean;
  canDelete: boolean;
  isAdditive: boolean;
  isReadOnly: boolean;
  readOnlyLabel: string;
  isCurrent: boolean;
}

interface ProviderCardProps extends ProviderListItem {
  appId: AppId;
  currentLabel: string;
  busy: boolean;
  switching: boolean;
  deleteButtonRef?: Ref<HTMLButtonElement>;
  onSwitch: (provider: ProviderRecord) => void;
  onRemove: (provider: ProviderRecord) => void;
  onEdit: (provider: ProviderRecord) => void;
  onDelete: (provider: ProviderRecord) => void;
}

export function ProviderCard({
  provider,
  endpoint,
  adapterAvailable,
  canEdit,
  canSwitch,
  canRemove,
  canDelete,
  isAdditive,
  isReadOnly,
  readOnlyLabel,
  isCurrent,
  appId,
  currentLabel,
  busy,
  switching,
  deleteButtonRef,
  onSwitch,
  onRemove,
  onEdit,
  onDelete,
}: ProviderCardProps) {
  const displayEndpoint = adapterAvailable
    ? endpoint || "Default endpoint"
    : "Adapter unavailable";
  const currentAriaLabel = isAdditive
    ? `${provider.name} is in ${appDefinition(appId).label}`
    : appId === "claude"
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
              icon={appDefinition(appId).icon}
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
              {isReadOnly && (
                <span className="rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-600 dark:text-amber-300">
                  {readOnlyLabel}
                </span>
              )}
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
          <div className="flex flex-shrink-0 items-center gap-1.5 opacity-0 pointer-events-none transition-opacity duration-200 group-hover:opacity-100 group-hover:pointer-events-auto group-focus-within:opacity-100 group-focus-within:pointer-events-auto">
            <ProviderActions
              providerName={provider.name}
              isCurrent={isCurrent}
              currentLabel={currentLabel}
              currentAriaLabel={currentAriaLabel}
              canEdit={canEdit}
              canSwitch={canSwitch}
              canRemove={canRemove}
              canDelete={canDelete}
              isAdditive={isAdditive}
              isReadOnly={isReadOnly}
              readOnlyLabel={readOnlyLabel}
              busy={busy}
              switching={switching}
              deleteButtonRef={deleteButtonRef}
              onSwitch={() => onSwitch(provider)}
              onRemove={() => onRemove(provider)}
              onEdit={() => onEdit(provider)}
              onDelete={() => onDelete(provider)}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
