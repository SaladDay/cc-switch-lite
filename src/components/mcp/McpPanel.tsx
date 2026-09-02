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
} from "lucide-react";

import { errorMessage } from "../../lib/providers";
import { mcpApi } from "../../lib/mcp";
import type { McpServer } from "../../lib/mcp-types";
import type { CoreAppDescriptor } from "../../lib/provider-types";
import { AppCountBar } from "../common/AppCountBar";
import { AppToggleGroup } from "../common/AppToggleGroup";
import { ListItemRow } from "../common/ListItemRow";
import { ManagementListSearch } from "../common/ManagementListSearch";
import { Button } from "../ui/button";
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
        <AppCountBar
          totalLabel={`${servers.length} MCP server${servers.length === 1 ? "" : "s"} configured`}
          counts={counts}
          apps={apps}
        />

        <ManagementListSearch
          value={search}
          onValueChange={setSearch}
          placeholder="Search MCP name, description, tag, or command..."
          ariaLabel="Search managed MCP servers"
          clearLabel="Clear MCP search"
        />

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

        <div className="-mr-3 min-h-0 flex-1 overflow-y-auto">
          <div className="pb-24 pr-3">
            {loading ? (
              <div className="flex justify-center py-12 text-muted-foreground">
                <LoaderCircle className="h-5 w-5 animate-spin" />
              </div>
            ) : servers.length === 0 ? (
              <div className="py-12 text-center">
                <div className="mx-auto mb-4 flex h-16 w-16 items-center justify-center rounded-full bg-muted">
                  <Server className="h-6 w-6 text-muted-foreground" />
                </div>
                <h2 className="text-lg font-medium">No servers yet</h2>
                <p className="mt-2 text-sm text-muted-foreground">
                  Click the button in the top right to add your first MCP
                  server.
                </p>
              </div>
            ) : filtered.length === 0 ? (
              <div className="flex flex-col items-center justify-center py-12 text-center text-muted-foreground">
                <Search className="mb-4 h-10 w-10 opacity-40" />
                <p className="text-sm">No MCP servers match your search</p>
              </div>
            ) : (
              <div className="overflow-hidden rounded-xl border border-border-default">
                {filtered.map((server, index) => (
                  <ListItemRow
                    key={server.id}
                    isLast={index === filtered.length - 1}
                  >
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-sm font-medium">
                        {server.name || server.id}
                      </p>
                      {server.description ? (
                        <p
                          className="truncate text-xs text-muted-foreground"
                          title={server.description}
                        >
                          {server.description}
                        </p>
                      ) : server.tags?.length ? (
                        <p className="truncate text-xs text-muted-foreground/60">
                          {server.tags.join(", ")}
                        </p>
                      ) : null}
                    </div>
                    <AppToggleGroup
                      apps={apps}
                      stateFor={(appId) => ({
                        enabled: Boolean(server.apps[appId]),
                      })}
                      onToggle={(appId) => void toggle(server, appId)}
                      ariaLabel={(_, state, label) =>
                        `${state.enabled ? "Disable" : "Enable"} ${server.name || server.id} for ${label}`
                      }
                      disabled={blocked}
                    />
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
                  </ListItemRow>
                ))}
              </div>
            )}
          </div>
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
