import { useMemo, useState } from "react";
import { LoaderCircle, Plus, Save } from "lucide-react";

import { MCP_PRESETS, type McpPreset } from "../../config/mcp-presets";
import { appDefinition } from "../../lib/apps";
import {
  formToServer,
  specToForm,
  type McpFormValue,
  type McpServer,
} from "../../lib/mcp-types";
import type { AppId, CoreAppDescriptor } from "../../lib/provider-types";
import { cn } from "../../lib/utils";
import { FullScreenPanel } from "../FullScreenPanel";
import { ProviderIcon } from "../ProviderIcon";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Textarea } from "../ui/textarea";

interface McpDialogProps {
  server?: McpServer;
  apps: CoreAppDescriptor[];
  existingIds: string[];
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onSave: (server: McpServer) => void;
}

function defaultApps(apps: CoreAppDescriptor[]): Record<AppId, boolean> {
  const enabledByDefault = new Set(["claude", "codex", "gemini", "grokbuild"]);
  return Object.fromEntries(
    apps.map((app) => [app.id, enabledByDefault.has(app.id)]),
  );
}

function emptyForm(apps: CoreAppDescriptor[]): McpFormValue {
  return {
    id: "",
    name: "",
    transport: "stdio",
    command: "",
    args: "",
    cwd: "",
    env: "",
    url: "",
    headers: "",
    apps: defaultApps(apps),
  };
}

function presetServer(preset: McpPreset, apps: CoreAppDescriptor[]): McpServer {
  return {
    ...preset,
    server: { ...preset.server },
    apps: defaultApps(apps),
    tags: [...preset.tags],
  };
}

export function McpDialog({
  server,
  apps,
  existingIds,
  busy,
  error,
  onCancel,
  onSave,
}: McpDialogProps) {
  const editing = server !== undefined;
  const [base, setBase] = useState<McpServer | undefined>(server);
  const [form, setForm] = useState<McpFormValue>(() =>
    server ? specToForm(server) : emptyForm(apps),
  );
  const [selectedPreset, setSelectedPreset] = useState<string | null>(
    editing ? null : "custom",
  );
  const [validationError, setValidationError] = useState<string | null>(null);
  const duplicate = useMemo(
    () => !editing && existingIds.includes(form.id.trim()),
    [editing, existingIds, form.id],
  );

  const update = <K extends keyof McpFormValue>(
    key: K,
    value: McpFormValue[K],
  ) => setForm((current) => ({ ...current, [key]: value }));

  const selectPreset = (preset: McpPreset) => {
    const next = presetServer(preset, apps);
    setBase(next);
    setForm(specToForm(next));
    setSelectedPreset(preset.id);
    setValidationError(null);
  };

  const selectCustom = () => {
    setBase(undefined);
    setForm(emptyForm(apps));
    setSelectedPreset("custom");
    setValidationError(null);
  };

  const submit = () => {
    const id = form.id.trim();
    if (!id) return setValidationError("Server ID is required.");
    if (duplicate) return setValidationError("That server ID already exists.");
    if (form.transport === "stdio" && !form.command.trim()) {
      return setValidationError("Command is required for a stdio server.");
    }
    if (form.transport !== "stdio" && !form.url.trim()) {
      return setValidationError("URL is required for a remote server.");
    }
    try {
      setValidationError(null);
      onSave(formToServer(form, base));
    } catch (caught) {
      setValidationError(
        caught instanceof Error ? caught.message : "The server is invalid.",
      );
    }
  };

  return (
    <FullScreenPanel
      title={editing ? "Edit MCP" : "Add MCP"}
      titleId="mcp-editor-title"
      closeLabel="Close MCP editor"
      busy={busy}
      onClose={onCancel}
      footer={
        <Button disabled={busy || duplicate} onClick={submit}>
          {busy ? (
            <LoaderCircle className="mr-2 h-4 w-4 animate-spin" />
          ) : editing ? (
            <Save className="mr-2 h-4 w-4" />
          ) : (
            <Plus className="mr-2 h-4 w-4" />
          )}
          {busy ? "Saving…" : editing ? "Save" : "Add"}
        </Button>
      }
    >
      <div className="flex h-full flex-col gap-6">
        <section className="glass flex-shrink-0 space-y-6 rounded-xl border border-white/10 p-6">
          {!editing && (
            <div>
              <h3 className="mb-3 text-sm font-medium">Preset</h3>
              <div
                className="flex flex-wrap gap-2"
                role="group"
                aria-label="MCP preset"
              >
                <button
                  type="button"
                  onClick={selectCustom}
                  aria-pressed={selectedPreset === "custom"}
                  className={cn(
                    "rounded-lg px-4 py-2 text-sm font-medium transition-colors",
                    selectedPreset === "custom"
                      ? "bg-emerald-500 text-white dark:bg-emerald-600"
                      : "bg-accent text-muted-foreground hover:bg-accent/80",
                  )}
                >
                  Custom
                </button>
                {MCP_PRESETS.map((preset) => (
                  <button
                    key={preset.id}
                    type="button"
                    onClick={() => selectPreset(preset)}
                    aria-pressed={selectedPreset === preset.id}
                    title={preset.description}
                    className={cn(
                      "rounded-lg px-4 py-2 text-sm font-medium transition-colors",
                      selectedPreset === preset.id
                        ? "bg-emerald-500 text-white dark:bg-emerald-600"
                        : "bg-accent text-muted-foreground hover:bg-accent/80",
                    )}
                  >
                    {preset.id}
                  </button>
                ))}
              </div>
            </div>
          )}

          <div>
            <label
              htmlFor="mcp-server-id"
              className="mb-2 block text-sm font-medium"
            >
              Server ID <span className="text-red-500">*</span>
            </label>
            <Input
              id="mcp-server-id"
              value={form.id}
              disabled={editing || busy}
              onChange={(event) => update("id", event.target.value)}
              placeholder="context7"
              aria-invalid={duplicate || undefined}
            />
          </div>
          <div>
            <label
              htmlFor="mcp-server-name"
              className="mb-2 block text-sm font-medium"
            >
              Name
            </label>
            <Input
              id="mcp-server-name"
              value={form.name}
              disabled={busy}
              onChange={(event) => update("name", event.target.value)}
              placeholder="Context7"
            />
          </div>

          <div>
            <p className="mb-3 text-sm font-medium">Enabled applications</p>
            <div className="flex flex-wrap gap-4">
              {apps.map((app) => {
                const definition = appDefinition(app.id, [app]);
                return (
                  <label
                    key={app.id}
                    className="flex cursor-pointer select-none items-center gap-2 text-sm"
                  >
                    <input
                      type="checkbox"
                      checked={Boolean(form.apps[app.id])}
                      disabled={busy}
                      onChange={(event) =>
                        update("apps", {
                          ...form.apps,
                          [app.id]: event.target.checked,
                        })
                      }
                      className="h-4 w-4 accent-emerald-500"
                    />
                    <ProviderIcon
                      icon={definition.icon}
                      name={definition.label}
                      size={16}
                    />
                    {definition.label}
                  </label>
                );
              })}
            </div>
          </div>
        </section>

        <section className="glass flex min-h-0 flex-1 flex-col space-y-5 rounded-xl border border-white/10 p-6">
          <div>
            <p className="mb-3 text-sm font-medium">Connection</p>
            <div
              className="inline-flex rounded-lg bg-muted p-1"
              role="group"
              aria-label="MCP transport"
            >
              {(["stdio", "http", "sse"] as const).map((transport) => (
                <button
                  key={transport}
                  type="button"
                  disabled={busy}
                  onClick={() => update("transport", transport)}
                  aria-pressed={form.transport === transport}
                  className={cn(
                    "rounded-md px-4 py-1.5 text-sm font-medium uppercase transition-colors",
                    form.transport === transport
                      ? "bg-background text-foreground shadow-sm"
                      : "text-muted-foreground",
                  )}
                >
                  {transport}
                </button>
              ))}
            </div>
          </div>

          {form.transport === "stdio" ? (
            <>
              <label className="block space-y-2 text-sm font-medium">
                <span>Command *</span>
                <Input
                  value={form.command}
                  disabled={busy}
                  onChange={(event) => update("command", event.target.value)}
                  placeholder="npx"
                />
              </label>
              <div className="grid gap-5 sm:grid-cols-2">
                <label className="space-y-2 text-sm font-medium">
                  <span>Arguments</span>
                  <Textarea
                    value={form.args}
                    disabled={busy}
                    onChange={(event) => update("args", event.target.value)}
                    placeholder={
                      "One argument per line\n-y\n@upstash/context7-mcp"
                    }
                    rows={6}
                  />
                </label>
                <label className="space-y-2 text-sm font-medium">
                  <span>Environment</span>
                  <Textarea
                    value={form.env}
                    disabled={busy}
                    onChange={(event) => update("env", event.target.value)}
                    placeholder={"One KEY=VALUE per line"}
                    rows={6}
                  />
                </label>
              </div>
              <label className="block space-y-2 text-sm font-medium">
                <span>Working directory</span>
                <Input
                  value={form.cwd}
                  disabled={busy}
                  onChange={(event) => update("cwd", event.target.value)}
                  placeholder="Optional"
                />
              </label>
            </>
          ) : (
            <>
              <label className="block space-y-2 text-sm font-medium">
                <span>URL *</span>
                <Input
                  type="url"
                  value={form.url}
                  disabled={busy}
                  onChange={(event) => update("url", event.target.value)}
                  placeholder="https://example.com/mcp"
                />
              </label>
              <label className="block space-y-2 text-sm font-medium">
                <span>Headers</span>
                <Textarea
                  value={form.headers}
                  disabled={busy}
                  onChange={(event) => update("headers", event.target.value)}
                  placeholder={"One KEY=VALUE per line"}
                  rows={6}
                />
              </label>
            </>
          )}
        </section>

        {(validationError || error) && (
          <p role="alert" className="text-sm text-red-600 dark:text-red-300">
            {validationError || error}
          </p>
        )}
      </div>
    </FullScreenPanel>
  );
}
