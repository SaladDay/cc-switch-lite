import { useEffect, useState, type FormEvent } from "react";
import {
  Check,
  LoaderCircle,
  Pencil,
  Plus,
  RefreshCw,
  ShieldCheck,
  Trash2,
} from "lucide-react";

import type {
  InstalledPlugin,
  MarketplaceCatalog,
  MarketplacePlugin,
  RegistryDraft,
  RegistrySource,
} from "../lib/plugin-types";
import { errorMessage, pluginsApi } from "../lib/providers";
import { FullScreenPanel } from "./FullScreenPanel";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Textarea } from "./ui/textarea";

interface MarketplaceDialogProps {
  onCancel: () => void;
  onChanged: () => void;
}

const EMPTY_CATALOG: MarketplaceCatalog = { plugins: [], failures: [] };

function registryDraft(registry?: RegistrySource): RegistryDraft {
  return {
    id: registry?.id,
    expectedRevision: registry?.revision,
    label: registry?.label ?? "",
    indexUrl: registry?.indexUrl ?? "",
    enabled: registry?.enabled ?? true,
    trustedPublishers: registry?.trustedPublishers.map((key) => ({
      ...key,
    })) ?? [{ publisherId: "", keyId: "", publicKey: "" }],
  };
}

function pluginKey(plugin: MarketplacePlugin): string {
  return [
    plugin.registryId,
    plugin.registryRevision,
    plugin.manifest.id,
    plugin.manifest.version,
    plugin.manifestSha256,
    plugin.packageSha256,
    plugin.publisherKeySha256,
  ].join(":");
}

function isExactInstall(plugin: MarketplacePlugin): boolean {
  const installed = plugin.installed;
  return Boolean(
    installed &&
    installed.version === plugin.manifest.version &&
    installed.registryId === plugin.registryId &&
    installed.manifestSha256 === plugin.manifestSha256 &&
    installed.packageSha256 === plugin.packageSha256 &&
    installed.publisherKeySha256 === plugin.publisherKeySha256 &&
    installed.publisher.id === plugin.manifest.publisher.id &&
    installed.publisher.keyId === plugin.manifest.publisher.keyId,
  );
}

export function MarketplaceDialog({
  onCancel,
  onChanged,
}: MarketplaceDialogProps) {
  const [tab, setTab] = useState<"catalog" | "sources">("catalog");
  const [registries, setRegistries] = useState<RegistrySource[]>([]);
  const [installed, setInstalled] = useState<InstalledPlugin[]>([]);
  const [catalog, setCatalog] = useState(EMPTY_CATALOG);
  const [editing, setEditing] = useState<RegistryDraft | null>(null);
  const [approved, setApproved] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const blocking = busy !== null && busy !== "refresh";

  const refreshCatalog = async () => {
    setBusy("refresh");
    setError(null);
    setApproved(null);
    try {
      setCatalog(await pluginsApi.refresh());
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(null);
    }
  };

  useEffect(() => {
    let ignore = false;
    const load = async () => {
      try {
        const sources = await pluginsApi.listRegistries();
        const active = await pluginsApi.listInstalled();
        const available = await pluginsApi.refresh();
        if (ignore) return;
        setRegistries(sources);
        setInstalled(active);
        setCatalog(available);
      } catch (caught) {
        if (!ignore) setError(errorMessage(caught));
      } finally {
        if (!ignore) setLoading(false);
      }
    };
    void load();
    return () => {
      ignore = true;
    };
  }, []);

  const saveRegistry = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!editing) return;
    setBusy("registry");
    setError(null);
    try {
      const saved = await pluginsApi.saveRegistry(editing);
      setRegistries((current) => [
        ...current.filter((registry) => registry.id !== saved.id),
        saved,
      ]);
      setEditing(null);
      setCatalog(EMPTY_CATALOG);
      setApproved(null);
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(null);
    }
  };

  const removeRegistry = async (registry: RegistrySource) => {
    setBusy(`registry:${registry.id}`);
    setError(null);
    try {
      await pluginsApi.removeRegistry(registry.id, registry.revision);
      setRegistries((current) =>
        current.filter((candidate) => candidate.id !== registry.id),
      );
      setCatalog(EMPTY_CATALOG);
      setApproved(null);
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(null);
    }
  };

  const install = async (plugin: MarketplacePlugin) => {
    const key = pluginKey(plugin);
    setBusy(key);
    setError(null);
    try {
      const active = await pluginsApi.install(
        {
          registryId: plugin.registryId,
          registryRevision: plugin.registryRevision,
          pluginId: plugin.manifest.id,
          version: plugin.manifest.version,
          manifestSha256: plugin.manifestSha256,
          packageSha256: plugin.packageSha256,
          publisherKeySha256: plugin.publisherKeySha256,
        },
        plugin.manifest.capabilities,
      );
      setInstalled((current) => [
        ...current.filter((item) => item.id !== active.id),
        active,
      ]);
      setCatalog((current) => ({
        ...current,
        plugins: current.plugins.map((item) =>
          item.manifest.id === active.id
            ? { ...item, installed: active }
            : item,
        ),
      }));
      setApproved(null);
      onChanged();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(null);
    }
  };

  const uninstall = async (pluginId: string) => {
    const key = `installed:${pluginId}`;
    setBusy(key);
    setError(null);
    try {
      await pluginsApi.uninstall(pluginId);
      setInstalled((current) => current.filter((item) => item.id !== pluginId));
      setCatalog((current) => ({
        ...current,
        plugins: current.plugins.map((item) =>
          item.manifest.id === pluginId
            ? { ...item, installed: undefined }
            : item,
        ),
      }));
      onChanged();
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(null);
    }
  };

  return (
    <FullScreenPanel
      title="Plugin marketplace"
      titleId="marketplace-dialog-title"
      description="Signed provider adapters from sources you trust."
      closeLabel="Close plugin marketplace"
      busy={blocking}
      onClose={onCancel}
      contentClassName="h-full space-y-0 p-0"
    >
      <div className="flex h-full min-h-0">
        <nav className="w-44 shrink-0 space-y-1 border-r border-border p-3">
          {(["catalog", "sources"] as const).map((item) => (
            <button
              key={item}
              type="button"
              aria-pressed={tab === item}
              onClick={() => setTab(item)}
              className={`h-10 w-full rounded-lg px-3 text-left text-sm font-medium capitalize transition-colors ${
                tab === item
                  ? "bg-muted text-foreground"
                  : "text-muted-foreground hover:bg-muted/60 hover:text-foreground"
              }`}
            >
              {item}
            </button>
          ))}
          <p className="px-3 pt-3 text-xs leading-5 text-muted-foreground">
            {installed.length} installed
          </p>
        </nav>

        <div className="min-w-0 flex-1 overflow-y-auto p-6">
          {error && (
            <p
              role="alert"
              className="mb-5 rounded-xl border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-600 dark:text-red-300"
            >
              {error}
            </p>
          )}

          {loading ? (
            <div
              role="status"
              aria-live="polite"
              className="grid h-full place-items-center"
            >
              <span className="sr-only">Loading plugin marketplace</span>
              <LoaderCircle
                aria-hidden="true"
                className="size-6 animate-spin text-muted-foreground"
              />
            </div>
          ) : tab === "catalog" ? (
            <Catalog
              catalog={catalog}
              installed={installed}
              busy={busy}
              approved={approved}
              onApprove={setApproved}
              onInstall={install}
              onUninstall={uninstall}
              onRefresh={refreshCatalog}
            />
          ) : (
            <Sources
              registries={registries}
              editing={editing}
              busy={busy}
              onEdit={setEditing}
              onSave={saveRegistry}
              onRemove={removeRegistry}
            />
          )}
        </div>
      </div>
    </FullScreenPanel>
  );
}

function Catalog({
  catalog,
  installed,
  busy,
  approved,
  onApprove,
  onInstall,
  onUninstall,
  onRefresh,
}: {
  catalog: MarketplaceCatalog;
  installed: InstalledPlugin[];
  busy: string | null;
  approved: string | null;
  onApprove: (key: string | null) => void;
  onInstall: (plugin: MarketplacePlugin) => void;
  onUninstall: (pluginId: string) => void;
  onRefresh: () => void;
}) {
  return (
    <>
      <div className="mb-5 flex items-center justify-between gap-4">
        <div>
          <h3 className="font-semibold">Available adapters</h3>
          <p className="mt-1 text-xs text-muted-foreground">
            Refresh is manual. Lite never updates plugins in the background.
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          disabled={busy !== null}
          onClick={onRefresh}
        >
          <RefreshCw
            className={`size-4 ${busy === "refresh" ? "animate-spin" : ""}`}
          />
          Refresh
        </Button>
      </div>

      {catalog.failures.map((failure) => (
        <p
          key={failure.registryId}
          className="mb-3 rounded-xl border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-300"
        >
          {failure.registryLabel}: {failure.message}
        </p>
      ))}

      {installed.length > 0 && (
        <section className="mb-6">
          <h3 className="mb-3 text-sm font-semibold">Installed plugins</h3>
          <div className="space-y-2">
            {installed.map((plugin) => (
              <div
                key={plugin.id}
                className="flex items-center justify-between gap-4 rounded-xl border border-border px-3 py-3"
              >
                <div className="min-w-0">
                  <p className="truncate text-sm font-medium">{plugin.id}</p>
                  <p className="mt-0.5 truncate text-xs text-muted-foreground">
                    Version {plugin.version} · Source {plugin.registryId}
                  </p>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy !== null}
                  onClick={() => onUninstall(plugin.id)}
                  className="h-8 shrink-0 text-xs hover:bg-red-500/10 hover:text-red-600"
                >
                  {busy === `installed:${plugin.id}` ? "Removing…" : "Remove"}
                </Button>
              </div>
            ))}
          </div>
        </section>
      )}

      {catalog.plugins.length === 0 ? (
        <div className="rounded-2xl border border-dashed border-border px-6 py-12 text-center">
          <ShieldCheck className="mx-auto size-7 text-muted-foreground" />
          <p className="mt-3 text-sm font-medium">No verified plugins found</p>
          <p className="mt-1 text-xs text-muted-foreground">
            Add an enabled source and its publisher keys, then refresh.
          </p>
        </div>
      ) : (
        <div className="space-y-4">
          {catalog.plugins.map((plugin) => {
            const key = pluginKey(plugin);
            const exactInstall = isExactInstall(plugin);
            const ownershipCollision = Boolean(
              plugin.installed &&
              (plugin.installed.registryId !== plugin.registryId ||
                plugin.installed.publisher.id !==
                  plugin.manifest.publisher.id ||
                plugin.installed.publisher.keyId !==
                  plugin.manifest.publisher.keyId ||
                plugin.installed.publisherKeySha256 !==
                  plugin.publisherKeySha256),
            );
            const sameVersionConflict = Boolean(
              plugin.installed &&
              !ownershipCollision &&
              plugin.installed.version === plugin.manifest.version &&
              !exactInstall,
            );
            const update = Boolean(
              plugin.installed && !exactInstall && !ownershipCollision,
            );
            const needsApproval =
              !exactInstall &&
              !ownershipCollision &&
              !sameVersionConflict &&
              approved !== key;
            return (
              <article
                key={key}
                className="rounded-xl border border-border p-4"
              >
                <div className="flex items-start justify-between gap-5">
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <h4 className="truncate font-semibold">
                        {plugin.manifest.name}
                      </h4>
                      <span className="rounded-full bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">
                        {plugin.manifest.version}
                      </span>
                    </div>
                    <p className="mt-1 text-xs text-muted-foreground">
                      {plugin.manifest.publisher.id} · {plugin.registryLabel}
                    </p>
                    <p className="mt-3 text-sm leading-6 text-muted-foreground">
                      {plugin.manifest.description}
                    </p>
                  </div>
                  {exactInstall ? (
                    <Button
                      variant="outline"
                      size="sm"
                      disabled
                      className="shrink-0 text-muted-foreground opacity-70"
                    >
                      Installed
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      disabled={
                        busy !== null ||
                        needsApproval ||
                        sameVersionConflict ||
                        ownershipCollision
                      }
                      onClick={() => onInstall(plugin)}
                      className="shrink-0"
                    >
                      {busy === key
                        ? "Verifying…"
                        : ownershipCollision
                          ? "ID collision"
                          : sameVersionConflict
                            ? "Version conflict"
                            : update
                              ? "Update"
                              : "Install"}
                    </Button>
                  )}
                </div>
                <div className="mt-4 rounded-xl bg-muted/70 px-3 py-3">
                  <p className="text-xs font-medium">Requested permissions</p>
                  <ul className="mt-2 space-y-1 text-xs text-muted-foreground">
                    {plugin.permissions.map((permission) => (
                      <li key={permission} className="flex gap-2">
                        <Check className="mt-0.5 size-3 shrink-0" />
                        {permission}
                      </li>
                    ))}
                  </ul>
                  {!exactInstall &&
                    !ownershipCollision &&
                    !sameVersionConflict && (
                      <label className="mt-3 flex cursor-pointer items-start gap-2 border-t border-border pt-3 text-xs">
                        <input
                          type="checkbox"
                          checked={approved === key}
                          onChange={(event) =>
                            onApprove(event.target.checked ? key : null)
                          }
                          className="mt-0.5"
                        />
                        Approve exactly these permissions for this signed
                        version
                      </label>
                    )}
                </div>
              </article>
            );
          })}
        </div>
      )}
    </>
  );
}

function Sources({
  registries,
  editing,
  busy,
  onEdit,
  onSave,
  onRemove,
}: {
  registries: RegistrySource[];
  editing: RegistryDraft | null;
  busy: string | null;
  onEdit: (draft: RegistryDraft | null) => void;
  onSave: (event: FormEvent<HTMLFormElement>) => void;
  onRemove: (registry: RegistrySource) => void;
}) {
  if (editing) {
    return (
      <form onSubmit={onSave} className="space-y-5">
        <div>
          <h3 className="font-semibold">
            {editing.id ? "Edit source" : "Add source"}
          </h3>
          <p className="mt-1 text-xs text-muted-foreground">
            Each source has its own independent publisher trust keys.
          </p>
        </div>
        <label className="block text-sm font-medium">
          Name
          <Input
            required
            maxLength={80}
            value={editing.label}
            onChange={(event) =>
              onEdit({ ...editing, label: event.target.value })
            }
            className="mt-2"
          />
        </label>
        <label className="block text-sm font-medium">
          Registry index URL
          <Input
            required
            type="url"
            value={editing.indexUrl}
            onChange={(event) =>
              onEdit({ ...editing, indexUrl: event.target.value })
            }
            placeholder="https://plugins.example.com/index.json"
            className="mt-2"
          />
        </label>
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={editing.enabled}
            onChange={(event) =>
              onEdit({ ...editing, enabled: event.target.checked })
            }
          />
          Include this source when refreshing
        </label>

        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <div>
              <p className="text-sm font-medium">Trusted publisher keys</p>
              <p className="mt-1 text-xs text-muted-foreground">
                A package is accepted only when one of these exact keys verifies
                it.
              </p>
            </div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                onEdit({
                  ...editing,
                  trustedPublishers: [
                    ...editing.trustedPublishers,
                    { publisherId: "", keyId: "", publicKey: "" },
                  ],
                })
              }
              className="h-8 text-xs"
            >
              <Plus className="size-3.5" /> Key
            </Button>
          </div>
          {editing.trustedPublishers.map((key, index) => (
            <div
              key={index}
              className="grid grid-cols-[1fr_1fr_auto] gap-2 rounded-xl border border-border p-3"
            >
              <Input
                required
                aria-label={`Publisher ID ${index + 1}`}
                value={key.publisherId}
                onChange={(event) => {
                  const keys = [...editing.trustedPublishers];
                  keys[index] = { ...key, publisherId: event.target.value };
                  onEdit({ ...editing, trustedPublishers: keys });
                }}
                placeholder="Publisher ID"
                className="px-2 text-xs"
              />
              <Input
                required
                aria-label={`Key ID ${index + 1}`}
                value={key.keyId}
                onChange={(event) => {
                  const keys = [...editing.trustedPublishers];
                  keys[index] = { ...key, keyId: event.target.value };
                  onEdit({ ...editing, trustedPublishers: keys });
                }}
                placeholder="Key ID"
                className="px-2 text-xs"
              />
              <Button
                type="button"
                variant="ghost"
                size="icon"
                disabled={editing.trustedPublishers.length === 1}
                onClick={() =>
                  onEdit({
                    ...editing,
                    trustedPublishers: editing.trustedPublishers.filter(
                      (_, keyIndex) => keyIndex !== index,
                    ),
                  })
                }
                className="text-muted-foreground hover:bg-red-500/10 hover:text-red-600 disabled:opacity-30"
                aria-label={`Remove publisher key ${index + 1}`}
              >
                <Trash2 className="size-4" />
              </Button>
              <Textarea
                required
                rows={2}
                aria-label={`Ed25519 public key ${index + 1}`}
                value={key.publicKey}
                onChange={(event) => {
                  const keys = [...editing.trustedPublishers];
                  keys[index] = { ...key, publicKey: event.target.value };
                  onEdit({ ...editing, trustedPublishers: keys });
                }}
                placeholder="Base64 Ed25519 public key"
                className="col-span-3 resize-none font-mono text-xs"
              />
            </div>
          ))}
        </div>

        <div className="flex justify-end gap-3 border-t border-border pt-5">
          <Button
            type="button"
            variant="outline"
            disabled={busy !== null}
            onClick={() => onEdit(null)}
          >
            Cancel
          </Button>
          <Button type="submit" disabled={busy !== null}>
            {busy === "registry" ? "Saving…" : "Save source"}
          </Button>
        </div>
      </form>
    );
  }

  return (
    <>
      <div className="mb-5 flex items-center justify-between gap-4">
        <div>
          <h3 className="font-semibold">Registry sources</h3>
          <p className="mt-1 text-xs text-muted-foreground">
            Sources can be independently enabled and trust different publishers.
          </p>
        </div>
        <Button
          size="sm"
          disabled={busy !== null}
          onClick={() => onEdit(registryDraft())}
        >
          <Plus className="size-4" /> Add source
        </Button>
      </div>
      <div className="space-y-3">
        {registries.map((registry) => (
          <article
            key={registry.id}
            className="flex items-start justify-between gap-4 rounded-xl border border-border p-4"
          >
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <h4 className="truncate font-medium">{registry.label}</h4>
                <span
                  className={`rounded-full px-2 py-0.5 text-[11px] ${
                    registry.enabled
                      ? "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300"
                      : "bg-muted text-muted-foreground"
                  }`}
                >
                  {registry.enabled ? "Enabled" : "Disabled"}
                </span>
              </div>
              <p className="mt-1 truncate text-xs text-muted-foreground">
                {registry.indexUrl}
              </p>
              <p className="mt-2 text-xs text-muted-foreground">
                {registry.trustedPublishers.length} trusted publisher keys
              </p>
            </div>
            <div className="flex shrink-0 gap-1">
              <Button
                variant="ghost"
                size="icon"
                disabled={busy !== null}
                onClick={() => onEdit(registryDraft(registry))}
                className="h-8 w-8 p-1"
                aria-label={`Edit ${registry.label}`}
              >
                <Pencil className="size-4" />
              </Button>
              <Button
                variant="ghost"
                size="icon"
                disabled={busy !== null}
                onClick={() => onRemove(registry)}
                className="h-8 w-8 p-1 hover:text-red-500 dark:hover:text-red-400"
                aria-label={`Remove ${registry.label}`}
              >
                {busy === `registry:${registry.id}` ? (
                  <LoaderCircle className="size-4 animate-spin" />
                ) : (
                  <Trash2 className="size-4" />
                )}
              </Button>
            </div>
          </article>
        ))}
        {registries.length === 0 && (
          <div className="rounded-lg border border-dashed border-border px-6 py-12 text-center text-sm text-muted-foreground">
            No registry sources configured.
          </div>
        )}
      </div>
    </>
  );
}
