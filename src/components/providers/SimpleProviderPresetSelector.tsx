import { useMemo, useState } from "react";
import { ArrowUpAZ, Search } from "lucide-react";

import type { SimpleProviderPreset } from "../../lib/provider-types";
import { ProviderIcon } from "../ProviderIcon";
import { Button } from "../ui/button";
import { Input } from "../ui/input";

interface SimpleProviderPresetSelectorProps {
  presets: SimpleProviderPreset[];
  selectedId: string;
  onSelect: (preset: SimpleProviderPreset | null) => void;
}

export function SimpleProviderPresetSelector({
  presets,
  selectedId,
  onSelect,
}: SimpleProviderPresetSelectorProps) {
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [alphabetical, setAlphabetical] = useState(false);
  const visiblePresets = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    const filtered = normalized
      ? presets.filter((preset) =>
          preset.name.toLowerCase().includes(normalized),
        )
      : presets;
    return alphabetical
      ? [...filtered].sort((left, right) => left.name.localeCompare(right.name))
      : filtered;
  }, [alphabetical, presets, query]);

  const presetClass = (active: boolean) =>
    `inline-flex w-full items-center justify-start gap-2 rounded-lg px-3 py-2 text-sm font-medium transition-colors ${
      active
        ? "bg-blue-500 text-white dark:bg-blue-600"
        : "bg-accent text-muted-foreground hover:bg-accent/80"
    }`;

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-medium">Provider preset</span>
        <div className="flex items-center gap-2">
          {searchOpen && (
            <Input
              autoFocus
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search presets…"
              aria-label="Search provider presets"
              className="h-8 w-60"
            />
          )}
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className={searchOpen ? "size-8 bg-accent" : "size-8"}
            aria-label="Search provider presets"
            aria-pressed={searchOpen}
            onClick={() => {
              setSearchOpen((current) => !current);
              if (searchOpen) setQuery("");
            }}
          >
            <Search className="size-4" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className={alphabetical ? "size-8 bg-accent" : "size-8"}
            aria-label="Toggle preset sorting"
            aria-pressed={alphabetical}
            onClick={() => setAlphabetical((current) => !current)}
          >
            <ArrowUpAZ className="size-4" />
          </Button>
        </div>
      </div>

      <div className="grid grid-cols-[repeat(auto-fill,minmax(150px,1fr))] gap-2">
        <button
          type="button"
          className={presetClass(selectedId === "custom")}
          onClick={() => onSelect(null)}
        >
          <span className="size-4" aria-hidden="true" />
          <span className="truncate">Custom</span>
        </button>
        {visiblePresets.map((preset) => (
          <button
            key={preset.id}
            type="button"
            className={presetClass(selectedId === preset.id)}
            title={preset.name}
            onClick={() => onSelect(preset)}
          >
            <ProviderIcon
              icon={preset.brandKey}
              name={preset.name}
              size={16}
              className="rounded-sm"
            />
            <span className="truncate">{preset.name}</span>
          </button>
        ))}
        {visiblePresets.length === 0 && (
          <div className="col-span-full rounded-md border border-dashed border-border-default px-3 py-2 text-xs text-muted-foreground">
            No matching presets.
          </div>
        )}
      </div>
      <p className="text-xs text-muted-foreground">
        Choose a preset, then adjust the fields below if needed.
      </p>
    </div>
  );
}
