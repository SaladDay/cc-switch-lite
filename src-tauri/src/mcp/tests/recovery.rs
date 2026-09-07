use super::*;
use crate::live::LiveConfig;
use cc_switch_core::fs::{
    shared_live_config_lock_path, SharedLiveConfigLock, SharedLiveConfigLockError,
};
use std::{cell::Cell, fs, time::Duration};

#[derive(Clone, Copy, Debug)]
enum Operation {
    Insert,
    Update,
    Toggle,
    Delete,
}

#[derive(Clone, Copy)]
enum Failure {
    Commit,
    Verification,
    Write,
}

fn recovery_case(operation: Operation, failure: Failure, report_recovery_error: bool) {
    let home = tempdir().unwrap();
    let path = home.path().join(".cc-switch/cc-switch.db");
    let store = McpStore::open(path.clone()).unwrap();
    let live = LiveConfig::from_home(home.path()).unwrap();
    let native = home.path().join(".gemini/settings.json");
    fs::create_dir_all(native.parent().unwrap()).unwrap();
    fs::write(&native, "{\"unowned\":{\"opaque\":true}}\n").unwrap();
    let mut draft = server();
    draft.apps = apps_enabled([AppType::Gemini]);
    draft.server = json!({"type":"stdio","command":"original"});
    if !matches!(operation, Operation::Insert) {
        store
            .upsert_with_live(
                draft.clone(),
                |changes| live.apply_mcp_recoverable(changes),
                |receipt| {
                    live.rollback_mcp(receipt)
                        .map_err(|error| error.to_string())
                },
            )
            .unwrap()
            .unwrap();
        draft = store.list().unwrap().remove(0);
    }
    let before_native = fs::read(&native).unwrap();
    let before_catalog = store.list().unwrap();
    let links = || {
        let connection = store.connect().unwrap();
        let mut query = connection.prepare(
            "SELECT server_id, app_id, native_snapshot FROM mcp_native_links ORDER BY server_id, app_id"
        ).unwrap();
        query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let before_links = links();
    let setup = store.connect().unwrap();
    if matches!(failure, Failure::Verification) {
        setup
            .execute_batch(
                "CREATE TRIGGER fixture_reject AFTER INSERT ON mcp_native_links BEGIN
                UPDATE mcp_servers SET name = 'unexpected' WHERE id = NEW.server_id;
             END;",
            )
            .unwrap();
    } else {
        setup
            .execute_batch(
                "CREATE TABLE fixture_parent (id INTEGER PRIMARY KEY);
             CREATE TABLE fixture_commit_guard (id INTEGER REFERENCES fixture_parent(id)
                 DEFERRABLE INITIALLY DEFERRED);",
            )
            .unwrap();
        let event = match operation {
            Operation::Insert => "INSERT",
            Operation::Update | Operation::Toggle => "UPDATE",
            Operation::Delete => "DELETE",
        };
        let trigger = if matches!(failure, Failure::Write) {
            format!(
                "CREATE TRIGGER fixture_reject BEFORE {event} ON mcp_servers BEGIN
                SELECT RAISE(IGNORE); END;"
            )
        } else {
            format!(
                "CREATE TRIGGER fixture_reject AFTER {event} ON mcp_servers BEGIN
                INSERT INTO fixture_commit_guard VALUES (1);
             END;"
            )
        };
        setup.execute_batch(&trigger).unwrap();
    }
    drop(setup);
    draft.server = json!({"type":"stdio","command":"changed"});
    let restored = Cell::new(false);
    let apply = |changes: &mut [McpLiveChange]| live.apply_mcp_recoverable(changes);
    let rollback = |receipt| {
        assert!(matches!(
            SharedLiveConfigLock::try_acquire(&shared_live_config_lock_path(home.path())),
            Err(SharedLiveConfigLockError::Unavailable)
        ));
        let peer = Connection::open(&path).unwrap();
        peer.busy_timeout(Duration::ZERO).unwrap();
        assert!(
            matches!(peer.execute_batch("BEGIN IMMEDIATE"),
            Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::DatabaseBusy),
            "{operation:?}: database protection must remain during native recovery"
        );
        live.rollback_mcp(receipt)
            .map_err(|error| error.to_string())?;
        restored.set(true);
        if report_recovery_error {
            Err("fixture recovery report".to_owned())
        } else {
            Ok(())
        }
    };
    let result = match operation {
        Operation::Insert | Operation::Update => {
            store.upsert_with_live(draft.clone(), apply, rollback)
        }
        Operation::Toggle => store.toggle_with_live(
            &draft.id,
            draft.revision,
            AppType::Gemini,
            false,
            apply,
            rollback,
        ),
        Operation::Delete => store.delete_with_live(&draft.id, draft.revision, apply, rollback),
    };
    if report_recovery_error {
        assert!(
            matches!(result, Err(McpError::Recovery(message)) if message.contains("fixture recovery report"))
        );
    } else if !matches!(failure, Failure::Commit) {
        assert!(matches!(result, Err(McpError::Conflict)));
    } else {
        assert!(
            matches!(result, Err(McpError::Database(rusqlite::Error::SqliteFailure(error, _)))
            if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY)
        );
    }
    assert!(restored.get());
    assert_eq!(fs::read(&native).unwrap(), before_native);
    assert_eq!(store.list().unwrap(), before_catalog);
    assert_eq!(links(), before_links);
    let lock =
        SharedLiveConfigLock::try_acquire(&shared_live_config_lock_path(home.path())).unwrap();
    drop(lock);
    let peer = Connection::open(&path).unwrap();
    peer.busy_timeout(Duration::ZERO).unwrap();
    peer.execute_batch("BEGIN IMMEDIATE; ROLLBACK; DROP TRIGGER fixture_reject;")
        .unwrap();
    drop(peer);
    let apply = |changes: &mut [McpLiveChange]| live.apply_mcp_recoverable(changes);
    let rollback = |receipt| {
        live.rollback_mcp(receipt)
            .map_err(|error| error.to_string())
    };
    let retry = match operation {
        Operation::Insert | Operation::Update => store.upsert_with_live(draft, apply, rollback),
        Operation::Toggle => store.toggle_with_live(
            &draft.id,
            draft.revision,
            AppType::Gemini,
            false,
            apply,
            rollback,
        ),
        Operation::Delete => store.delete_with_live(&draft.id, draft.revision, apply, rollback),
    };
    retry.unwrap().unwrap();
}

#[test]
fn mcp_commit_failure_retains_protection_for_every_mutator() {
    for operation in [
        Operation::Insert,
        Operation::Update,
        Operation::Toggle,
        Operation::Delete,
    ] {
        for report in [false, true] {
            recovery_case(operation, Failure::Commit, report);
        }
    }
}

#[test]
fn mcp_verification_failure_retains_protection_through_recovery() {
    for report in [false, true] {
        recovery_case(Operation::Insert, Failure::Verification, report);
    }
}

#[test]
fn mcp_write_failure_retains_protection_for_every_mutator() {
    for operation in [
        Operation::Insert,
        Operation::Update,
        Operation::Toggle,
        Operation::Delete,
    ] {
        for report in [false, true] {
            recovery_case(operation, Failure::Write, report);
        }
    }
}
