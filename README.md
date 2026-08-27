# CC Switch Lite

A focused provider switcher for Claude Code and Codex.

CC Switch Lite shares low-level contracts with CC Switch through
[`cc-switch-core`](https://github.com/SaladDay/cc-switch-core), while keeping
its UI, state, and release cycle independent.

The project is in pre-alpha development. The first release is intentionally
limited to provider management and safe live-configuration switching. Proxy
routing, managed OAuth, usage tracking, MCP, prompts, and skills remain in the
full CC Switch application.

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

The plugin platform design is documented in
[`docs/architecture/0001-plugin-platform.md`](docs/architecture/0001-plugin-platform.md).

Provider credentials are currently stored as a local JSON file in Tauri's app
data directory. Writes are atomic; the file is forced to mode `0600` on Unix
and uses the app-data directory's inherited ACL on Windows. This storage is
separate from Claude Code and Codex live configuration.
