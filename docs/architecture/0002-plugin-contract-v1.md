# ADR 0002: Plugin contract major 1

Status: Accepted for the Lite pre-alpha marketplace.

## Goal and boundary

Contract major 1 makes signed third-party provider adapters usable without
giving a component ambient authority. A plugin can contribute schema-rendered
provider forms for Claude Code or Codex and can read or propose writes only for
the existing logical configuration slots that the user approves at install.

Adding a new client or an arbitrary filesystem slot is not compatible with
this world. That requires a later contract major with a separately reviewed
slot-registration model.

## Component world

The WIT world exports one `invoke` function. Its request and result are bounded
UTF-8 JSON envelopes. This keeps one canonical set of Rust/JSON wire records
for built-ins and components instead of maintaining parallel WIT and serde
models. The envelope carries `contractMajor: 1` and one operation:

- `validate` receives an adapter ID and provider settings;
- `import` receives an adapter ID and approved live snapshots;
- `plan` receives a stored provider and approved live snapshots;
- `current` receives an adapter ID, settings, and approved live snapshots.

Import responses carry the same `ProviderDraft` record consumed by the host.
Plan responses carry only a strict provider route (`apiKey`, optional `baseUrl`,
and optional `model`); the host merges that route into the live configuration
and creates the `OperationPlan`. A component can never submit whole-file
contents or executable client configuration. Unknown fields, operations, and response shapes are rejected.
Plugin-provided error text is not displayed because the component sees secrets.

The component receives no imports. In particular, the host does not link WASI,
filesystem, network, environment, clock, random, process, or shell interfaces.
Every call has component-size, request, response, memory, table, instance, and
fuel limits. Each approved live-configuration snapshot is limited to 256 KiB;
the complete request and response envelopes are each limited to 8 MiB.
The daily `current` path uses a smaller per-call fuel grant plus per-plugin and
per-command call caps; the first failed call fuses that plugin for the command.
The Codex authentication snapshot is never the raw `auth.json`: the host emits
only `OPENAI_API_KEY` for an API-key-only file and withholds OAuth or mixed auth.

## Signed package

A package is a ZIP archive containing exactly `manifest.json`, `manifest.sig`,
and the payload paths listed by the manifest. Directories are implicit; duplicate
entries, symbolic links, encrypted entries, traversal, undeclared payloads, and
oversized archives are rejected.

Manifest schema 1 uses strict JSON with deterministic canonical bytes: parse
into the versioned manifest record, then serialize that record as compact JSON.
Record field order is the Rust schema order and payload hashes use a sorted map.
The Ed25519 signature covers those canonical bytes. `manifest.json` in the
archive must already equal the canonical bytes, preventing multiple encodings
of the same signed object.

Registry index schema 1 embeds the manifest, detached signature, immutable
package URL, package SHA-256, and canonical manifest SHA-256. Each registry
source owns an independent allowlist of `(publisher ID, key ID, Ed25519 public
key)` tuples. A registry cannot replace publisher-signed fields.
Invalid package entries are isolated to their `(plugin ID, version)`; they do
not hide independently valid entries from the same source.

## Installation and state

Downloads have HTTPS/loopback-HTTP, redirect, timeout, and byte limits. Enabled
sources refresh concurrently and fail independently. The host
verifies the package digest, canonical manifest digest, publisher signature,
payload declarations, payload hashes, host API, adapter schemas, and requested
capabilities before activation. Approval and installation bind the source
revision, manifest digest, complete package digest, and publisher-key
fingerprint. Extraction occurs in a private staging directory on the same
filesystem as the installed directory.

Activation renames the completed version into place and then atomically updates
the private plugin lockfile. A prior active version is retained as the single
rollback candidate. A failure before the lockfile update leaves the prior
version active; an unreferenced completed directory is inert and can be reused
only after every signed payload and metadata file is reverified.

On startup and before every locked state operation, the host removes stale
staging directories, restores an uninstall tombstone when installed state still
names that plugin, and removes version directories not named by active/retained state.
Uninstall renames the plugin directory to a deterministic private tombstone
before committing state, so either crash outcome has a single recovery direction.

Contract major 1 does not define provider-data migration. The host therefore
rejects a plugin version update while any stored provider references that plugin;
the user must remove those providers first. A later contract may add an explicit,
transactional migration protocol.

Registry configuration, installed state, and provider state have separate
private files and sidecar locks. Registry refresh never mutates installed state.
Installed state pins the source ID, publisher identity, and SHA-256 fingerprint
of the publisher public key. Contract-major-1 updates must preserve all three;
ownership transfer and key rotation require a future explicit workflow.
Removing a registry never uninstalls its plugins. Automatic background updates,
ratings, publishing, OAuth, proxying, and plugin JavaScript are outside v1.
