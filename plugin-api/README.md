# Plugin API contract major 1

This directory is the publisher-facing boundary for Lite provider adapters.
The authoritative WIT world is [`wit/plugin.wit`](wit/plugin.wit). A component
exports one `invoke(string) -> result<string, string>` function and exchanges
the strict JSON envelopes defined by the host's contract-major-1 Rust types.

Contract major 1 can contribute schema-rendered provider forms for `claude` or
`codex`. It cannot add applications, paths, arbitrary UI, or host imports. The
host links no WASI interfaces, so guests must compile without ambient WASI
imports. The small Rust guest in [`examples/fixture`](examples/fixture) is a
buildable reference and is exercised by the host runtime tests.

For switching, a guest returns only a provider route containing an API key and
optional base URL/model. The host validates those values, merges only its
provider-owned keys, and builds the transactional operation plan. Guests cannot
return whole client configuration files.

The corresponding response envelope is:

```json
{
  "operation": "routed",
  "payload": {
    "route": { "apiKey": "...", "baseUrl": null, "model": null }
  }
}
```

Each approved live-configuration snapshot is capped at 256 KiB. The complete
request and response JSON envelopes are capped at 8 MiB.
The `codexAuth` snapshot is a host-sanitized object containing only
`OPENAI_API_KEY`; OAuth and mixed-auth files are withheld.

A distributable package is a ZIP with no directory entries. It contains:

- canonical compact `manifest.json`;
- `manifest.sig`, containing the base64 Ed25519 signature also published by the
  registry index;
- `plugin.wasm` and any other payloads declared by lowercase SHA-256 in the
  manifest.

Every payload path and digest is signed through the manifest. The registry
index additionally pins the manifest digest and complete ZIP digest. A source
accepts a package only when the manifest's `(publisher ID, key ID)` matches one
of that source's configured public keys. Install approval binds the source
revision, both digests, and the exact publisher-key fingerprint.

Publishing tooling and a hosted registry are intentionally outside this step.
The exact schemas and limits are frozen in
[`../docs/architecture/0002-plugin-contract-v1.md`](../docs/architecture/0002-plugin-contract-v1.md).
