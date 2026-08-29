import type { Ref } from "react";
import {
  Check,
  Edit,
  LoaderCircle,
  Minus,
  Play,
  Plus,
  Trash2,
} from "lucide-react";

import { Button } from "../ui/button";
import { cn } from "../../lib/utils";

interface ProviderActionsProps {
  providerName: string;
  isCurrent: boolean;
  currentLabel: string;
  currentAriaLabel: string;
  canEdit: boolean;
  canSwitch: boolean;
  canRemove: boolean;
  canDelete: boolean;
  isAdditive: boolean;
  isReadOnly: boolean;
  readOnlyLabel: string;
  busy: boolean;
  switching: boolean;
  deleteButtonRef?: Ref<HTMLButtonElement>;
  onSwitch: () => void;
  onRemove: () => void;
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
  canRemove,
  canDelete,
  isAdditive,
  isReadOnly,
  readOnlyLabel,
  busy,
  switching,
  deleteButtonRef,
  onSwitch,
  onRemove,
  onEdit,
  onDelete,
}: ProviderActionsProps) {
  const iconButtonClass = "h-8 w-8 p-1";
  const mainDisabled =
    busy ||
    isReadOnly ||
    !canSwitch ||
    (!isAdditive && isCurrent) ||
    (isAdditive && isCurrent && !canRemove);
  const mainLabel = isReadOnly
    ? "Managed"
    : isAdditive
      ? isCurrent
        ? "Remove"
        : "Add"
      : isCurrent
        ? currentLabel
        : "Switch";
  const mainVariant = isCurrent ? "secondary" : "default";
  const handleMainAction = isAdditive && isCurrent ? onRemove : onSwitch;

  return (
    <div className="flex items-center gap-1.5">
      <span className={cn("inline-flex", mainDisabled && "cursor-not-allowed")}>
        <Button
          size="sm"
          variant={mainVariant}
          onClick={handleMainAction}
          disabled={mainDisabled}
          aria-label={
            switching
              ? `Updating ${providerName}`
              : isReadOnly
                ? `${providerName}: ${readOnlyLabel}`
                : isAdditive && isCurrent
                  ? `Remove ${providerName} from configuration`
                  : isAdditive
                    ? `Add ${providerName} to configuration`
                    : isCurrent
                      ? currentAriaLabel
                      : `Switch to ${providerName}`
          }
          className={cn(
            "w-[4.5rem] px-2.5",
            isCurrent &&
              "bg-gray-200 text-muted-foreground hover:bg-gray-200 hover:text-muted-foreground dark:bg-gray-700 dark:hover:bg-gray-700",
            isAdditive &&
              !isCurrent &&
              "bg-emerald-500 hover:bg-emerald-600 dark:bg-emerald-600 dark:hover:bg-emerald-700",
            isAdditive &&
              isCurrent &&
              "bg-orange-100 text-orange-600 hover:bg-orange-200 dark:bg-orange-900/50 dark:text-orange-400 dark:hover:bg-orange-900/70",
          )}
        >
          {switching ? (
            <>
              <LoaderCircle className="h-4 w-4 animate-spin" />
              <span className="sr-only">Switching</span>
            </>
          ) : isReadOnly || (!isAdditive && isCurrent) ? (
            <Check className="h-4 w-4" />
          ) : isAdditive && isCurrent ? (
            <Minus className="h-4 w-4" />
          ) : isAdditive ? (
            <Plus className="h-4 w-4" />
          ) : (
            <Play className="h-4 w-4" />
          )}
          {!switching && mainLabel}
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
          disabled={!canDelete || busy}
          title={
            isReadOnly
              ? `${providerName}: ${readOnlyLabel}`
              : `Delete ${providerName}`
          }
          aria-label={`Delete ${providerName}`}
          className={cn(
            iconButtonClass,
            canDelete && !busy && "hover:text-red-500 dark:hover:text-red-400",
            (!canDelete || busy) &&
              "cursor-not-allowed text-muted-foreground opacity-40",
          )}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
