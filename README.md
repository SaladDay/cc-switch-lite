# CC Switch Lite

A provider configuration editor for every application supported by CC Switch.

CC Switch Lite uses
[`cc-switch-core`](https://github.com/SaladDay/cc-switch-core) as its built-in
application registry and native Import/Apply/Remove domain layer. Lite retains
host paths and I/O, database persistence, UI presentation, and its own release
cycle.

The project is in pre-alpha development. The first release is intentionally
limited to provider switching, MCP configuration, and Skill configuration.
Proxy routing, managed OAuth, usage tracking, prompts, and common configuration
management remain in the full CC Switch application.
Appearance and application visibility are local Lite preferences and do not
write the shared CC Switch settings file.

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

Provider records live in the same `~/.cc-switch/cc-switch.db` database used by
CC Switch. Lite does not keep a second provider catalog. Updates use SQLite
transactions and reject stale edits made against an older record revision.
Until the full application adopts Core's shared live-file lock protocol, do not
write configuration from CC Switch and Lite at the same time.

Import reads only the API-provider fields that Lite can reproduce. Switching
uses the shared Core executor with logical configuration targets. Lite updates its
managed Claude environment keys or its dedicated Codex provider table and
preserves unrelated live settings. Codex routes carry an installation identity
and content digest, so Lite refuses to replace a route it cannot prove it owns.
Lite retains older managed Codex routes because profiles in other Codex config
layers may still reference them; profile-aware route cleanup is not part of
this bootstrap step.
Claude Code status refers to the user-level default; project, local, or managed
settings can still override it.

Installed Skill rows and per-application selections use the same shared
database. Lite can inspect and switch those selections, but it does not install,
discover, update, or uninstall Skills. Core owns native state projection and
reference recovery; Lite owns the SQLite transaction, shared live-file lock,
and resolved host paths. Reference identity is stored beside each native Skill
root under `.cc-switch-skill-references/<app>` so future CC Switch hosts can use
the same owner records. Existing native links created without Core ownership
remain read-only instead of being adopted or removed implicitly.
