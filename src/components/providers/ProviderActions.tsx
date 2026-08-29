import type { Ref } from "react";
import { Check, Edit, LoaderCircle, Play, Trash2 } from "lucide-react";

import { Button } from "../ui/button";
import { cn } from "../../lib/utils";

interface ProviderActionsProps {
  providerName: string;
  isCurrent: boolean;
  currentLabel: string;
  currentAriaLabel: string;
  canEdit: boolean;
  canSwitch: boolean;
  busy: boolean;
  switching: boolean;
  deleteButtonRef?: Ref<HTMLButtonElement>;
  onSwitch: () => void;
  onEdit: () => void;
  onDelete: () => void;
}

export function ProviderActions({
  providerName,
  isCurrent,
  currentLabel,
  currentAriaLabel,
  canEdit,
  canSwitch,
  busy,
  switching,
  deleteButtonRef,
  onSwitch,
  onEdit,
  onDelete,
}: ProviderActionsProps) {
  const iconButtonClass = "h-8 w-8 p-1";
  const mainDisabled = isCurrent || !canSwitch || busy;

  return (
    <div className="flex items-center gap-1.5">
      <span className={cn("inline-flex", mainDisabled && "cursor-not-allowed")}>
        <Button
          size="sm"
          variant={isCurrent ? "secondary" : "default"}
          onClick={onSwitch}
          disabled={mainDisabled}
          aria-label={
            switching
              ? `Switching to ${providerName}`
              : isCurrent
                ? currentAriaLabel
                : `Switch to ${providerName}`
          }
          className={cn(
            "w-[4.5rem] px-2.5",
            isCurrent &&
              "bg-gray-200 text-muted-foreground hover:bg-gray-200 hover:text-muted-foreground dark:bg-gray-700 dark:hover:bg-gray-700",
          )}
        >
          {switching ? (
            <>
              <LoaderCircle className="h-4 w-4 animate-spin" />
              <span className="sr-only">Switching</span>
            </>
          ) : isCurrent ? (
            <Check className="h-4 w-4" />
          ) : (
            <Play className="h-4 w-4" />
          )}
          {!switching && (isCurrent ? currentLabel : "Switch")}
        </Button>
      </span>

      <div className="flex items-center gap-1">
        <Button
          size="icon"
          variant="ghost"
          onClick={onEdit}
          disabled={!canEdit || busy}
          title={`Edit ${providerName}`}
          aria-label={`Edit ${providerName}`}
          className={cn(
            iconButtonClass,
            (!canEdit || busy) &&
              "cursor-not-allowed text-muted-foreground opacity-40",
          )}
        >
          <Edit className="h-4 w-4" />
        </Button>

        <Button
          ref={deleteButtonRef}
          size="icon"
          variant="ghost"
          onClick={onDelete}
          disabled={busy}
          title={`Delete ${providerName}`}
          aria-label={`Delete ${providerName}`}
          className={cn(
            iconButtonClass,
            !busy && "hover:text-red-500 dark:hover:text-red-400",
            busy && "cursor-not-allowed text-muted-foreground opacity-40",
          )}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
