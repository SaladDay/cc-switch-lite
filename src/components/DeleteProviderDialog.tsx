import { AlertTriangle, LoaderCircle } from "lucide-react";

import type { ProviderRecord } from "../lib/provider-types";
import { useModalDialog } from "../lib/use-modal-dialog";
import { Button } from "./ui/button";

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
      className="fixed inset-0 z-[60] m-auto max-h-[90vh] w-full max-w-sm overflow-hidden border border-border-default bg-background p-0 text-foreground shadow-lg sm:rounded-lg"
    >
      <div className="flex max-h-[90vh] min-h-0 flex-col">
        <div className="flex flex-shrink-0 flex-col px-6 pt-5 text-center sm:text-left">
          <h2
            id="delete-dialog-title"
            className="flex items-center gap-2 text-lg font-semibold leading-tight tracking-tight"
          >
            <AlertTriangle className="h-5 w-5 text-destructive" />
            Delete provider?
          </h2>
        </div>
        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-6 pt-3 text-center sm:text-left">
          <p
            id="delete-dialog-description"
            className="break-words whitespace-pre-line text-sm leading-relaxed text-muted-foreground"
          >
            {provider.name} will be removed from the provider catalog. Live
            configuration is not touched.
          </p>
          {error && (
            <p role="alert" className="text-sm text-red-600 dark:text-red-300">
              {error}
            </p>
          )}
        </div>

        <div className="flex flex-col-reverse items-center gap-2 px-6 py-5 pt-2 sm:flex-row sm:justify-end">
          <Button
            autoFocus
            type="button"
            variant="outline"
            disabled={busy}
            onClick={onCancel}
          >
            Cancel
          </Button>
          <Button
            type="button"
            variant="destructive"
            disabled={busy}
            onClick={onConfirm}
          >
            {busy && <LoaderCircle className="mr-2 h-4 w-4 animate-spin" />}
            {busy ? "Deleting…" : "Delete provider"}
          </Button>
        </div>
      </div>
    </dialog>
  );
}
