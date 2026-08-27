# ADR 0001: Plugin platform boundary

Status: Accepted for implementation in the Lite pre-alpha series.

## Context

CC Switch Lite starts with built-in Claude Code and Codex support, but it must
be possible to add clients, provider forms, validation, and configuration
generation without rebuilding the application. Loading native libraries or
arbitrary frontend JavaScript would make installation unsafe and tie the host
to platform-specific ABIs.

The plugin boundary also must not create a second switching engine. Built-in
adapters and third-party adapters should produce the same data for the same
host executor.

## Decision

Plugins will be WebAssembly Components with a versioned WIT contract. A plugin
package contains a manifest, one component, locale files, and static assets.
The host never loads native dynamic libraries and does not evaluate plugin
JavaScript.

The implementation is split into four layers:

```text
React host UI
  -> schema renderer and marketplace client
  -> versioned plugin API and built-in adapters
  -> capability broker and operation-plan executor
  -> cc-switch-core file primitives
```

Built-in adapters use the plugin API wire types directly. The Wasm runtime is
an alternate producer of the same types, rather than a separate adapter path.

## Package manifest

The precise schema will be frozen when the first consumer is implemented. The
manifest must carry at least:

- a globally unique plugin ID and semantic version;
- the compatible host API range;
- entry points and contributed application/provider identifiers;
- requested capabilities and their constraints;
- hashes for the component and every asset;
- publisher identity and package signature metadata.

Plugin IDs and contributed identifiers are stable storage keys. Display names
belong in locale resources and may change without migrating user data.

## Capabilities

A component starts without filesystem, network, environment-variable, process,
or shell access. The host can grant narrow capabilities declared by the
manifest:

- read a named live-configuration slot;
- propose writes to a named live-configuration slot;
- make HTTP requests to an allowlist of hosts;
- read a named environment value;
- open an external URL after a user action.

Capabilities are granted per plugin and can be revoked. Shell execution and
unrestricted paths are not part of the first API version.

## Operation plans

Adapters do not receive filesystem paths and do not write files. They accept
plain input snapshots and return an `OperationPlan` containing logical target
slots, expected prior digests, proposed bytes, sensitivity, and deletion
intent.

The host resolves logical slots to platform paths, validates size and digest
limits, takes a rollback snapshot, and executes the plan. It records the new
current provider only after every write succeeds. A failure restores all
targets touched by the plan.

Import uses the reverse flow: the host reads allowed slots, then passes their
contents to the adapter for parsing into provider data.

## User interface contributions

Plugins provide a data schema, a UI schema, locale resources, and static icons.
The host renderer owns all controls, focus behavior, secret masking, error
presentation, and theme tokens. Schemas may express sections, conditional
fields, repeated groups, validation rules, secret fields, and read-only config
previews.

Arbitrary plugin UI is intentionally excluded from the first API. If a future
use case cannot be represented by the schema renderer, it will require a new
explicit UI capability and an isolated message protocol. Existing schema-based
plugins will remain valid.

## Marketplace and installation

The marketplace client accepts multiple registry sources. A registry contains
metadata and immutable package references; it does not execute code. Installing
or updating a plugin follows one transaction:

1. download to a staging directory with size and time limits;
2. verify package and file hashes;
3. verify the publisher signature and registry trust policy;
4. validate the manifest, host API range, and requested capabilities;
5. unpack with traversal and symlink protections;
6. ask the user to approve new capabilities;
7. atomically activate the new version and retain one rollback version.

The installed-plugin lockfile records the exact version, registry, package
digest, publisher, granted capabilities, and activation state. Removing one
plugin cannot remove another plugin's data.

## Compatibility policy

The WIT package carries a semantic version. Additive fields and functions use a
minor version; removed or reinterpreted behavior requires a new major world.
The host may support multiple major worlds during a migration window. Stored
provider data includes its plugin ID, plugin version, schema version, and
adapter identifier so migrations are explicit.

The Rust representation of wire types may change internally. Serialized plugin
data and the WIT contract are the compatibility boundaries.

## Out of scope for the bootstrap milestone

This ADR does not add a Wasm runtime, plugin SDK, registry client, installer,
provider persistence, or live configuration writes. Those pieces are added only
when their contracts have real callers and dedicated tests.
