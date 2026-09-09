use super::*;
use crate::{
    skill::{SkillError, SkillStore},
    skill_live::SkillHostError,
};

#[test]
#[ignore = "invoked by the CLI acceptance test with an isolated fixture"]
fn skill_in_cli_fixture() {
    let home = fixture_home();
    let path = home.join(".cc-switch/cc-switch.db");
    let live = LiveConfig::for_skill_fixture(&home);
    let mode = std::env::var("CC_SWITCH_COORDINATION_MODE").unwrap();
    if let Some(request) = mode.strip_prefix("deployment:") {
        let (app, action) = request.split_once(':').expect("App and deployment action");
        let app = app.parse::<AppType>().unwrap();
        assert_eq!(app, AppType::Claude);
        let enabled = match action {
            "enable" => true,
            "disable" => false,
            _ => panic!("unexpected deployment action: {action}"),
        };
        let store = SkillStore::open(path).unwrap();
        let result = store.toggle(&live, "cli-skill", app.clone(), enabled);
        let error = result.err().map(|error| {
            json!({
                "code": error.code(), "message": error.to_string()
            })
        });
        let snapshots = store.list(&live).unwrap();
        let state = snapshots
            .iter()
            .find(|skill| skill.id() == "cli-skill")
            .unwrap()
            .apps()
            .find(|state| state.app() == &app)
            .unwrap();
        fs::write(
            home.join("lite-skill-deployment.json"),
            serde_json::to_vec(&json!({"error": error, "state": state})).unwrap(),
        )
        .unwrap();
        return;
    }
    if mode == "hold" {
        // Hold a real native receipt without a database write lock, so refusal
        // cannot be explained by SQLite contention alone.
        let catalog =
            cc_switch_store::read_skill_catalog(&rusqlite::Connection::open(&path).unwrap())
                .unwrap();
        let receipt = live
            .apply_skill_recoverable(&catalog, "cli-skill", &AppType::Gemini, Some(true))
            .unwrap();
        fs::write(home.join("lite-skill-held"), "ready").unwrap();
        let mut release = [0];
        std::io::stdin().read_exact(&mut release).unwrap();
        assert_eq!(release, *b"r");
        live.rollback_skill(receipt).unwrap();
        return;
    }
    assert!(matches!(mode.as_str(), "toggle" | "interleave"));
    let result = SkillStore::open(path)
        .and_then(|store| store.toggle(&live, "cli-skill", AppType::Gemini, true));
    let outcome = match result {
        Ok(()) => "committed",
        Err(SkillError::Database(rusqlite::Error::SqliteFailure(error, _)))
            if mode == "interleave"
                && matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
        {
            "busy"
        }
        Err(SkillError::Host(SkillHostError::LockUnavailable)) if mode == "interleave" => "busy",
        Err(error) => panic!("unexpected Lite Skill failure: {error}"),
    };
    fs::write(home.join("lite-skill-outcome"), outcome).unwrap();
}
