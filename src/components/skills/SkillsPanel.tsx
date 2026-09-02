import { useEffect, useMemo, useRef, useState } from "react";
import { LoaderCircle, Search, Sparkles } from "lucide-react";

import { errorMessage } from "../../lib/providers";
import { skillsApi } from "../../lib/skills";
import type {
  InstalledSkill,
  SkillAppState,
  SkillControlReason,
} from "../../lib/skill-types";
import type { CoreAppDescriptor } from "../../lib/provider-types";
import { AppCountBar } from "../common/AppCountBar";
import { AppToggleGroup } from "../common/AppToggleGroup";
import { ListItemRow } from "../common/ListItemRow";
import { ManagementListSearch } from "../common/ManagementListSearch";

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
      <AppCountBar
        totalLabel={`${skills.length} installed`}
        counts={counts}
        apps={skillApps}
      />

      <ManagementListSearch
        value={search}
        onValueChange={setSearch}
        placeholder="Search installed skill name, description, or repo..."
        ariaLabel="Search installed skills"
        clearLabel="Clear Skill search"
      />

      {error && (
        <p
          role="alert"
          className="mb-4 rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-600 dark:text-red-300"
        >
          {error}
        </p>
      )}

      <div className="-mr-3 min-h-0 flex-1 overflow-y-auto">
        <div className="pb-24 pr-3">
          {loading ? (
            <div className="flex justify-center py-12 text-muted-foreground">
              <LoaderCircle className="h-5 w-5 animate-spin" />
            </div>
          ) : skills.length === 0 ? (
            <div className="py-12 text-center">
              <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-muted">
                <Sparkles className="h-6 w-6 text-muted-foreground" />
              </div>
              <h2 className="text-lg font-medium">No skills installed</h2>
              <p className="mt-2 text-sm text-muted-foreground">
                Skills already present in the shared catalog will appear here.
              </p>
            </div>
          ) : filtered.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-center text-muted-foreground">
              <Search className="mb-4 h-10 w-10 opacity-40" />
              <p className="text-sm">No installed skills match your search</p>
            </div>
          ) : (
            <div className="overflow-hidden rounded-xl border border-border-default">
              {filtered.map((skill, index) => (
                <ListItemRow
                  key={skill.id}
                  isLast={index === filtered.length - 1}
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
                  <AppToggleGroup
                    apps={skillApps}
                    stateFor={(appId) => {
                      const state = stateFor(skill, appId);
                      const enabled = state?.selected === true;
                      const warning =
                        state?.enabled === null ||
                        (state?.enabled !== undefined &&
                          state.enabled !== state.selected);
                      const canToggle =
                        state?.selected !== null &&
                        (enabled ? state?.canDisable : state?.canEnable);
                      return {
                        enabled,
                        warning,
                        disabled: !canToggle,
                        pending: busyKey === `${skill.id}:${appId}`,
                        title: stateTitle(state),
                      };
                    }}
                    onToggle={(appId) => {
                      const app = skillApps.find((item) => item.id === appId);
                      const state = stateFor(skill, appId);
                      if (app && state) void toggle(skill, app, state);
                    }}
                    ariaLabel={(_, state, label) =>
                      `${state.enabled ? "Disable" : "Enable"} ${skill.name} for ${label}${state.title ? `. ${state.title}` : ""}`
                    }
                    disabled={blocked}
                  />
                </ListItemRow>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
