import { useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, LoaderCircle, Search, Sparkles, X } from "lucide-react";

import { appDefinition } from "../../lib/apps";
import { errorMessage } from "../../lib/providers";
import { skillsApi } from "../../lib/skills";
import type {
  InstalledSkill,
  SkillAppState,
  SkillControlReason,
} from "../../lib/skill-types";
import type { CoreAppDescriptor } from "../../lib/provider-types";
import { ProviderIcon } from "../ProviderIcon";
import { Input } from "../ui/input";

interface SkillsPanelProps {
  apps: CoreAppDescriptor[];
  onInteractionBlockedChange?: (blocked: boolean) => void;
}

const REASON_LABELS: Record<SkillControlReason, string> = {
  missingSource: "The installed Skill source is missing.",
  invalidSource: "The installed Skill source is invalid.",
  recoveryPending: "A previous Skill update needs recovery.",
  managedReferenceDrift: "The managed Skill reference has changed.",
  catalogDrift: "The shared selection and application state differ.",
  nativeConflict: "The application directory contains an unmanaged entry.",
  unifiedConflict: "The shared Skill directory contains a conflicting entry.",
  observationFailed: "The application state could not be inspected safely.",
  invalidConfiguration: "The application configuration is invalid.",
  directUnifiedDiscovery: "This application reads the unified Skill directly.",
  required: "This Skill is required by the application.",
  globallyDisabled: "This Skill is disabled by the application.",
  externallyDisabled: "This Skill is disabled outside CC Switch.",
};

function searchText(skill: InstalledSkill): string {
  return [skill.id, skill.name, skill.description, skill.directory]
    .filter((value): value is string => typeof value === "string")
    .join("\n")
    .toLocaleLowerCase();
}

function stateFor(
  skill: InstalledSkill,
  appId: string,
): SkillAppState | undefined {
  return skill.apps.find((state) => state.app === appId);
}

function stateTitle(state: SkillAppState | undefined): string | undefined {
  return state?.reason ? REASON_LABELS[state.reason] : undefined;
}

export function SkillsPanel({
  apps,
  onInteractionBlockedChange,
}: SkillsPanelProps) {
  const skillApps = useMemo(
    () => apps.filter((app) => app.capabilities.includes("skills")),
    [apps],
  );
  const [skills, setSkills] = useState<InstalledSkill[]>([]);
  const [loading, setLoading] = useState(true);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const refreshGeneration = useRef(0);
  const blocked = loading || busyKey !== null;

  useEffect(() => {
    onInteractionBlockedChange?.(blocked);
    return () => onInteractionBlockedChange?.(false);
  }, [blocked, onInteractionBlockedChange]);

  const refresh = async () => {
    const generation = ++refreshGeneration.current;
    const next = await skillsApi.list();
    if (generation === refreshGeneration.current) setSkills(next);
  };

  useEffect(() => {
    let mounted = true;
    setLoading(true);
    setError(null);
    void refresh()
      .catch((caught) => {
        if (mounted) setError(errorMessage(caught));
      })
      .finally(() => {
        if (mounted) setLoading(false);
      });
    return () => {
      mounted = false;
      refreshGeneration.current += 1;
    };
  }, []);

  const toggle = async (
    skill: InstalledSkill,
    app: CoreAppDescriptor,
    state: SkillAppState,
  ) => {
    if (blocked || state.selected === null) return;
    const enabled = !state.selected;
    if (enabled ? !state.canEnable : !state.canDisable) return;
    const key = `${skill.id}:${app.id}`;
    setBusyKey(key);
    setError(null);
    try {
      await skillsApi.toggle(skill.id, app.id, enabled);
      await refresh();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusyKey(null);
    }
  };

  const normalizedSearch = search.trim().toLocaleLowerCase();
  const filtered = useMemo(
    () =>
      normalizedSearch
        ? skills.filter((skill) => searchText(skill).includes(normalizedSearch))
        : skills,
    [normalizedSearch, skills],
  );
  const counts = useMemo(
    () =>
      Object.fromEntries(
        skillApps.map((app) => [
          app.id,
          skills.filter((skill) => stateFor(skill, app.id)?.selected === true)
            .length,
        ]),
      ),
    [skillApps, skills],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden px-6">
      <div className="mb-4 flex flex-shrink-0 items-center gap-4 rounded-xl border border-white/10 px-6 py-4 glass">
        <span className="h-7 shrink-0 rounded-full border border-border-default bg-background/50 px-3 py-1 text-xs font-medium">
          {skills.length} installed
        </span>
        <div className="ml-auto flex min-w-0 gap-2 overflow-x-auto">
          {skillApps.map((app) => {
            const definition = appDefinition(app.id, [app]);
            return (
              <span
                key={app.id}
                className="flex shrink-0 items-center gap-1.5 rounded-full bg-muted px-2.5 py-1 text-xs text-muted-foreground"
              >
                <ProviderIcon
                  icon={definition.icon}
                  name={definition.label}
                  size={14}
                />
                {definition.label}:{" "}
                <strong className="text-foreground">{counts[app.id]}</strong>
              </span>
            );
          })}
        </div>
      </div>

      <div className="relative mb-4 flex-shrink-0" role="search">
        <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          placeholder="Search installed Skills…"
          aria-label="Search installed Skills"
          className="pl-9 pr-9"
        />
        {search && (
          <button
            type="button"
            onClick={() => setSearch("")}
            aria-label="Clear Skill search"
            className="absolute right-2 top-1/2 flex h-7 w-7 -translate-y-1/2 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted"
          >
            <X className="h-4 w-4" />
          </button>
        )}
      </div>

      {error && (
        <p
          role="alert"
          className="mb-4 rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-600 dark:text-red-300"
        >
          {error}
        </p>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto pb-20">
        {loading ? (
          <div className="flex justify-center py-12 text-muted-foreground">
            <LoaderCircle className="h-5 w-5 animate-spin" />
          </div>
        ) : skills.length === 0 ? (
          <div className="py-12 text-center">
            <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-muted">
              <Sparkles className="h-6 w-6 text-muted-foreground" />
            </div>
            <h2 className="text-lg font-medium">No installed Skills</h2>
            <p className="mt-2 text-sm text-muted-foreground">
              Install Skills in CC Switch, then choose which applications use
              them here.
            </p>
          </div>
        ) : filtered.length === 0 ? (
          <div className="py-12 text-center text-sm text-muted-foreground">
            No matching Skills.
          </div>
        ) : (
          <div className="overflow-hidden rounded-xl border border-border-default">
            {filtered.map((skill, index) => (
              <div
                key={skill.id}
                className={`group flex items-center gap-3 px-4 py-2.5 transition-colors hover:bg-muted/50 ${
                  index < filtered.length - 1
                    ? "border-b border-border-default"
                    : ""
                }`}
              >
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-medium">{skill.name}</p>
                  <p
                    className="truncate text-xs text-muted-foreground"
                    title={skill.description || skill.directory}
                  >
                    {skill.description || skill.directory}
                  </p>
                </div>
                <div className="flex shrink-0 items-center gap-1.5">
                  {skillApps.map((app) => {
                    const definition = appDefinition(app.id, [app]);
                    const state = stateFor(skill, app.id);
                    const selected = state?.selected === true;
                    const drift =
                      state?.enabled === null ||
                      (state?.enabled !== undefined &&
                        state.enabled !== state.selected);
                    const canToggle =
                      state?.selected !== null &&
                      (selected ? state?.canDisable : state?.canEnable);
                    const key = `${skill.id}:${app.id}`;
                    const title = stateTitle(state);
                    return (
                      <button
                        key={app.id}
                        type="button"
                        disabled={blocked || !canToggle}
                        onClick={() => state && void toggle(skill, app, state)}
                        aria-label={`${selected ? "Disable" : "Enable"} ${skill.name} for ${definition.label}${title ? `. ${title}` : ""}`}
                        aria-pressed={selected}
                        title={title || definition.label}
                        className={`relative flex h-7 w-7 cursor-pointer items-center justify-center rounded-lg transition-all disabled:cursor-not-allowed ${
                          drift
                            ? "bg-amber-500/15 opacity-100"
                            : selected
                              ? "bg-emerald-500/15 opacity-100"
                              : "opacity-35 hover:opacity-70"
                        }`}
                      >
                        {busyKey === key ? (
                          <LoaderCircle className="h-4 w-4 animate-spin" />
                        ) : (
                          <ProviderIcon
                            icon={definition.icon}
                            name={definition.label}
                            size={17}
                          />
                        )}
                        {drift && busyKey !== key && (
                          <AlertTriangle
                            className="absolute -right-1 -top-1 h-3 w-3 text-amber-500"
                            aria-hidden="true"
                          />
                        )}
                      </button>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
