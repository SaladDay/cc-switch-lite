import { useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  LoaderCircle,
  LockKeyhole,
  Search,
  Sparkles,
  X,
} from "lucide-react";

import { appDefinition } from "../../lib/apps";
import type { AppId, CoreAppDescriptor } from "../../lib/provider-types";
import type { SkillRecord } from "../../lib/skill-types";
import { skillsApi } from "../../lib/skills";
import { errorMessage } from "../../lib/providers";
import { ProviderIcon } from "../ProviderIcon";
import { Input } from "../ui/input";

interface SkillsPanelProps {
  apps: CoreAppDescriptor[];
  onInteractionBlockedChange?: (blocked: boolean) => void;
}

export function SkillsPanel({
  apps,
  onInteractionBlockedChange,
}: SkillsPanelProps) {
  const [skills, setSkills] = useState<SkillRecord[]>([]);
  const [search, setSearch] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);
  const [pending, setPending] = useState<string | null>(null);
  const writeLock = useRef(false);

  useEffect(() => {
    onInteractionBlockedChange?.(pending !== null);
  }, [onInteractionBlockedChange, pending]);

  useEffect(
    () => () => {
      onInteractionBlockedChange?.(false);
    },
    [onInteractionBlockedChange],
  );

  useEffect(() => {
    let ignore = false;
    setLoading(true);
    skillsApi
      .list()
      .then((items) => {
        if (!ignore) {
          setSkills(items);
          setError(null);
          setStale(false);
        }
      })
      .catch((reason: unknown) => {
        if (!ignore) {
          setError(errorMessage(reason));
          setStale(true);
        }
      })
      .finally(() => {
        if (!ignore) setLoading(false);
      });
    return () => {
      ignore = true;
    };
  }, [reloadKey]);

  const counts = useMemo(
    () =>
      Object.fromEntries(
        apps.map((app) => [
          app.id,
          skills.filter((skill) => skill.apps[app.id]?.enabled === true).length,
        ]),
      ),
    [apps, skills],
  );
  const filtered = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    if (!query) return skills;
    return skills.filter((skill) =>
      [
        skill.name,
        skill.id,
        skill.description,
        skill.directory,
        skill.repoOwner,
        skill.repoName,
        skill.repoOwner && skill.repoName
          ? `${skill.repoOwner}/${skill.repoName}`
          : undefined,
      ].some((value) => value?.toLocaleLowerCase().includes(query)),
    );
  }, [search, skills]);

  const toggle = async (skill: SkillRecord, appId: AppId, enabled: boolean) => {
    if (writeLock.current) return;
    writeLock.current = true;
    setPending(`${skill.id}:${appId}`);
    setError(null);
    try {
      await skillsApi.toggle(skill.id, appId, enabled);
      const refreshed = await skillsApi.list();
      setSkills(refreshed);
      setStale(false);
    } catch (reason) {
      setError(errorMessage(reason));
      setStale(true);
    } finally {
      writeLock.current = false;
      setPending(null);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden px-6">
      <div className="mb-4 flex flex-shrink-0 items-center gap-4 rounded-xl border border-white/10 px-6 py-4 glass">
        <span className="h-7 shrink-0 rounded-full border border-border-default bg-background/50 px-3 py-1 text-xs font-medium">
          {skills.length} installed
        </span>
        <div className="ml-auto flex min-w-0 gap-2 overflow-x-auto">
          {apps.map((app) => {
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
          onKeyDown={(event) => {
            if (event.key === "Escape" && search) setSearch("");
          }}
          placeholder="Search installed Skills…"
          aria-label="Search installed Skills"
          className="pl-9 pr-9"
        />
        {search && (
          <button
            type="button"
            onClick={() => setSearch("")}
            aria-label="Clear Skill search"
            className="absolute right-2 top-1/2 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-md text-muted-foreground hover:bg-muted"
          >
            <X className="h-4 w-4" />
          </button>
        )}
      </div>

      {error && (
        <div
          role="alert"
          className="mb-4 flex items-center gap-3 rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-600 dark:text-red-300"
        >
          <span className="min-w-0 flex-1">{error}</span>
          <button
            type="button"
            onClick={() => setReloadKey((key) => key + 1)}
            className="shrink-0 rounded-md border border-current/30 px-2.5 py-1 text-xs font-medium hover:bg-red-500/10"
          >
            Retry
          </button>
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto pb-20">
        {loading ? (
          <div className="flex justify-center py-12 text-muted-foreground">
            <LoaderCircle className="h-5 w-5 animate-spin" />
          </div>
        ) : error && skills.length === 0 ? null : skills.length === 0 ? (
          <div className="py-12 text-center">
            <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-muted">
              <Sparkles className="h-6 w-6 text-muted-foreground" />
            </div>
            <h2 className="text-lg font-medium">No installed Skills</h2>
            <p className="mt-2 text-sm text-muted-foreground">
              Skills installed by CC Switch will appear here.
            </p>
          </div>
        ) : filtered.length === 0 ? (
          <div className="py-12 text-center text-sm text-muted-foreground">
            No matching Skills.
          </div>
        ) : (
          <div className="overflow-hidden rounded-xl border border-border-default">
            {filtered.map((skill, index) => {
              const source =
                skill.repoOwner && skill.repoName
                  ? `${skill.repoOwner}/${skill.repoName}`
                  : "Local";
              return (
                <div
                  key={skill.id}
                  className={`group flex items-center gap-3 px-4 py-2.5 transition-colors hover:bg-muted/50 ${
                    index < filtered.length - 1
                      ? "border-b border-border-default"
                      : ""
                  }`}
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <p className="truncate text-sm font-medium">
                        {skill.name || skill.directory}
                      </p>
                      <span className="shrink-0 text-xs text-muted-foreground/50">
                        {source}
                      </span>
                      {skill.issue && (
                        <AlertTriangle
                          className="h-3.5 w-3.5 shrink-0 text-amber-500"
                          aria-label="Skill unavailable"
                        />
                      )}
                    </div>
                    <p
                      className="truncate text-xs text-muted-foreground"
                      title={
                        skill.issue || skill.description || skill.directory
                      }
                    >
                      {skill.issue || skill.description || skill.directory}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    {apps.map((app) => {
                      const definition = appDefinition(app.id, [app]);
                      const appState = skill.apps[app.id] ?? {
                        enabled: null,
                        issue: "Skill state was not reported",
                      };
                      const enabled = appState.enabled === true;
                      const key = `${skill.id}:${app.id}`;
                      const readOnlyIssue = appState.issue || skill.issue;
                      const stateLabel =
                        appState.enabled == null
                          ? "state unknown"
                          : enabled
                            ? "enabled"
                            : "disabled";
                      if (readOnlyIssue) {
                        return (
                          <span
                            key={app.id}
                            role="status"
                            tabIndex={0}
                            aria-label={`${definition.label}: ${stateLabel}, read-only. ${readOnlyIssue}`}
                            title={`${stateLabel}; ${readOnlyIssue}`}
                            className={`relative flex h-7 w-7 items-center justify-center rounded-lg text-muted-foreground ${
                              enabled
                                ? "bg-emerald-500/15 opacity-100"
                                : appState.enabled == null
                                  ? "border border-amber-500/40 bg-muted opacity-70"
                                  : "bg-muted opacity-45"
                            }`}
                          >
                            <ProviderIcon
                              icon={definition.icon}
                              name={definition.label}
                              size={17}
                            />
                            <LockKeyhole
                              aria-hidden="true"
                              className="absolute -bottom-1 -right-1 h-3 w-3 rounded-full bg-background p-0.5 text-amber-500"
                            />
                          </span>
                        );
                      }
                      const disabled = pending !== null || stale || loading;
                      return (
                        <button
                          key={app.id}
                          type="button"
                          disabled={disabled}
                          onClick={() => void toggle(skill, app.id, !enabled)}
                          aria-label={`${enabled ? "Disable" : "Enable"} ${skill.name || skill.directory} for ${definition.label}`}
                          aria-pressed={enabled}
                          aria-busy={pending === key}
                          title={definition.label}
                          className={`flex h-7 w-7 items-center justify-center rounded-lg transition-all disabled:cursor-not-allowed ${
                            enabled
                              ? "bg-emerald-500/15 opacity-100"
                              : "opacity-35 hover:opacity-70"
                          }`}
                        >
                          {pending === key ? (
                            <LoaderCircle className="h-4 w-4 animate-spin" />
                          ) : (
                            <ProviderIcon
                              icon={definition.icon}
                              name={definition.label}
                              size={17}
                            />
                          )}
                        </button>
                      );
                    })}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
