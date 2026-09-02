import type { ReactNode } from "react";

interface ListItemRowProps {
  isLast?: boolean;
  children: ReactNode;
}

export function ListItemRow({ isLast, children }: ListItemRowProps) {
  return (
    <div
      className={`group flex items-center gap-3 px-4 py-2.5 transition-colors hover:bg-muted/50 ${
        !isLast ? "border-b border-border-default" : ""
      }`}
    >
      {children}
    </div>
  );
}
