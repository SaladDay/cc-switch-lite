import { AlertTriangle, LoaderCircle } from "lucide-react";

import type { McpServer } from "../../lib/mcp-types";
import { useModalDialog } from "../../lib/use-modal-dialog";
import { Button } from "../ui/button";

interface DeleteMcpDialogProps {
  server: McpServer;
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onConfirm: () => void;
}

export function DeleteMcpDialog({
  server,
  busy,
  error,
  onCancel,
  onConfirm,
}: DeleteMcpDialogProps) {
  const dialogRef = useModalDialog({ busy, onCancel });
  return (
    <dialog
      ref={dialogRef}
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="delete-mcp-title"
      aria-describedby="delete-mcp-description"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onCancel();
      }}
      className="fixed inset-0 z-[60] m-auto max-h-[90vh] w-full max-w-sm overflow-hidden border border-border-default bg-background p-0 text-foreground shadow-lg sm:rounded-lg"
    >
      <div className="flex flex-col p-6">
        <h2
          id="delete-mcp-title"
          className="flex items-center gap-2 text-lg font-semibold"
        >
          <AlertTriangle className="h-5 w-5 text-destructive" />
          Delete MCP server?
        </h2>
        <p
          id="delete-mcp-description"
          className="mt-3 text-sm text-muted-foreground"
        >
          {server.name} will be removed from the shared catalog and from every
          application where it is enabled.
        </p>
        {error && (
          <p role="alert" className="mt-3 text-sm text-red-600">
            {error}
          </p>
        )}
        <div className="mt-5 flex justify-end gap-2">
          <Button variant="outline" disabled={busy} onClick={onCancel}>
            Cancel
          </Button>
          <Button variant="destructive" disabled={busy} onClick={onConfirm}>
            {busy && <LoaderCircle className="mr-2 h-4 w-4 animate-spin" />}
            {busy ? "Deleting…" : "Delete server"}
          </Button>
        </div>
      </div>
    </dialog>
  );
}
