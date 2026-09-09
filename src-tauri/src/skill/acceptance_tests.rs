//! Observe real Skill recovery before and after the Core receipt is restored.

use super::*;
use cc_switch_core::fs::{
    shared_live_config_lock_path, SharedLiveConfigLock, SharedLiveConfigLockError,
};
use std::{cell::RefCell, fs, path::Path, rc::Rc, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryPoint {
    BeforeNative,
    AfterNative,
}

type RecoveryHook = Box<dyn FnMut(RecoveryPoint)>;

thread_local! {
    static RECOVERY: RefCell<Option<RecoveryHook>> = RefCell::new(None);
}

fn observe(point: RecoveryPoint) {
    RECOVERY.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(point);
        }
    });
}

pub(super) fn before_recovery() {
    observe(RecoveryPoint::BeforeNative);
}

pub(crate) fn after_recovery() {
    observe(RecoveryPoint::AfterNative);
}

fn with_recovery<T>(hook: impl FnMut(RecoveryPoint) + 'static, action: impl FnOnce() -> T) -> T {
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

fn probe_locks(home: &Path) -> (bool, bool) {
    let file_locked = match SharedLiveConfigLock::try_acquire(&shared_live_config_lock_path(home)) {
        Err(SharedLiveConfigLockError::Unavailable) => true,
        Ok(guard) => {
            drop(guard);
            false
        }
        Err(error) => panic!("unexpected lock error: {error}"),
    };
    let peer = rusqlite::Connection::open(home.join(".cc-switch/cc-switch.db")).unwrap();
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
    (file_locked, database_locked)
}

#[derive(Clone, Copy, Debug)]
enum Failure {
    DeferredCommit,
    StatementAbort,
    TransactionAbort,
}

fn exercise_recovery(app: AppType, enabled: bool, failure: Failure, external_edit: bool) {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".cc-switch/cc-switch.db");
    let store = SkillStore::open(path).unwrap();
    store.connect().unwrap().execute_batch(
        "INSERT INTO skills (id,name,directory) VALUES ('demo','Demo','demo'), ('peer','Peer','peer');
         ALTER TABLE skills ADD COLUMN fixture_opaque BLOB;
         UPDATE skills SET fixture_opaque = X'00FF81', enabled_pi = 1;"
    ).unwrap();
    let source = home.path().join(".cc-switch/skills/demo");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("SKILL.md"), "# Demo\n").unwrap();
    fs::write(source.join("asset.bin"), [0, 255, 128]).unwrap();
    let gemini_config = home.path().join(".gemini/settings.json");
    fs::create_dir_all(gemini_config.parent().unwrap()).unwrap();
    fs::write(
        &gemini_config,
        "{\"fixture\":true,\"skills\":{\"disabled\":[\"Demo\",\"Peer\"]}}\n",
    )
    .unwrap();
    let live = LiveConfig::for_skill_fixture(home.path());
    if !enabled {
        store.toggle(&live, "demo", app.clone(), true).unwrap();
    }
    let native = home.path().join(format!(".{}/skills/demo", app.as_str()));
    let column = match app {
        AppType::Claude => "enabled_claude",
        AppType::Gemini => "enabled_gemini",
        _ => panic!("unexpected fixture App"),
    };
    let conn = store.connect().unwrap();
    let rows = cc_switch_store::read_skill_catalog_rows(&conn).unwrap();
    let config = fs::read(&gemini_config).unwrap();
    match failure {
        Failure::DeferredCommit => conn.execute_batch(&format!(
            "CREATE UNIQUE INDEX fixture_skill_reference ON skills(id,{column});
             CREATE TABLE fixture_commit_guard(id TEXT, selected INTEGER,
               FOREIGN KEY(id,selected) REFERENCES skills(id,{column})
               DEFERRABLE INITIALLY DEFERRED);
             INSERT INTO fixture_commit_guard VALUES ('demo',{});",
            i64::from(!enabled)
        )),
        Failure::StatementAbort | Failure::TransactionAbort => {
            let action = if matches!(failure, Failure::TransactionAbort) {
                "ROLLBACK"
            } else {
                "ABORT"
            };
            conn.execute_batch(&format!(
                "CREATE TRIGGER fixture_reject BEFORE UPDATE OF {column} ON skills
                 WHEN OLD.id = 'demo' AND NEW.{column} != OLD.{column}
                 BEGIN SELECT RAISE({action}, 'fixture rejects selection'); END;"
            ))
        }
    }
    .unwrap();
    drop(conn);

    let observed = Rc::new(RefCell::new(Vec::new()));
    let recorded = observed.clone();
    let probe_home = home.path().to_owned();
    let probe_native = native.clone();
    let result = with_recovery(
        move |point| {
            recorded
                .borrow_mut()
                .push((point, probe_locks(&probe_home)));
            if !external_edit {
                assert_eq!(
                    probe_native.join("SKILL.md").is_file(),
                    if point == RecoveryPoint::BeforeNative {
                        enabled
                    } else {
                        !enabled
                    },
                    "{point:?} must observe the actual native change/recovery",
                );
            } else if point == RecoveryPoint::BeforeNative {
                // Replace only the fixture's public link. Recovery must preserve this edit.
                #[cfg(unix)]
                fs::remove_file(&probe_native).unwrap();
                #[cfg(windows)]
                fs::remove_dir(&probe_native).unwrap();
                fs::create_dir(&probe_native).unwrap();
                fs::write(probe_native.join("external"), "keep").unwrap();
            }
        },
        || store.toggle(&live, "demo", app.clone(), enabled),
    );
    if external_edit {
        assert!(
            matches!(result, Err(SkillError::Recovery(ref error))
            if error.contains("update error:") && error.contains("live recovery error:")),
            "both failures must be reported: {result:?}"
        );
        assert_eq!(fs::read(native.join("external")).unwrap(), b"keep");
    } else {
        match failure {
            Failure::DeferredCommit => assert!(
                matches!(result, Err(SkillError::Database(rusqlite::Error::SqliteFailure(error, _)))
                    if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
                "expected deferred COMMIT failure: {result:?}"
            ),
            Failure::StatementAbort | Failure::TransactionAbort => assert!(
                matches!(result, Err(SkillError::SharedWrite(SharedStoreError::SkillCatalogWrite {
                    transaction_aborted, extended_code: Some(code), ..
                })) if transaction_aborted == matches!(failure, Failure::TransactionAbort)
                    && code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER),
                "expected catalog statement failure: {result:?}"
            ),
        }
        assert_eq!(native.join("SKILL.md").is_file(), !enabled);
        if !enabled {
            assert_eq!(fs::read(native.join("asset.bin")).unwrap(), [0, 255, 128]);
        }
        assert_eq!(fs::read(&gemini_config).unwrap(), config);
    }
    assert_eq!(
        cc_switch_store::read_skill_catalog_rows(&store.connect().unwrap()).unwrap(),
        rows
    );
    assert_eq!(
        probe_locks(home.path()),
        (false, false),
        "both locks must be released"
    );

    if !external_edit {
        store
            .connect()
            .unwrap()
            .execute_batch(match failure {
                Failure::DeferredCommit => "DROP TABLE fixture_commit_guard",
                _ => "DROP TRIGGER fixture_reject",
            })
            .unwrap();
        store.toggle(&live, "demo", app.clone(), enabled).unwrap();
        assert_eq!(native.join("SKILL.md").is_file(), enabled);
        assert_eq!(
            read_skill_catalog_entry(&store.connect().unwrap(), "demo")
                .unwrap()
                .unwrap()
                .selected_for(&app),
            Some(enabled)
        );
    }
    let expected = (true, !matches!(failure, Failure::TransactionAbort));
    assert_eq!(
        *observed.borrow(),
        vec![
            (RecoveryPoint::BeforeNative, expected),
            (RecoveryPoint::AfterNative, expected)
        ],
        "native protection must span recovery; SQLite may itself abort its transaction"
    );
}

#[test]
fn skill_commit_failure_keeps_locks_through_native_recovery() {
    for app in [AppType::Claude, AppType::Gemini] {
        for enabled in [true, false] {
            exercise_recovery(app.clone(), enabled, Failure::DeferredCommit, false);
        }
    }
}

#[test]
fn skill_statement_failure_keeps_locks_through_native_recovery() {
    for app in [AppType::Claude, AppType::Gemini] {
        for enabled in [true, false] {
            exercise_recovery(app.clone(), enabled, Failure::StatementAbort, false);
        }
    }
}

#[test]
fn skill_sqlite_abort_keeps_native_protection_and_allows_retry() {
    for app in [AppType::Claude, AppType::Gemini] {
        for enabled in [true, false] {
            exercise_recovery(app.clone(), enabled, Failure::TransactionAbort, false);
        }
    }
}

#[test]
fn skill_failed_recovery_preserves_external_edits_and_reports_both_errors() {
    exercise_recovery(AppType::Claude, true, Failure::DeferredCommit, true);
}
