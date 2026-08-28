import { useState, type FormEvent } from "react";
import { LoaderCircle, Plus, Save } from "lucide-react";

import type {
  AdapterDescriptor,
  ProviderChanges,
  ProviderRecord,
} from "../lib/provider-types";
import { isNativeAdapter } from "../lib/provider-types";
import { FullScreenPanel } from "./FullScreenPanel";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Textarea } from "./ui/textarea";

interface ProviderDialogProps {
  adapters: AdapterDescriptor[];
  provider?: ProviderRecord;
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onSave: (provider: ProviderChanges) => void;
}

function initialSettings(
  adapter: AdapterDescriptor,
  provider?: ProviderRecord,
): Record<string, string> {
  return Object.fromEntries(
    adapter.fields.map((field) => {
      const value = provider?.settings[field.key];
      return [field.key, typeof value === "string" ? value : ""];
    }),
  );
}

export function ProviderDialog({
  adapters,
  provider,
  busy,
  error,
  onCancel,
  onSave,
}: ProviderDialogProps) {
  const [selectedIndex, setSelectedIndex] = useState(0);
  const adapter = adapters[selectedIndex];
  const [name, setName] = useState(provider?.name ?? "");
  const [settings, setSettings] = useState(() =>
    initialSettings(adapter, provider),
  );
  const [settingsJson, setSettingsJson] = useState(() =>
    JSON.stringify(provider?.settings ?? {}, null, 2),
  );
  const [jsonError, setJsonError] = useState<string | null>(null);
  const native = isNativeAdapter(adapter.reference);
  const title = provider ? "Edit provider" : "Add provider";

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (native) {
      try {
        const parsed: unknown = JSON.parse(settingsJson);
        if (
          typeof parsed !== "object" ||
          parsed === null ||
          Array.isArray(parsed)
        ) {
          setJsonError("Provider configuration must be a JSON object.");
          return;
        }
        setJsonError(null);
        onSave({
          name,
          settings: parsed as ProviderChanges["settings"],
          adapter: provider ? undefined : adapter.reference,
        });
      } catch {
        setJsonError("Provider configuration must contain valid JSON.");
      }
      return;
    }
    onSave({
      name,
      settings,
      adapter: provider ? undefined : adapter.reference,
    });
  };

  return (
    <FullScreenPanel
      title={title}
      titleId="provider-dialog-title"
      description={
        native
          ? "Edit the provider's native CC Switch configuration."
          : `${adapter.displayName}. Credentials remain in CC Switch Lite until this provider is activated.`
      }
      closeLabel="Close provider dialog"
      busy={busy}
      onClose={onCancel}
      contentClassName="pt-3"
      footer={
        <>
          <Button variant="outline" onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
          <Button
            type="submit"
            form="provider-form"
            disabled={busy}
            className="bg-primary text-primary-foreground hover:bg-primary/90"
          >
            {busy ? (
              <LoaderCircle className="mr-2 h-4 w-4 animate-spin" />
            ) : provider ? (
              <Save className="mr-2 h-4 w-4" />
            ) : (
              <Plus className="mr-2 h-4 w-4" />
            )}
            {busy ? "Saving…" : "Save provider"}
          </Button>
        </>
      }
    >
      <form
        id="provider-form"
        onSubmit={submit}
        className="glass space-y-6 rounded-xl border border-white/10 p-6"
      >
        {!provider && adapters.length > 1 && (
          <label className="block space-y-2 text-sm font-medium">
            <span>Adapter</span>
            <select
              value={selectedIndex}
              onChange={(event) => {
                const index = Number(event.target.value);
                setSelectedIndex(index);
                setSettings(initialSettings(adapters[index]));
                setSettingsJson("{}");
                setJsonError(null);
              }}
              className="flex h-9 w-full rounded-md border border-border-default bg-background px-3 py-1 text-sm text-foreground shadow-sm outline-none transition-colors focus:ring-2 focus:ring-blue-500/20 dark:focus:ring-blue-400/20"
            >
              {adapters.map((candidate, index) => (
                <option
                  key={`${candidate.reference.pluginId}:${candidate.reference.pluginVersion}:${candidate.reference.adapterId}`}
                  value={index}
                >
                  {candidate.displayName}
                </option>
              ))}
            </select>
          </label>
        )}

        <label className="block space-y-2 text-sm font-medium">
          <span>Provider name</span>
          <Input
            autoFocus
            required
            maxLength={80}
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="Work"
          />
        </label>

        {native && (
          <div className="space-y-2">
            <label
              htmlFor="provider-native-settings"
              className="block text-sm font-medium"
            >
              Configuration JSON
            </label>
            <Textarea
              id="provider-native-settings"
              required
              spellCheck={false}
              value={settingsJson}
              onChange={(event) => {
                setSettingsJson(event.target.value);
                setJsonError(null);
              }}
              className="min-h-64 font-mono text-xs"
            />
            <p className="text-xs font-normal leading-5 text-muted-foreground">
              This object is stored as the provider's settings_config value.
            </p>
          </div>
        )}

        {!native &&
          adapter.fields.map((field) => {
            const inputId = `provider-setting-${field.key}`;
            const helpId = `${inputId}-help`;
            return (
              <div key={field.key} className="space-y-2">
                <label htmlFor={inputId} className="block text-sm font-medium">
                  {field.label}
                  {!field.required && (
                    <span className="ml-1 font-normal text-muted-foreground">
                      Optional
                    </span>
                  )}
                </label>
                <Input
                  id={inputId}
                  aria-describedby={helpId}
                  required={field.required}
                  type={
                    field.kind === "secret"
                      ? "password"
                      : field.kind === "url"
                        ? "url"
                        : "text"
                  }
                  value={settings[field.key] ?? ""}
                  onChange={(event) =>
                    setSettings((current) => ({
                      ...current,
                      [field.key]: event.target.value,
                    }))
                  }
                  placeholder={field.placeholder}
                />
                <p
                  id={helpId}
                  className="text-xs font-normal leading-5 text-muted-foreground"
                >
                  {field.help}
                </p>
              </div>
            );
          })}

        {(jsonError || error) && (
          <p
            role="alert"
            className="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-600 dark:text-red-300"
          >
            {jsonError || error}
          </p>
        )}
      </form>
    </FullScreenPanel>
  );
}
