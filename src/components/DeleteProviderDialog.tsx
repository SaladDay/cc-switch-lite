import type { ProviderRecord } from "../lib/provider-types";
import { useModalDialog } from "../lib/use-modal-dialog";

interface DeleteProviderDialogProps {
  provider: ProviderRecord;
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onConfirm: () => void;
}

export function DeleteProviderDialog({
  provider,
  busy,
  error,
  onCancel,
  onConfirm,
}: DeleteProviderDialogProps) {
  const dialogRef = useModalDialog({ busy, onCancel });

  return (
    <dialog
      ref={dialogRef}
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="delete-dialog-title"
      aria-describedby="delete-dialog-description"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onCancel();
      }}
      className="glass-card fixed inset-0 z-50 m-auto w-[calc(100%-3rem)] max-w-sm rounded-2xl p-6 text-foreground shadow-2xl"
    >
      <h2 id="delete-dialog-title" className="text-lg font-semibold">
        Delete provider?
      </h2>
      <p
        id="delete-dialog-description"
        className="mt-2 text-sm leading-6 text-muted-foreground"
      >
        {provider.name} will be removed from Lite storage. Live configuration is
        not touched.
      </p>
      {error && (
        <p role="alert" className="mt-3 text-sm text-red-600 dark:text-red-300">
          {error}
        </p>
      )}
      <div className="mt-6 flex justify-end gap-3">
        <button
          autoFocus
          type="button"
          disabled={busy}
          onClick={onCancel}
          className="h-10 rounded-xl border border-border px-4 text-sm font-medium hover:bg-muted disabled:opacity-50"
        >
          Cancel
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={onConfirm}
          className="h-10 rounded-xl bg-red-600 px-4 text-sm font-medium text-white disabled:opacity-60"
        >
          {busy ? "Deleting…" : "Delete provider"}
        </button>
      </div>
    </dialog>
  );
}
