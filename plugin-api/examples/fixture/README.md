# Runtime fixture

This component proves that contract major 1 is consumable by a normal Rust
guest. Rebuild the committed test fixture from the repository root with:

```sh
rustup target add wasm32-wasip2
fixture_target=$(mktemp -d /tmp/cc-switch-plugin-fixture.XXXXXX)
CARGO_TARGET_DIR="$fixture_target" cargo build \
  --manifest-path plugin-api/examples/fixture/Cargo.toml \
  --release --target wasm32-wasip2 --locked
cp "$fixture_target/wasm32-wasip2/release/cc_switch_plugin_fixture.wasm" \
  src-tauri/testdata/plugin-fixture.wasm
rm -rf "$fixture_target"
```

The fixture deliberately exports only `invoke` and returns the smallest valid
host response. It is test data, not a marketplace plugin.
