import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  Check,
  Edit3,
  LoaderCircle,
  Search,
  Server,
  Trash2,
  X,
} from "lucide-react";

import { appDefinition } from "../../lib/apps";
import { errorMessage } from "../../lib/providers";
import { mcpApi } from "../../lib/mcp";
import type { McpServer } from "../../lib/mcp-types";
import type { CoreAppDescriptor } from "../../lib/provider-types";
import { ProviderIcon } from "../ProviderIcon";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { DeleteMcpDialog } from "./DeleteMcpDialog";
import { McpDialog } from "./McpDialog";

export interface McpPanelHandle {
  openAdd: () => void;
  importExisting: () => void;
}

interface McpPanelProps {
  apps: CoreAppDescriptor[];
  onInteractionBlockedChange?: (blocked: boolean) => void;
}

function searchText(server: McpServer): string {
  const spec = server.server;
  return [
    server.id,
    server.name,
    server.description,
    ...(server.tags ?? []),
    typeof spec.type === "string" ? spec.type : "",
    typeof spec.command === "string" ? spec.command : "",
    ...(Array.isArray(spec.args) ? spec.args : []),
    typeof spec.cwd === "string" ? spec.cwd : "",
    typeof spec.url === "string" ? spec.url : "",
    server.homepage,
    server.docs,
  ]
    .filter((value): value is string => typeof value === "string")
    .join("\n")
    .toLowerCase();
}

export const McpPanel = forwardRef<McpPanelHandle, McpPanelProps>(
  function McpPanel({ apps, onInteractionBlockedChange }, ref) {
    const [servers, setServers] = useState<McpServer[]>([]);
    const [loading, setLoading] = useState(true);
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [notice, setNotice] = useState<string | null>(null);
    const [search, setSearch] = useState("");
    const [editing, setEditing] = useState<McpServer | "new" | null>(null);
    const [deleting, setDeleting] = useState<McpServer | null>(null);
    const refreshGeneration = useRef(0);

    const blocked = loading || busy || editing !== null || deleting !== null;
    useEffect(() => {
      onInteractionBlockedChange?.(blocked);
      return () => onInteractionBlockedChange?.(false);
    }, [blocked, onInteractionBlockedChange]);

    const refreshServers = async () => {
      const generation = ++refreshGeneration.current;
      try {
        const next = await mcpApi.list();
        if (generation === refreshGeneration.current) setServers(next);
      } catch (caught) {
        if (generation === refreshGeneration.current) throw caught;
      }
    };

    useEffect(() => {
      let mounted = true;
      setLoading(true);
      setError(null);
      void refreshServers()
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

    const importExisting = async () => {
      if (blocked) return;
      setBusy(true);
      setError(null);
      setNotice(null);
      try {
        const report = await mcpApi.importExisting();
        await refreshServers();
        const changed =
          report.newServers + report.enabledApps + report.disabledApps;
        setNotice(
          changed === 0
            ? "No new MCP servers were found."
            : `Imported ${report.newServers} server${report.newServers === 1 ? "" : "s"}; enabled ${report.enabledApps} and disabled ${report.disabledApps} existing application link${report.enabledApps + report.disabledApps === 1 ? "" : "s"}.`,
        );
        if (report.failedApps.length > 0) {
          setError(
            `Some applications could not be imported: ${report.failedApps.join("; ")}`,
          );
        }
      } catch (caught) {
        setError(errorMessage(caught));
      } finally {
        setBusy(false);
      }
    };

    useImperativeHandle(ref, () => ({
      openAdd: () => {
        if (!blocked) {
          setError(null);
          setEditing("new");
        }
      },
      importExisting: () => void importExisting(),
    }));

    const save = async (server: McpServer) => {
      setBusy(true);
      setError(null);
      setNotice(null);
      try {
        await mcpApi.upsert(server);
        await refreshServers();
        setEditing(null);
        setNotice(
          editing === "new" ? "MCP server added." : "MCP server updated.",
        );
      } catch (caught) {
        setError(errorMessage(caught));
      } finally {
        setBusy(false);
      }
    };

    const toggle = async (server: McpServer, appId: string) => {
      if (blocked) return;
      setBusy(true);
      setError(null);
      setNotice(null);
      const enabled = !server.apps[appId];
      try {
        await mcpApi.toggle(server.id, appId, enabled, server.revision ?? 0);
        await refreshServers();
      } catch (caught) {
        setError(errorMessage(caught));
      } finally {
        setBusy(false);
      }
    };

    const confirmDelete = async () => {
      if (!deleting) return;
      setBusy(true);
      setError(null);
      setNotice(null);
      try {
        await mcpApi.delete(deleting.id, deleting.revision ?? 0);
        setServers((current) =>
          current.filter((item) => item.id !== deleting.id),
        );
        setDeleting(null);
        setNotice("MCP server deleted.");
      } catch (caught) {
        setError(errorMessage(caught));
      } finally {
        setBusy(false);
      }
    };

    const normalizedSearch = search.trim().toLowerCase();
    const filtered = useMemo(
      () =>
        normalizedSearch
          ? servers.filter((server) =>
              searchText(server).includes(normalizedSearch),
            )
          : servers,
      [normalizedSearch, servers],
    );
    const counts = useMemo(
      () =>
        Object.fromEntries(
          apps.map((app) => [
            app.id,
            servers.filter((server) => server.apps[app.id]).length,
          ]),
        ),
      [apps, servers],
    );

    return (
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden px-6">
        <div className="mb-4 flex flex-shrink-0 items-center gap-4 rounded-xl border border-white/10 px-6 py-4 glass">
          <span className="h-7 shrink-0 rounded-full border border-border-default bg-background/50 px-3 py-1 text-xs font-medium">
            {servers.length} server{servers.length === 1 ? "" : "s"}
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
            placeholder="Search MCP servers…"
            aria-label="Search MCP servers"
            className="pl-9 pr-9"
          />
          {search && (
            <button
              type="button"
              onClick={() => setSearch("")}
              aria-label="Clear MCP search"
              className="absolute right-2 top-1/2 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-md text-muted-foreground hover:bg-muted"
            >
              <X className="h-4 w-4" />
            </button>
          )}
        </div>

        {(error || notice) && (
          <div className="mb-4 space-y-2">
            {error && (
              <p
                role="alert"
                className="rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-sm text-red-600 dark:text-red-300"
              >
                {error}
              </p>
            )}
            {notice && (
              <p
                role="status"
                className="flex items-center gap-2 rounded-xl border border-emerald-500/30 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-700 dark:text-emerald-300"
              >
                <Check className="h-4 w-4" /> {notice}
              </p>
            )}
          </div>
        )}

        <div className="min-h-0 flex-1 overflow-y-auto pb-20">
          {loading ? (
            <div className="flex justify-center py-12 text-muted-foreground">
              <LoaderCircle className="h-5 w-5 animate-spin" />
            </div>
          ) : servers.length === 0 ? (
            <div className="py-12 text-center">
              <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-muted">
                <Server className="h-6 w-6 text-muted-foreground" />
              </div>
              <h2 className="text-lg font-medium">No MCP servers</h2>
              <p className="mt-2 text-sm text-muted-foreground">
                Add a server or import existing application configuration.
              </p>
            </div>
          ) : filtered.length === 0 ? (
            <div className="py-12 text-center text-sm text-muted-foreground">
              No matching MCP servers.
            </div>
          ) : (
            <div className="overflow-hidden rounded-xl border border-border-default">
              {filtered.map((server, index) => (
                <div
                  key={server.id}
                  className={`group flex items-center gap-3 px-4 py-2.5 transition-colors hover:bg-muted/50 ${
                    index < filtered.length - 1
                      ? "border-b border-border-default"
                      : ""
                  }`}
                >
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium">
                      {server.name || server.id}
                    </p>
                    <p className="truncate text-xs text-muted-foreground">
                      {server.description ||
                        server.tags?.join(", ") ||
                        server.id}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    {apps.map((app) => {
                      const definition = appDefinition(app.id, [app]);
                      const enabled = Boolean(server.apps[app.id]);
                      return (
                        <button
                          key={app.id}
                          type="button"
                          disabled={blocked}
                          onClick={() => void toggle(server, app.id)}
                          aria-label={`${enabled ? "Disable" : "Enable"} ${server.name || server.id} for ${definition.label}`}
                          aria-pressed={enabled}
                          title={definition.label}
                          className={`flex h-7 w-7 items-center justify-center rounded-lg transition-all disabled:cursor-not-allowed ${
                            enabled
                              ? "bg-emerald-500/15 opacity-100"
                              : "opacity-35 hover:opacity-70"
                          }`}
                        >
                          <ProviderIcon
                            icon={definition.icon}
                            name={definition.label}
                            size={17}
                          />
                        </button>
                      );
                    })}
                  </div>
                  <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100">
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7"
                      disabled={blocked}
                      onClick={() => {
                        setError(null);
                        setEditing(server);
                      }}
                      aria-label={`Edit ${server.name || server.id}`}
                    >
                      <Edit3 className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7 hover:bg-red-100 hover:text-red-500 dark:hover:bg-red-500/10"
                      disabled={blocked}
                      onClick={() => {
                        setError(null);
                        setDeleting(server);
                      }}
                      aria-label={`Delete ${server.name || server.id}`}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>

        {editing && (
          <McpDialog
            server={editing === "new" ? undefined : editing}
            apps={apps}
            existingIds={servers.map((server) => server.id)}
            busy={busy}
            error={error}
            onCancel={() => {
              if (!busy) setEditing(null);
            }}
            onSave={(server) => void save(server)}
          />
        )}
        {deleting && (
          <DeleteMcpDialog
            server={deleting}
            busy={busy}
            error={error}
            onCancel={() => {
              if (!busy) setDeleting(null);
            }}
            onConfirm={() => void confirmDelete()}
          />
        )}
      </div>
    );
  },
);
