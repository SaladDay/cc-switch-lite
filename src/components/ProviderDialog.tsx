import { useState, type FormEvent } from "react";
import { X } from "lucide-react";

import type {
  AdapterDescriptor,
  ProviderChanges,
  ProviderRecord,
} from "../lib/provider-types";
import { useModalDialog } from "../lib/use-modal-dialog";

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
  const dialogRef = useModalDialog({ busy, onCancel });
  const [name, setName] = useState(provider?.name ?? "");
  const [settings, setSettings] = useState(() =>
    initialSettings(adapter, provider),
  );
  const title = provider ? "Edit provider" : "Add provider";

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    onSave({
      name,
      settings,
      adapter: provider ? undefined : adapter.reference,
    });
  };

  return (
    <dialog
      ref={dialogRef}
      aria-modal="true"
      aria-labelledby="provider-dialog-title"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onCancel();
      }}
      className="glass-card fixed inset-0 z-50 m-auto max-h-[calc(100vh-3rem)] w-[calc(100%-3rem)] max-w-lg overflow-y-auto rounded-2xl p-0 text-foreground shadow-2xl"
    >
      <div className="flex items-start justify-between border-b border-border px-6 py-5">
        <div>
          <h2 id="provider-dialog-title" className="text-lg font-semibold">
            {title}
          </h2>
          <p className="mt-1 text-sm text-muted-foreground">
            {adapter.displayName}
          </p>
        </div>
        <button
          type="button"
          onClick={onCancel}
          disabled={busy}
          className="inline-flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-50"
          aria-label="Close provider dialog"
        >
          <X className="size-4" />
        </button>
      </div>

      <form onSubmit={submit} className="space-y-5 px-6 py-6">
        {!provider && adapters.length > 1 && (
          <label className="block text-sm font-medium">
            Adapter
            <select
              value={selectedIndex}
              onChange={(event) => {
                const index = Number(event.target.value);
                setSelectedIndex(index);
                setSettings(initialSettings(adapters[index]));
              }}
              className="mt-2 h-10 w-full rounded-xl border border-border bg-background px-3 text-sm outline-none transition focus:border-primary"
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

        <label className="block text-sm font-medium">
          Provider name
          <input
            autoFocus
            required
            maxLength={80}
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="Work"
            className="mt-2 h-10 w-full rounded-xl border border-border bg-background px-3 text-sm outline-none transition focus:border-primary"
          />
        </label>

        {adapter.fields.map((field) => {
          const inputId = `provider-setting-${field.key}`;
          const helpId = `${inputId}-help`;
          return (
            <div key={field.key}>
              <label htmlFor={inputId} className="block text-sm font-medium">
                {field.label}
                {!field.required && (
                  <span className="ml-1 font-normal text-muted-foreground">
                    Optional
                  </span>
                )}
              </label>
              <input
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
                autoComplete={field.kind === "secret" ? "off" : undefined}
                className="mt-2 h-10 w-full rounded-xl border border-border bg-background px-3 text-sm outline-none transition focus:border-primary"
              />
              <p
                id={helpId}
                className="mt-1.5 text-xs font-normal leading-5 text-muted-foreground"
              >
                {field.help}
              </p>
            </div>
          );
        })}

        {error && (
          <p
            role="alert"
            className="rounded-xl border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-600 dark:text-red-300"
          >
            {error}
          </p>
        )}

        <div className="flex justify-end gap-3 border-t border-border pt-5">
          <button
            type="button"
            onClick={onCancel}
            disabled={busy}
            className="h-10 rounded-xl border border-border px-4 text-sm font-medium transition-colors hover:bg-muted disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={busy}
            className="h-10 rounded-xl bg-primary px-4 text-sm font-medium text-primary-foreground shadow-sm transition-opacity disabled:opacity-60"
          >
            {busy ? "Saving…" : "Save provider"}
          </button>
        </div>
      </form>
    </dialog>
  );
}
