//! Opt-in peer for the CLI/Lite shared-database acceptance tests.

use std::{fs, path::PathBuf};

use cc_switch_core::AppType;
use serde_json::json;

use crate::{
    provider::{native_adapter_reference, ProviderDraft, ProviderPresentation},
    store::ProviderStore,
};

#[test]
#[ignore = "invoked by the CLI acceptance test with an isolated fixture"]
fn create_provider_in_cli_fixture() {
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
    let store = ProviderStore::open(database).unwrap();
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
