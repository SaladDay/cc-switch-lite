# CC Switch Lite

A lightweight provider, MCP, and Skill configuration switcher for every
application supported by CC Switch.

CC Switch Lite uses
[`cc-switch-core`](https://github.com/SaladDay/cc-switch-core) as its built-in
application registry and native Import/Apply/Remove domain layer. Lite retains
host paths and I/O, database persistence, UI presentation, and its own release
cycle.

The project is in pre-alpha development. The first release is intentionally
limited to provider switching, MCP configuration, and Skill configuration.
Proxy routing, managed OAuth, usage tracking, prompts, and common configuration
management remain in the full CC Switch application.

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
CC Switch. MCP servers and installed Skills use their existing shared tables as
well; Lite does not keep parallel catalogs. If the full application has a custom
configuration directory, Lite follows its `app_paths.json` setting. Skill switch
recovery state stays in a per-database sidecar under
`~/.cc-switch/cc-switch-lite-state/` and is never part of the shared catalog or
its backups. Updates use SQLite transactions and reject stale provider or MCP
edits made against an older record revision.
Until the full application adopts Core's shared live-file lock protocol, do not
write configuration from CC Switch and Lite at the same time.
Until it also adopts Core's application-home resolution, users of
`GEMINI_CLI_HOME` or `GROK_HOME` should use Lite as the sole configuration
writer, or set the matching explicit directory in CC Switch.

The Skills page only switches already-installed Skills between supported
applications. Installation, discovery, updates, backups, and a marketplace are
outside Lite. A switch may remove only a link, a Core-marked verified copy, or
an exact legacy copy backed by the shared catalog's prior selection. An
unrelated same-name directory is left intact and reported as a conflict.
For applications that discover `~/.agents/skills` directly, Lite uses a native
per-Skill control only when Core declares a safe one. Gemini, Grok, and Hermes
use their native disabled lists. Other directly discovered copies are shown as
externally managed instead of guessing whether the application has enabled them.
Skills required by a native application remain enabled and read-only.
Skill status is the user-level default managed by Lite; project, workspace,
system, administrator, or plugin layers can still override it at runtime.
Pending switches are bound to the resolved paths and sync method that started
them. If those settings change before recovery, Lite leaves the switch read-only
instead of replaying it against a different directory. Native sections whose
comments, anchors, or aliases cannot be preserved are also read-only.

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
