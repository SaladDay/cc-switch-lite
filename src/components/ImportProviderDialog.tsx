import { useState, type FormEvent } from "react";
import { LoaderCircle, X } from "lucide-react";

import type {
  AdapterDescriptor,
  AdapterReference,
} from "../lib/provider-types";
import { useModalDialog } from "../lib/use-modal-dialog";

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
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onCancel();
      }}
      className="glass-card fixed inset-0 z-50 m-auto w-[min(460px,calc(100%-2rem))] rounded-2xl p-0 text-foreground shadow-2xl"
    >
      <form onSubmit={submit}>
        <div className="flex items-start justify-between border-b border-border px-6 py-5">
          <div>
            <h2
              id="import-provider-dialog-title"
              className="text-lg font-semibold"
            >
              Import provider
            </h2>
            <p className="mt-1 text-sm text-muted-foreground">
              Choose the adapter that understands the live configuration.
            </p>
          </div>
          <button
            type="button"
            onClick={onCancel}
            disabled={busy}
            className="inline-flex size-9 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"
            aria-label="Close import provider dialog"
          >
            <X className="size-4" />
          </button>
        </div>

        <div className="space-y-5 px-6 py-5">
          {error && (
            <p
              role="alert"
              className="rounded-xl border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-600 dark:text-red-300"
            >
              {error}
            </p>
          )}
          <label className="block text-sm font-medium">
            Adapter
            <select
              value={selected}
              onChange={(event) => setSelected(Number(event.target.value))}
              disabled={busy}
              className="mt-2 h-10 w-full rounded-xl border border-border bg-background px-3 text-sm outline-none focus:border-primary disabled:opacity-50"
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

        <div className="flex justify-end gap-3 border-t border-border px-6 py-4">
          <button
            type="button"
            onClick={onCancel}
            disabled={busy}
            className="h-10 rounded-xl border border-border px-4 text-sm font-medium hover:bg-muted disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={busy || adapters.length === 0}
            className="inline-flex h-10 min-w-28 items-center justify-center gap-2 rounded-xl bg-primary px-4 text-sm font-medium text-primary-foreground disabled:opacity-50"
          >
            {busy && <LoaderCircle className="size-4 animate-spin" />}
            Import provider
          </button>
        </div>
      </form>
    </dialog>
  );
}
