use super::*;

#[test]
#[ignore = "invoked by the CLI acceptance test with an isolated fixture"]
fn mcp_lifecycle_in_cli_fixture() {
    let home = fixture_home();
    let mode = std::env::var("CC_SWITCH_COORDINATION_MODE").unwrap();
    let (app, action) = mode.split_once(':').expect("App and action");
    let app = app.parse::<AppType>().unwrap();
    assert!(matches!(
        app,
        AppType::Claude | AppType::Codex | AppType::Gemini | AppType::OpenCode | AppType::Hermes
    ));
    let live = LiveConfig::from_home(&home).unwrap();
    live.assert_mcp_fixture_paths(&home);
    let store = McpStore::open(home.join(".cc-switch/cc-switch.db")).unwrap();
    if action == "import" {
        let report = store
            .import_with_live(
                || live.observe_mcp(),
                |observation| live.mcp_observation_is_current(observation),
            )
            .unwrap()
            .unwrap();
        fs::write(
            home.join("lite-mcp-import-report.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        return;
    }
    let server = store
        .list()
        .unwrap()
        .into_iter()
        .find(|server| server.id == "cli-import")
        .expect("the shared target exists");
    let result = match action {
        "enable" | "disable" => store.toggle_with_live(
            &server.id,
            server.revision,
            app,
            action == "enable",
            |changes| live.apply_mcp_recoverable(changes),
            |receipt| {
                live.rollback_mcp(receipt)
                    .map_err(|error| error.to_string())
            },
        ),
        "delete" => store.delete_with_live(
            &server.id,
            server.revision,
            |changes| live.apply_mcp_recoverable(changes),
            |receipt| {
                live.rollback_mcp(receipt)
                    .map_err(|error| error.to_string())
            },
        ),
        _ => panic!("unexpected MCP lifecycle action: {action}"),
    };
    result.unwrap().unwrap();
}
