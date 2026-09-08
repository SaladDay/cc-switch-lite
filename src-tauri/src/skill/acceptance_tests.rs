//! Observe the real Store/native path at compensation entry.

use super::*;
use cc_switch_core::fs::{
    shared_live_config_lock_path, SharedLiveConfigLock, SharedLiveConfigLockError,
};
use std::{
    cell::{Cell, RefCell},
    fs,
    rc::Rc,
    time::Duration,
};

thread_local! {
    static RECOVERY: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

pub(super) fn before_recovery() {
    let hook = RECOVERY.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

fn with_recovery<T>(hook: impl FnOnce() + 'static, action: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            RECOVERY.with(|slot| *slot.borrow_mut() = None);
        }
    }
    RECOVERY.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
    let _reset = Reset;
    action()
}

#[test]
#[ignore = "acceptance gate: Skill commit-failure lock lifetime awaits adoption"]
fn skill_commit_failure_keeps_database_lock_at_native_recovery_entry() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".cc-switch/cc-switch.db");
    let store = SkillStore::open(path.clone()).unwrap();
    let conn = store.connect().unwrap();
    conn.execute_batch(
        "INSERT INTO skills (id,name,directory) VALUES ('demo','Demo','demo');
         CREATE UNIQUE INDEX fixture_skill_reference ON skills(id,enabled_claude);
         CREATE TABLE fixture_commit_guard(id TEXT, selected INTEGER,
           FOREIGN KEY(id,selected) REFERENCES skills(id,enabled_claude)
           DEFERRABLE INITIALLY DEFERRED);
         INSERT INTO fixture_commit_guard VALUES ('demo',0);",
    )
    .unwrap();
    drop(conn);
    let source = home.path().join(".cc-switch/skills/demo");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("SKILL.md"), "# Demo\n").unwrap();
    let live = LiveConfig::for_skill_fixture(home.path());
    let observed = Rc::new(Cell::new(None));
    let recorded = observed.clone();
    let probe_home = home.path().to_owned();
    let result = with_recovery(
        move || {
            assert!(
                probe_home.join(".claude/skills/demo/SKILL.md").is_file(),
                "native publication must precede the commit failure"
            );
            let file_locked =
                match SharedLiveConfigLock::try_acquire(&shared_live_config_lock_path(&probe_home))
                {
                    Err(SharedLiveConfigLockError::Unavailable) => true,
                    Ok(guard) => {
                        drop(guard);
                        false
                    }
                    Err(error) => panic!("unexpected lock error: {error}"),
                };
            let peer = rusqlite::Connection::open(&path).unwrap();
            peer.busy_timeout(Duration::ZERO).unwrap();
            let database_locked = match peer.execute_batch("BEGIN IMMEDIATE") {
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    true
                }
                Ok(()) => {
                    peer.execute_batch("ROLLBACK").unwrap();
                    false
                }
                Err(error) => panic!("unexpected database probe failure: {error}"),
            };
            recorded.set(Some((file_locked, database_locked)));
        },
        || store.toggle(&live, "demo", AppType::Claude, true),
    );
    assert!(
        matches!(result, Err(SkillError::Database(rusqlite::Error::SqliteFailure(error, _)))
        if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
        "expected deferred COMMIT failure: {result:?}"
    );
    assert!(!home.path().join(".claude/skills/demo").exists());
    assert_eq!(
        read_skill_catalog_entry(&store.connect().unwrap(), "demo")
            .unwrap()
            .unwrap()
            .selected_for(&AppType::Claude),
        Some(false)
    );
    drop(SharedLiveConfigLock::try_acquire(&shared_live_config_lock_path(home.path())).unwrap());
    store
        .connect()
        .unwrap()
        .execute_batch("DROP TABLE fixture_commit_guard")
        .unwrap();
    store.toggle(&live, "demo", AppType::Claude, true).unwrap();
    assert!(home.path().join(".claude/skills/demo/SKILL.md").is_file());
    assert_eq!(
        observed.get(),
        Some((true, true)),
        "both locks must still be held at native recovery entry"
    );
}
