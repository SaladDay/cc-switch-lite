import type { AppId, ProviderRecord } from "../../lib/provider-types";
import { ProviderCard, type ProviderListItem } from "./ProviderCard";
import { ProviderEmptyState } from "./ProviderEmptyState";

export type { ProviderListItem } from "./ProviderCard";

interface ProviderListProps {
  appId: AppId;
  items: ProviderListItem[];
  isLoading: boolean;
  emptyTitle: string;
  currentLabel: string;
  importLabel: string;
  disabled: boolean;
  busy: boolean;
  importing: boolean;
  switchingId: string | null;
  onCreate: () => void;
  onImport: () => void;
  onSwitch: (provider: ProviderRecord) => void;
  onEdit: (provider: ProviderRecord) => void;
  onDelete: (provider: ProviderRecord) => void;
  setDeleteButtonRef: (
    providerId: string,
    element: HTMLButtonElement | null,
  ) => void;
}

export function ProviderList({
  appId,
  items,
  isLoading,
  emptyTitle,
  currentLabel,
  importLabel,
  disabled,
  busy,
  importing,
  switchingId,
  onCreate,
  onImport,
  onSwitch,
  onEdit,
  onDelete,
  setDeleteButtonRef,
}: ProviderListProps) {
  if (isLoading) {
    return (
      <div
        role="status"
        aria-live="polite"
        aria-label="Loading providers"
        className="space-y-3"
      >
        <span className="sr-only">Loading providers</span>
        {[0, 1, 2].map((index) => (
          <div
            key={index}
            aria-hidden="true"
            className="h-28 w-full rounded-lg border border-dashed border-muted-foreground/40 bg-muted/40"
          />
        ))}
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <ProviderEmptyState
        title={emptyTitle}
        description="Add one manually, or import the API provider from your current live configuration."
        importLabel={importLabel}
        disabled={disabled}
        importing={importing}
        onCreate={onCreate}
        onImport={onImport}
      />
    );
  }

  return (
    <section className="mt-4 space-y-4" aria-label="Providers">
      <div className="space-y-3">
        {items.map((item) => (
          <ProviderCard
            key={item.provider.id}
            {...item}
            appId={appId}
            currentLabel={currentLabel}
            busy={busy}
            switching={switchingId === item.provider.id}
            deleteButtonRef={(element) =>
              setDeleteButtonRef(item.provider.id, element)
            }
            onSwitch={onSwitch}
            onEdit={onEdit}
            onDelete={onDelete}
          />
        ))}
      </div>
    </section>
  );
}
