import { useState, type FormEvent } from "react";
import { LoaderCircle, Plus, Save } from "lucide-react";

import type {
  ProviderChanges,
  ProviderRecord,
  SimpleProviderField,
  SimpleProviderFormDescriptor,
  SimpleProviderValues,
} from "../lib/provider-types";
import { FullScreenPanel } from "./FullScreenPanel";
import { SimpleProviderPresetSelector } from "./providers/SimpleProviderPresetSelector";
import { Button } from "./ui/button";
import { Input } from "./ui/input";

interface ProviderDialogProps {
  form: SimpleProviderFormDescriptor;
  provider?: ProviderRecord;
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onSave: (provider: ProviderChanges) => void;
}

const EMPTY_VALUES: SimpleProviderValues = {
  baseUrl: "",
  apiKey: "",
  model: "",
};

const FIELD_COPY: Record<
  SimpleProviderField,
  { label: string; placeholder: string; help: string; type: string }
> = {
  baseUrl: {
    label: "Base URL",
    placeholder: "https://api.example.com/v1",
    help: "The provider endpoint used by this application.",
    type: "url",
  },
  apiKey: {
    label: "API key",
    placeholder: "sk-…",
    help: "Stored in the shared CC Switch database and written only when activated.",
    type: "password",
  },
  model: {
    label: "Model",
    placeholder: "Model ID",
    help: "The model ID sent to the provider.",
    type: "text",
  },
};

function initialValues(provider?: ProviderRecord): SimpleProviderValues {
  return provider?.simpleValues
    ? { ...provider.simpleValues }
    : { ...EMPTY_VALUES };
}

export function ProviderDialog({
  form,
  provider,
  busy,
  error,
  onCancel,
  onSave,
}: ProviderDialogProps) {
  const [name, setName] = useState(provider?.name ?? "");
  const [values, setValues] = useState(() => initialValues(provider));
  const [selectedPresetId, setSelectedPresetId] = useState("custom");
  const title = provider ? "Edit Provider" : "Add New Provider";

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    onSave({
      name,
      values,
      ...(!provider && selectedPresetId !== "custom"
        ? { presetId: selectedPresetId }
        : {}),
    });
  };

  return (
    <FullScreenPanel
      title={title}
      titleId="provider-dialog-title"
      closeLabel="Close provider dialog"
      busy={busy}
      onClose={onCancel}
      contentClassName={provider ? undefined : "pt-3"}
      footer={
        <>
          {!provider && (
            <>
              <span className="mr-auto min-w-0 truncate text-xs text-muted-foreground">
                💡 After choosing a preset, fill in the fields below (e.g. API
                Key)
              </span>
              <Button
                variant="outline"
                onClick={onCancel}
                disabled={busy}
                className="border-border/20 hover:bg-accent hover:text-accent-foreground"
              >
                Cancel
              </Button>
            </>
          )}
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
            {busy ? "Saving…" : provider ? "Save" : "Add"}
          </Button>
        </>
      }
    >
      <form
        id="provider-form"
        onSubmit={submit}
        className="glass space-y-6 rounded-xl border border-white/10 p-6"
      >
        {!provider && (
          <SimpleProviderPresetSelector
            presets={form.presets}
            selectedId={selectedPresetId}
            onSelect={(preset) => {
              setSelectedPresetId(preset?.id ?? "custom");
              if (!preset) return;
              setName(preset.name);
              setValues((current) => ({
                baseUrl: preset.baseUrl,
                apiKey: current.apiKey,
                model: preset.model,
              }));
            }}
          />
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

        {form.fields.map((field) => {
          const copy = FIELD_COPY[field.key];
          const inputId = `provider-setting-${field.key}`;
          const helpId = `${inputId}-help`;
          const keepsExistingEnvironmentCredential =
            provider !== undefined &&
            form.appId === "grokbuild" &&
            field.key === "apiKey" &&
            provider.simpleValues?.apiKey.trim() === "";
          const required =
            field.required && !keepsExistingEnvironmentCredential;
          return (
            <div key={field.key} className="space-y-2">
              <label htmlFor={inputId} className="block text-sm font-medium">
                {copy.label}
                {!required && (
                  <span className="ml-1 font-normal text-muted-foreground">
                    Optional
                  </span>
                )}
              </label>
              <Input
                id={inputId}
                aria-describedby={helpId}
                required={required}
                type={copy.type}
                value={values[field.key]}
                onChange={(event) =>
                  setValues((current) => ({
                    ...current,
                    [field.key]: event.target.value,
                  }))
                }
                placeholder={copy.placeholder}
              />
              <p
                id={helpId}
                className="text-xs font-normal leading-5 text-muted-foreground"
              >
                {keepsExistingEnvironmentCredential
                  ? "Leave blank to keep the existing environment credential."
                  : copy.help}
              </p>
            </div>
          );
        })}

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
