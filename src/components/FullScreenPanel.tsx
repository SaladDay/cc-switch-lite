import type { CSSProperties, ReactNode } from "react";
import { ArrowLeft } from "lucide-react";

import { useModalDialog } from "../lib/use-modal-dialog";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";

interface FullScreenPanelProps {
  title: string;
  titleId: string;
  description?: string;
  closeLabel: string;
  busy: boolean;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  contentClassName?: string;
}

const DRAG_BAR_HEIGHT = 28;
const HEADER_HEIGHT = 64;

export function FullScreenPanel({
  title,
  titleId,
  description,
  closeLabel,
  busy,
  onClose,
  children,
  footer,
  contentClassName,
}: FullScreenPanelProps) {
  const dialogRef = useModalDialog({ busy, onCancel: onClose });

  return (
    <dialog
      ref={dialogRef}
      aria-modal="true"
      aria-labelledby={titleId}
      aria-describedby={description ? `${titleId}-description` : undefined}
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onClose();
      }}
      className="fixed inset-0 z-[60] m-0 h-screen max-h-none w-screen max-w-none overflow-hidden border-0 bg-background p-0 text-foreground"
    >
      <div className="flex h-full min-h-0 flex-col">
        <div
          data-tauri-drag-region
          style={
            {
              WebkitAppRegion: "drag",
              height: DRAG_BAR_HEIGHT,
            } as CSSProperties
          }
        />

        <div
          className="flex flex-shrink-0 items-center"
          data-tauri-drag-region
          style={
            {
              WebkitAppRegion: "drag",
              backgroundColor: "hsl(var(--background))",
              height: HEADER_HEIGHT,
            } as CSSProperties
          }
        >
          <div
            className="flex w-full items-center gap-4 px-6"
            data-tauri-drag-region
            style={{ WebkitAppRegion: "drag" } as CSSProperties}
          >
            <Button
              type="button"
              variant="outline"
              size="icon"
              disabled={busy}
              onClick={onClose}
              className="select-none rounded-lg"
              style={{ WebkitAppRegion: "no-drag" } as CSSProperties}
              aria-label={closeLabel}
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <div className="min-w-0 select-none">
              <h2
                id={titleId}
                className="truncate text-lg font-semibold text-foreground"
              >
                {title}
              </h2>
              {description && (
                <p
                  id={`${titleId}-description`}
                  className="truncate text-xs text-muted-foreground"
                >
                  {description}
                </p>
              )}
            </div>
          </div>
        </div>

        <div className="scroll-overlay flex-1 overflow-y-auto">
          <div className={cn("w-full space-y-6 px-6 py-6", contentClassName)}>
            {children}
          </div>
        </div>

        {footer && (
          <div
            className="flex-shrink-0 border-t border-border-default py-4"
            style={{ backgroundColor: "hsl(var(--background))" }}
          >
            <div className="flex items-center justify-end gap-3 px-6">
              {footer}
            </div>
          </div>
        )}
      </div>
    </dialog>
  );
}
