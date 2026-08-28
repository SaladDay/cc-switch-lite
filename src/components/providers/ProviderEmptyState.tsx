import { Download, LoaderCircle, Users } from "lucide-react";

import { Button } from "../ui/button";

interface ProviderEmptyStateProps {
  title: string;
  description: string;
  importLabel: string;
  disabled: boolean;
  importDisabled: boolean;
  importing: boolean;
  onCreate: () => void;
  onImport: () => void;
}

export function ProviderEmptyState({
  title,
  description,
  importLabel,
  disabled,
  importDisabled,
  importing,
  onCreate,
  onImport,
}: ProviderEmptyStateProps) {
  return (
    <div className="flex flex-col items-center justify-center rounded-lg border border-dashed border-border p-10 text-center">
      <div className="mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-muted">
        <Users className="h-7 w-7 text-muted-foreground" />
      </div>
      <h3 id="empty-state-title" className="text-lg font-semibold">
        {title}
      </h3>
      <p className="mt-2 max-w-lg text-sm text-muted-foreground">
        {description}
      </p>
      <div className="mt-6 flex flex-col gap-2">
        <Button onClick={onImport} disabled={importDisabled}>
          {importing ? (
            <LoaderCircle className="mr-2 h-4 w-4 animate-spin" />
          ) : (
            <Download className="mr-2 h-4 w-4" />
          )}
          {importLabel}
        </Button>
        <Button variant="outline" onClick={onCreate} disabled={disabled}>
          Add provider
        </Button>
      </div>
    </div>
  );
}
