//! Opt-in peer for the CLI/Lite shared-database acceptance tests.

use std::{fs, io::Read, path::PathBuf};

use cc_switch_core::AppType;
use serde_json::json;

use crate::{
    live::{LiveConfig, LiveError},
    mcp::{McpServer, McpStore},
    provider::{native_adapter_reference, ProviderDraft, ProviderPresentation, ProviderRecord},
    store::ProviderStore,
};

mod mcp_lifecycle;

#[test]
#[ignore = "invoked by the CLI acceptance test with an isolated fixture"]
fn create_mcp_in_cli_fixture() {
    let home = fixture_home();
    let live = LiveConfig::from_home(&home).unwrap();
    let store = McpStore::open(home.join(".cc-switch/cc-switch.db")).unwrap();
    store
        .upsert_with_live(
            McpServer {
                id: "lite-peer".into(),
                name: "Lite MCP peer fixture".into(),
                server: json!({"command":"not-executed-lite-peer"}),
                apps: Default::default(),
                description: Some("Retain this independently committed record".into()),
                homepage: None,
                docs: None,
                tags: vec!["peer-fixture".into()],
                revision: 0,
            },
            |changes| live.apply_mcp_recoverable(changes),
            |receipt| {
                live.rollback_mcp(receipt)
                    .map_err(|error| error.to_string())
            },
        )
        .unwrap()
        .unwrap();
}

#[test]
#[ignore = "invoked by the CLI acceptance test with an isolated fixture"]
fn toggle_mcp_in_cli_fixture() {
    let home = fixture_home();
    let live = LiveConfig::from_home(&home).unwrap();
    let store = McpStore::open(home.join(".cc-switch/cc-switch.db")).unwrap();
    let server = store
        .list()
        .unwrap()
        .into_iter()
        .find(|server| server.id == "cli-target")
        .unwrap();
    let enabled = match std::env::var("CC_SWITCH_COORDINATION_MODE")
        .unwrap()
        .as_str()
    {
        "enable" => true,
        "disable" => false,
        mode => panic!("unexpected MCP peer mode: {mode}"),
    };
    store
        .toggle_with_live(
            &server.id,
            server.revision,
            AppType::Gemini,
            enabled,
            |changes| live.apply_mcp_recoverable(changes),
            |receipt| {
                live.rollback_mcp(receipt)
                    .map_err(|error| error.to_string())
            },
        )
        .unwrap()
        .unwrap();
}

#[test]
#[ignore = "invoked by the CLI acceptance test with an isolated fixture"]
fn create_provider_in_cli_fixture() {
    let home = fixture_home();
    let store = ProviderStore::open(home.join(".cc-switch/cc-switch.db")).unwrap();
    let provider = store
        .create_native_with_presentation(
            ProviderDraft {
                app_id: "gemini".into(),
                adapter: native_adapter_reference(&AppType::Gemini),
                name: "Lite peer fixture".into(),
                settings: json!({"env":{"GEMINI_API_KEY":"peer-fake"}})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
            ProviderPresentation::default(),
        )
        .unwrap();
    fs::write(home.join("lite-provider-id"), provider.id).unwrap();
}

fn fixture_home() -> PathBuf {
    let home = PathBuf::from(
        std::env::var_os("CC_SWITCH_COORDINATION_HOME")
            .expect("the CLI test must provide its temporary fixture"),
    );
    let home = home.canonicalize().unwrap();
    let temporary = std::env::temp_dir().canonicalize().unwrap();
    assert!(home.starts_with(&temporary) && home != temporary);
    assert_eq!(
        fs::read_to_string(home.join("coordination-fixture")).unwrap(),
        "cli-lite-v1"
    );
    let database = home.join(".cc-switch/cc-switch.db");
    assert!(database.is_file(), "CLI must initialize the shared fixture");
    assert!(database.canonicalize().unwrap().starts_with(&home));
    home
}

#[test]
#[ignore = "invoked by the CLI acceptance test with an isolated fixture"]
fn native_switch_in_cli_fixture() {
    let home = fixture_home();
    let live = LiveConfig::from_home(&home).unwrap();
    let mode = std::env::var("CC_SWITCH_COORDINATION_MODE").unwrap();
    if mode == "store_switch" {
        let store = ProviderStore::open(home.join(".cc-switch/cc-switch.db")).unwrap();
        let provider = store
            .list("gemini")
            .unwrap()
            .into_iter()
            .find(|p| p.id == "new")
            .unwrap();
        store
            .switch_with_provider(
                "gemini",
                &provider.id,
                provider.revision,
                |provider, snippet| live.switch_native_recoverable(provider, snippet),
                |receipt| live.rollback(receipt).map_err(|error| error.to_string()),
            )
            .unwrap()
            .unwrap();
        return;
    }
    // Exercise the real native service separately from the database gate:
    // a database-busy error alone would not prove file-lock participation.
    let provider = ProviderRecord {
        id: "native-peer".into(),
        revision: 0,
        app_id: "gemini".into(),
        adapter: native_adapter_reference(&AppType::Gemini),
        name: "Native peer".into(),
        settings: json!({"env":{"GEMINI_API_KEY":"lite-fake"}})
            .as_object()
            .unwrap()
            .clone(),
        category: None,
        metadata: json!({}),
        extensions: Default::default(),
    };
    let result = live.switch_native_recoverable(&provider, None);
    if mode == "probe_locked" {
        match result {
            Err(LiveError::LockUnavailable) => return,
            Ok(receipt) => {
                let _ = live.rollback(receipt);
                panic!("Lite native writer entered CLI's protected operation");
            }
            Err(error) => panic!("expected lock contention, got {error}"),
        }
    }
    let receipt = result.unwrap();
    if mode == "hold" {
        fs::write(home.join("lite-native-held"), "ready").unwrap();
        let mut release = [0];
        std::io::stdin().read_exact(&mut release).unwrap();
        assert_eq!(release, *b"r");
    } else {
        assert_eq!(mode, "probe_released");
    }
    live.rollback(receipt).unwrap();
}
