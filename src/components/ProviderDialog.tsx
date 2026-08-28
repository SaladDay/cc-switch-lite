import { useState, type FormEvent } from "react";
import { LoaderCircle, Plus, Save } from "lucide-react";

import type {
  AdapterDescriptor,
  JsonValue,
  ProviderChanges,
  ProviderRecord,
} from "../lib/provider-types";
import { isNativeAdapter } from "../lib/provider-types";
import { nativeSettingsTemplate } from "../lib/apps";
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
): ProviderChanges["settings"] {
  const settings = { ...(provider?.settings ?? {}) };
  for (const field of adapter.fields) {
    if (typeof settings[field.key] !== "string") settings[field.key] = "";
  }
  return settings;
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
  const [nativeSettings, setNativeSettings] = useState(() =>
    JSON.stringify(
      provider?.settings ?? nativeSettingsTemplate(adapter.appId),
      null,
      2,
    ),
  );
  const [nativeSettingsError, setNativeSettingsError] = useState<string | null>(
    null,
  );
  const title = provider ? "Edit provider" : "Add provider";
  const native = isNativeAdapter(adapter.reference);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    let nextSettings = settings;
    if (native) {
      try {
        const parsed: JsonValue = JSON.parse(nativeSettings);
        if (
          typeof parsed !== "object" ||
          parsed === null ||
          Array.isArray(parsed)
        ) {
          setNativeSettingsError("Configuration must be a JSON object.");
          return;
        }
        nextSettings = parsed;
        setNativeSettingsError(null);
      } catch {
        setNativeSettingsError("Configuration must be valid JSON.");
        return;
      }
    }
    onSave({
      name,
      settings: nextSettings,
      adapter: provider ? undefined : adapter.reference,
    });
  };

  return (
    <FullScreenPanel
      title={title}
      titleId="provider-dialog-title"
      description={
        native
          ? "Edit the same provider configuration object used by full CC Switch."
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
                setNativeSettings(
                  JSON.stringify(
                    nativeSettingsTemplate(adapters[index].appId),
                    null,
                    2,
                  ),
                );
                setNativeSettingsError(null);
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

        {adapter.fields.map((field) => {
          const inputId = `provider-setting-${field.key}`;
          const helpId = `${inputId}-help`;
          const fieldValue = settings[field.key];
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
                value={typeof fieldValue === "string" ? fieldValue : ""}
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

        {native && (
          <div className="space-y-2">
            <label
              htmlFor="native-settings"
              className="block text-sm font-medium"
            >
              Configuration JSON
            </label>
            <Textarea
              id="native-settings"
              required
              value={nativeSettings}
              onChange={(event) => {
                setNativeSettings(event.target.value);
                setNativeSettingsError(null);
              }}
              className="min-h-64 font-mono text-xs leading-5"
              aria-describedby="native-settings-help"
              aria-invalid={nativeSettingsError !== null}
            />
            <p
              id="native-settings-help"
              className="text-xs font-normal leading-5 text-muted-foreground"
            >
              This is the provider fragment written by the selected application.
              Unknown keys are preserved.
            </p>
            {nativeSettingsError && (
              <p
                role="alert"
                className="text-sm text-red-600 dark:text-red-300"
              >
                {nativeSettingsError}
              </p>
            )}
          </div>
        )}

        {error && (
          <p
            role="alert"
            className="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-600 dark:text-red-300"
          >
            {error}
          </p>
        )}
      </form>
    </FullScreenPanel>
  );
}
