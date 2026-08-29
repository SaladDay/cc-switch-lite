import { useState, type FormEvent } from "react";
import { LoaderCircle } from "lucide-react";

import type {
  AdapterDescriptor,
  AdapterReference,
} from "../lib/provider-types";
import { useModalDialog } from "../lib/use-modal-dialog";
import { Button } from "./ui/button";

interface ImportProviderDialogProps {
  adapters: AdapterDescriptor[];
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onImport: (adapter: AdapterReference) => void;
}

export function ImportProviderDialog({
  adapters,
  busy,
  error,
  onCancel,
  onImport,
}: ImportProviderDialogProps) {
  const [selected, setSelected] = useState(0);
  const dialogRef = useModalDialog({ busy, onCancel });

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const adapter = adapters[selected];
    if (adapter) onImport(adapter.reference);
  };

  return (
    <dialog
      ref={dialogRef}
      aria-modal="true"
      aria-labelledby="import-provider-dialog-title"
      aria-describedby="import-provider-dialog-description"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onCancel();
      }}
      className="fixed inset-0 z-50 m-auto max-h-[90vh] w-full max-w-lg overflow-hidden border border-border-default bg-background p-0 text-foreground shadow-lg sm:rounded-lg"
    >
      <form onSubmit={submit} className="flex max-h-[90vh] min-h-0 flex-col">
        <div className="flex flex-shrink-0 flex-col space-y-1.5 border-b border-border-default bg-muted/20 px-6 py-5 text-center sm:text-left">
          <h2
            id="import-provider-dialog-title"
            className="text-lg font-semibold leading-tight tracking-tight"
          >
            Import provider
          </h2>
          <p
            id="import-provider-dialog-description"
            className="text-sm text-muted-foreground"
          >
            Choose the adapter that understands the live configuration.
          </p>
        </div>

        <div className="min-h-0 flex-1 space-y-5 overflow-y-auto px-6 py-5">
          {error && (
            <p
              role="alert"
              className="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-600 dark:text-red-300"
            >
              {error}
            </p>
          )}
          <label className="block space-y-2 text-sm font-medium">
            <span>Adapter</span>
            <select
              value={selected}
              onChange={(event) => setSelected(Number(event.target.value))}
              disabled={busy}
              className="flex h-9 w-full rounded-md border border-border-default bg-background px-3 py-1 text-sm text-foreground shadow-sm outline-none transition-colors focus:ring-2 focus:ring-blue-500/20 dark:focus:ring-blue-400/20 disabled:cursor-not-allowed disabled:opacity-50"
            >
              {adapters.map((adapter, index) => (
                <option
                  key={`${adapter.reference.pluginId}:${adapter.reference.pluginVersion}:${adapter.reference.adapterId}`}
                  value={index}
                >
                  {adapter.displayName}
                </option>
              ))}
            </select>
          </label>
        </div>

        <div className="flex flex-shrink-0 flex-col-reverse items-center gap-2 border-t border-border-default bg-muted/20 px-6 py-5 sm:flex-row sm:justify-end">
          <Button
            type="button"
            variant="outline"
            onClick={onCancel}
            disabled={busy}
          >
            Cancel
          </Button>
          <Button type="submit" disabled={busy || adapters.length === 0}>
            {busy && <LoaderCircle className="mr-2 h-4 w-4 animate-spin" />}
            Import provider
          </Button>
        </div>
      </form>
    </dialog>
  );
}
