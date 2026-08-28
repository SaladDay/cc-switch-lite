# CC Switch Lite

A provider configuration editor for every application supported by CC Switch.

CC Switch Lite shares low-level contracts with CC Switch through
[`cc-switch-core`](https://github.com/SaladDay/cc-switch-core), while keeping
its UI, state, and release cycle independent.

The project is in pre-alpha development. The first release is intentionally
limited to provider management, safe live-configuration switching, and signed
provider-adapter plugins. Proxy routing, managed OAuth, usage tracking, MCP,
prompts, and skills remain in the full CC Switch application.

## Development

Requirements: Node.js 20.19 or newer in the 20.x line, or Node.js 22.12+;
pnpm 10, Rust 1.88.0, and the platform
dependencies required by Tauri 2.

```sh
pnpm install
pnpm typecheck
pnpm test
pnpm tauri dev
```

The plugin platform direction is documented in
[`docs/architecture/0001-plugin-platform.md`](docs/architecture/0001-plugin-platform.md),
and the implemented contract-major-1 boundary is frozen in
[`docs/architecture/0002-plugin-contract-v1.md`](docs/architecture/0002-plugin-contract-v1.md).

The marketplace supports multiple independently configured registry sources.
Each source has its own Ed25519 publisher allowlist. Installations verify the
signed manifest, complete package, and every declared payload before activation;
permissions must be approved for the exact signed version. Components receive
no WASI, network, filesystem, environment, process, or shell imports. See
[`plugin-api/README.md`](plugin-api/README.md) for the guest and package contract.

Provider records live in the same `~/.cc-switch/cc-switch.db` database used by
CC Switch. Lite does not keep a second provider catalog. Updates use SQLite
transactions and reject stale edits made against an older record revision.

Import reads only the API-provider fields that Lite can reproduce. Switching
uses a versioned, host-validated operation plan with logical configuration
targets—adapters never receive arbitrary file-write access. Lite updates its
managed Claude environment keys or its dedicated Codex provider table and
preserves unrelated live settings. Codex routes carry an installation identity
and content digest, so Lite refuses to replace a route it cannot prove it owns.
Lite retains older managed Codex routes because profiles in other Codex config
layers may still reference them; profile-aware route cleanup is not part of
this bootstrap step.
Claude Code status refers to the user-level default; project, local, or managed
settings can still override it.
