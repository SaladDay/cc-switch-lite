import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { Search, X } from "lucide-react";

import type { AppId, ProviderRecord } from "../../lib/provider-types";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { ProviderCard, type ProviderListItem } from "./ProviderCard";
import { ProviderEmptyState } from "./ProviderEmptyState";

export type { ProviderListItem } from "./ProviderCard";

interface ProviderListProps {
  appId: AppId;
  items: ProviderListItem[];
  isLoading: boolean;
  emptyTitle: string;
  currentLabel: string;
  importLabel: string;
  disabled: boolean;
  importDisabled: boolean;
  busy: boolean;
  importing: boolean;
  switchingId: string | null;
  onCreate: () => void;
  onImport: () => void;
  onSwitch: (provider: ProviderRecord) => void;
  onRemove: (provider: ProviderRecord) => void;
  onEdit: (provider: ProviderRecord) => void;
  onDelete: (provider: ProviderRecord) => void;
  setDeleteButtonRef: (
    providerId: string,
    element: HTMLButtonElement | null,
  ) => void;
}

export function ProviderList({
  appId,
  items,
  isLoading,
  emptyTitle,
  currentLabel,
  importLabel,
  disabled,
  importDisabled,
  busy,
  importing,
  switchingId,
  onCreate,
  onImport,
  onSwitch,
  onRemove,
  onEdit,
  onDelete,
  setDeleteButtonRef,
}: ProviderListProps) {
  const [searchTerm, setSearchTerm] = useState("");
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const searchOwnerRef = useRef<HTMLElement | null>(null);
  const sectionRef = useRef<HTMLElement>(null);
  const reduceMotion = useReducedMotion();
  const closeSearch = useCallback(() => {
    setIsSearchOpen(false);
    setSearchTerm("");
    const owner = searchOwnerRef.current;
    searchOwnerRef.current = null;
    requestAnimationFrame(() => {
      if (owner?.isConnected && !owner.closest("[inert]")) owner.focus();
    });
  }, []);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      const key = event.key.toLowerCase();
      if ((event.metaKey || event.ctrlKey) && key === "f") {
        if (
          !sectionRef.current ||
          sectionRef.current.closest("[inert]") ||
          document.querySelector("dialog[open]")
        )
          return;
        const active = document.activeElement;
        if (
          active instanceof HTMLElement &&
          (active.matches("input, textarea, select") ||
            active.isContentEditable)
        )
          return;
        event.preventDefault();
        searchOwnerRef.current = active instanceof HTMLElement ? active : null;
        setIsSearchOpen(true);
        return;
      }
      if (key === "escape" && isSearchOpen) closeSearch();
    };

    globalThis.addEventListener("keydown", handleKeyDown);
    return () => globalThis.removeEventListener("keydown", handleKeyDown);
  }, [closeSearch, isSearchOpen]);

  useEffect(() => {
    if (!isSearchOpen) return;
    const frame = requestAnimationFrame(() => {
      searchInputRef.current?.focus();
      searchInputRef.current?.select();
    });
    return () => cancelAnimationFrame(frame);
  }, [isSearchOpen]);

  const filteredItems = useMemo(() => {
    const keyword = searchTerm.trim().toLowerCase();
    if (!keyword) return items;
    return items.filter(({ provider, endpoint }) =>
      [provider.name, provider.notes, provider.websiteUrl, endpoint].some(
        (field) => field?.toLowerCase().includes(keyword),
      ),
    );
  }, [items, searchTerm]);

  if (isLoading) {
    return (
      <div
        role="status"
        aria-live="polite"
        aria-label="Loading providers"
        className="space-y-3"
      >
        <span className="sr-only">Loading providers</span>
        {[0, 1, 2].map((index) => (
          <div
            key={index}
            aria-hidden="true"
            className="h-28 w-full rounded-lg border border-dashed border-muted-foreground/40 bg-muted/40"
          />
        ))}
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <ProviderEmptyState
        title={emptyTitle}
        description="Add one manually, or import the API provider from your current live configuration."
        importLabel={importLabel}
        disabled={disabled}
        importDisabled={importDisabled}
        importing={importing}
        onCreate={onCreate}
        onImport={onImport}
      />
    );
  }

  return (
    <section ref={sectionRef} className="mt-4 space-y-4" aria-label="Providers">
      <AnimatePresence>
        {isSearchOpen && (
          <motion.div
            key="provider-search"
            initial={reduceMotion ? false : { opacity: 0, y: -8, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -8, scale: 0.98 }}
            transition={{ duration: reduceMotion ? 0 : 0.18, ease: "easeOut" }}
            className="fixed left-1/2 top-[6.5rem] z-40 w-[min(90vw,26rem)] -translate-x-1/2 sm:right-6 sm:left-auto sm:translate-x-0"
          >
            <div className="space-y-3 rounded-2xl border border-white/10 bg-background/95 p-4 shadow-md shadow-black/20 backdrop-blur-md">
              <div className="relative flex items-center gap-2">
                <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  ref={searchInputRef}
                  value={searchTerm}
                  onChange={(event) => setSearchTerm(event.target.value)}
                  placeholder="Search name, notes, or URL…"
                  aria-label="Search providers"
                  className="pl-9 pr-16"
                />
                {searchTerm && (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="absolute right-11 top-1/2 -translate-y-1/2 text-xs"
                    onClick={() => setSearchTerm("")}
                  >
                    Clear
                  </Button>
                )}
                <Button
                  variant="ghost"
                  size="icon"
                  className="ml-auto"
                  onClick={closeSearch}
                  aria-label="Close provider search"
                >
                  <X className="h-4 w-4" />
                </Button>
              </div>
              <div className="flex flex-wrap items-center justify-between gap-2 text-[11px] text-muted-foreground">
                <span>Matches provider name, notes, and URL.</span>
                <span>Press Esc to close</span>
              </div>
            </div>
          </motion.div>
        )}
      </AnimatePresence>

      {filteredItems.length === 0 ? (
        <div className="rounded-lg border border-dashed border-border px-6 py-8 text-center text-sm text-muted-foreground">
          No providers match your search.
        </div>
      ) : (
        <div className="space-y-3">
          {filteredItems.map((item) => (
            <ProviderCard
              key={item.provider.id}
              {...item}
              appId={appId}
              currentLabel={currentLabel}
              busy={busy}
              switching={switchingId === item.provider.id}
              deleteButtonRef={(element) =>
                setDeleteButtonRef(item.provider.id, element)
              }
              onSwitch={onSwitch}
              onRemove={onRemove}
              onEdit={onEdit}
              onDelete={onDelete}
            />
          ))}
        </div>
      )}
    </section>
  );
}
