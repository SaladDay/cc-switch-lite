import { appDefinition } from "../../lib/apps";
import type { CoreAppDescriptor } from "../../lib/provider-types";
import { cn } from "../../lib/utils";
import { appBadgeClass } from "./app-management-style";

interface AppCountBarProps {
  totalLabel: string;
  counts: Record<string, number>;
  apps: CoreAppDescriptor[];
}

export function AppCountBar({ totalLabel, counts, apps }: AppCountBarProps) {
  return (
    <div className="mb-4 flex flex-shrink-0 items-center gap-4 rounded-xl border border-white/10 px-6 py-4 glass">
      <span className="inline-flex h-7 shrink-0 items-center whitespace-nowrap rounded-full border border-border-default bg-background/50 px-3 py-0.5 text-xs font-semibold">
        {totalLabel}
      </span>
      <div className="min-w-0 flex-1 overflow-x-auto">
        <div className="ml-auto flex w-max min-w-full items-center justify-end gap-2">
          {apps.map((app) => {
            const definition = appDefinition(app.id, [app]);
            return (
              <span
                key={app.id}
                className={cn(
                  "inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border border-transparent px-2.5 py-0.5 text-xs font-semibold",
                  appBadgeClass(app.id),
                )}
              >
                <span className="opacity-75">{definition.label}:</span>
                <span className="font-bold">{counts[app.id] ?? 0}</span>
              </span>
            );
          })}
        </div>
      </div>
    </div>
  );
}
