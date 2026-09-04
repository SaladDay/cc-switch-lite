use std::{fs, path::PathBuf};

use cc_switch_core::{
    builtin_app_adapter, builtin_app_registry, fs::atomic_write, mcp_servers_equivalent,
    AppCapability, AppType, McpConfigResource, McpServerProjection,
};

use crate::{
    live::{LiveError, ResolvedConfigDirs},
    mcp::{McpImportsByApp, McpLiveChange, McpNativeLinkState},
    operation::{read_optional, resolve_write_path, LivePaths, OperationError},
};

struct McpPath {
    app: AppType,
    path: PathBuf,
    install_marker: PathBuf,
}

pub struct McpLiveConfig {
    paths: Vec<McpPath>,
}

pub struct McpLiveReceipt {
    writes: Vec<McpFileReceipt>,
}

pub struct McpImportSnapshot {
    pub imports: McpImportsByApp,
    observations: Vec<McpFileObservation>,
}

#[derive(Clone, PartialEq)]
struct McpFileObservation {
    app: AppType,
    contents: Result<Option<Vec<u8>>, String>,
}

struct McpFileReceipt {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
}

impl McpLiveConfig {
    pub fn new(
        native: &LivePaths,
        roots: &ResolvedConfigDirs,
        host_defined: impl IntoIterator<Item = (AppType, PathBuf)>,
    ) -> Result<Self, LiveError> {
        let host_defined = host_defined.into_iter().collect::<Vec<_>>();
        let paths = builtin_app_registry()
            .descriptors()
            .filter_map(|descriptor| {
                descriptor
                    .mcp_contract()
                    .map(|contract| (descriptor, contract))
            })
            .map(|(descriptor, contract)| {
                let app = descriptor.app().clone();
                let path = match contract.resource() {
                    McpConfigResource::LogicalTarget(target) => native.path_for(target).to_owned(),
                    McpConfigResource::HostDefined => host_defined
                        .iter()
                        .find_map(|(candidate, path)| (candidate == &app).then(|| path.clone()))
                        .ok_or_else(|| {
                            LiveError::InvalidConfig(format!(
                                "host MCP path is missing for {}",
                                descriptor.id()
                            ))
                        })?,
                    _ => {
                        return Err(LiveError::InvalidConfig(format!(
                            "MCP resource is not supported for {}",
                            descriptor.id()
                        )))
                    }
                };
                Ok(McpPath {
                    install_marker: roots.root(&app)?.to_owned(),
                    app,
                    path,
                })
            })
            .collect::<Result<Vec<_>, LiveError>>()?;
        Ok(Self { paths })
    }

    pub fn apply(&self, changes: &mut [McpLiveChange]) -> Result<McpLiveReceipt, LiveError> {
        let mut receipt = McpLiveReceipt { writes: Vec::new() };
        for change in changes {
            let applied = self.apply_one(change, &mut receipt).and_then(|()| {
                if let McpLiveChange::Upsert {
                    app, link_state, ..
                } = change
                {
                    if *link_state != McpNativeLinkState::Observed {
                        return Err(LiveError::Missing(format!(
                            "{} MCP configuration",
                            app.as_str()
                        )));
                    }
                }
                Ok(())
            });
            if let Err(error) = applied {
                if let Err(rollback) = rollback_writes(receipt.writes) {
                    return Err(LiveError::Recovery(format!(
                        "MCP write error: {error}; live recovery error: {rollback}"
                    )));
                }
                return Err(error);
            }
        }
        Ok(receipt)
    }

    pub fn rollback(&self, receipt: McpLiveReceipt) -> Result<(), LiveError> {
        rollback_writes(receipt.writes)
    }

    pub fn import_snapshot(&self) -> McpImportSnapshot {
        let observations = self.observe_files();
        let imports = observations
            .iter()
            .map(|observation| {
                let imports = match &observation.contents {
                    Ok(contents) => builtin_app_adapter(&observation.app)
                        .import_mcp_servers(contents.as_deref())
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.clone()),
                };
                (observation.app.clone(), imports)
            })
            .collect();
        McpImportSnapshot {
            imports,
            observations,
        }
    }

    pub fn snapshot_is_current(&self, snapshot: &McpImportSnapshot) -> bool {
        self.observe_files() == snapshot.observations
    }

    fn observe_files(&self) -> Vec<McpFileObservation> {
        builtin_app_registry()
            .descriptors()
            .filter(|descriptor| descriptor.supports(AppCapability::Mcp))
            .map(|descriptor| {
                let app = descriptor.app().clone();
                let contents = self
                    .path_for_app(&app)
                    .and_then(|path| {
                        if !path.path.exists() && !path.install_marker.exists() {
                            return Ok(None);
                        }
                        read_optional(&path.path).map_err(Into::into)
                    })
                    .map_err(|error| error.to_string());
                McpFileObservation { app, contents }
            })
            .collect()
    }

    fn apply_one(
        &self,
        change: &mut McpLiveChange,
        receipt: &mut McpLiveReceipt,
    ) -> Result<(), LiveError> {
        let app = change.app().clone();
        let configured = self.path_for_app(&app)?;
        if !configured.path.exists() && !configured.install_marker.exists() {
            return Ok(());
        }
        let path = resolve_write_path(&configured.path)?;
        let before = read_optional(&path)?;
        let adapter = builtin_app_adapter(&app);
        let imports = match adapter.import_mcp_servers(before.as_deref()) {
            Ok(imports) => imports,
            Err(error) => return Err(LiveError::InvalidConfig(error.to_string())),
        };
        let current = imports.iter().find(|entry| entry.id == change.id());
        let (id, projection) = match change {
            McpLiveChange::Upsert {
                id,
                previous,
                server,
                native_snapshot,
                ..
            } => {
                if current.is_some_and(|current| {
                    !mcp_servers_equivalent(&app, &current.server, server)
                        && previous.as_ref().is_none_or(|previous| {
                            !mcp_servers_equivalent(&app, &current.server, previous)
                        })
                }) {
                    return Err(OperationError::Conflict.into());
                }
                let projection = if current.is_none() {
                    native_snapshot
                        .as_ref()
                        .map_or(McpServerProjection::Enable(server), |snapshot| {
                            McpServerProjection::Restore { server, snapshot }
                        })
                } else {
                    McpServerProjection::Enable(server)
                };
                (id.as_str(), projection)
            }
            McpLiveChange::Disable {
                id,
                previous,
                server,
                native_snapshot,
                link_state,
                ..
            } => {
                let Some(current) = current else {
                    return Ok(());
                };
                if !mcp_servers_equivalent(&app, &current.server, previous)
                    && !mcp_servers_equivalent(&app, &current.server, server)
                {
                    return Err(OperationError::Conflict.into());
                }
                if let Some(snapshot) = &current.native_snapshot {
                    *native_snapshot = Some(snapshot.clone());
                }
                *link_state = McpNativeLinkState::Observed;
                (id.as_str(), McpServerProjection::Disable(server))
            }
            McpLiveChange::Remove { id, server, .. } => {
                let Some(current) = current else {
                    return Ok(());
                };
                if !mcp_servers_equivalent(&app, &current.server, server) {
                    return Err(OperationError::Conflict.into());
                }
                (id.as_str(), McpServerProjection::Remove)
            }
        };
        let Some(projected) = adapter
            .project_mcp_server(before.as_deref(), id, projection)
            .map_err(|error| LiveError::InvalidConfig(error.to_string()))?
        else {
            return Ok(());
        };
        let after = projected.into_bytes();
        if let McpLiveChange::Upsert { link_state, .. } = change {
            *link_state = McpNativeLinkState::Observed;
        }
        if before.as_deref() == Some(after.as_slice()) {
            return Ok(());
        }
        if read_optional(&path)? != before {
            return Err(OperationError::Conflict.into());
        }
        atomic_write(&path, &after).map_err(OperationError::from)?;
        receipt.writes.push(McpFileReceipt {
            path,
            before,
            after,
        });
        Ok(())
    }

    fn path_for_app(&self, app: &AppType) -> Result<&McpPath, LiveError> {
        self.paths
            .iter()
            .find(|path| &path.app == app)
            .ok_or_else(|| {
                LiveError::InvalidConfig(format!(
                    "application '{}' does not support MCP",
                    app.as_str()
                ))
            })
    }
}

fn rollback_writes(writes: Vec<McpFileReceipt>) -> Result<(), LiveError> {
    let mut failures = Vec::new();
    for write in writes.into_iter().rev() {
        if let Err(error) = restore_write(write) {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(LiveError::Recovery(failures.join("; ")))
    }
}

fn restore_write(write: McpFileReceipt) -> Result<(), LiveError> {
    if read_optional(&write.path)?.as_deref() != Some(write.after.as_slice()) {
        return Err(LiveError::Recovery(format!(
            "{} changed after the MCP write",
            write.path.display()
        )));
    }
    match write.before {
        Some(contents) => atomic_write(&write.path, &contents)
            .map_err(OperationError::from)
            .map_err(Into::into),
        None => fs::remove_file(&write.path).map_err(|source| LiveError::Io {
            path: write.path,
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpApps, McpServer, McpStore};
    use serde_json::{json, Value};
    use std::path::Path;
    use tempfile::tempdir;

    fn config(home: &Path) -> McpLiveConfig {
        let claude = home.join(".claude");
        let codex = home.join(".codex");
        let roots = crate::live::ResolvedConfigDirs::for_tests(home, claude.clone(), codex.clone());
        let native = crate::native_live::NativeLiveConfig::for_tests(home, claude, codex);
        McpLiveConfig::new(
            native.paths(),
            &roots,
            [(AppType::Claude, home.join(".claude.json"))],
        )
        .expect("test MCP resources are complete")
    }

    #[test]
    fn applies_and_rolls_back_multi_app_changes() {
        let directory = tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".claude")).unwrap();
        fs::create_dir_all(directory.path().join(".codex")).unwrap();
        fs::write(directory.path().join(".claude.json"), "{\"keep\":true}").unwrap();
        fs::write(
            directory.path().join(".codex/config.toml"),
            "model = \"keep\"\n",
        )
        .unwrap();
        let live = config(directory.path());
        let mut changes = [
            McpLiveChange::Upsert {
                app: AppType::Claude,
                id: "server".to_owned(),
                previous: None,
                server: json!({"type":"stdio","command":"npx"}),
                native_snapshot: None,
                link_state: McpNativeLinkState::Unowned,
            },
            McpLiveChange::Upsert {
                app: AppType::Codex,
                id: "server".to_owned(),
                previous: None,
                server: json!({"type":"stdio","command":"npx"}),
                native_snapshot: None,
                link_state: McpNativeLinkState::Unowned,
            },
        ];
        let receipt = live.apply(&mut changes).unwrap();
        let claude: Value =
            serde_json::from_slice(&fs::read(directory.path().join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(claude["mcpServers"]["server"]["command"], "npx");
        assert!(
            fs::read_to_string(directory.path().join(".codex/config.toml"))
                .unwrap()
                .contains("[mcp_servers.server]")
        );

        live.rollback(receipt).unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join(".claude.json")).unwrap(),
            "{\"keep\":true}"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join(".codex/config.toml")).unwrap(),
            "model = \"keep\"\n"
        );
    }

    #[test]
    fn rejects_enabling_apps_that_are_not_installed() {
        let directory = tempdir().unwrap();
        let live = config(directory.path());
        let mut changes = [McpLiveChange::Upsert {
            app: AppType::Gemini,
            id: "server".to_owned(),
            previous: None,
            server: json!({"type":"stdio","command":"npx"}),
            native_snapshot: None,
            link_state: McpNativeLinkState::Unowned,
        }];
        let error = live.apply(&mut changes).err().unwrap();
        assert!(matches!(error, LiveError::Missing(_)));
        assert!(!directory.path().join(".gemini/settings.json").exists());
    }

    #[test]
    fn missing_application_keeps_the_database_disabled() {
        let directory = tempdir().unwrap();
        let live = config(directory.path());
        let store = McpStore::open(directory.path().join("cc-switch.db")).unwrap();
        store
            .upsert_with_live(
                McpServer {
                    id: "server".to_owned(),
                    name: "Server".to_owned(),
                    server: json!({"type":"stdio","command":"npx"}),
                    apps: McpApps::default(),
                    description: None,
                    homepage: None,
                    docs: None,
                    tags: Vec::new(),
                    revision: 0,
                },
                |_| Ok::<_, ()>(()),
                |_| Ok(()),
            )
            .unwrap()
            .unwrap();
        let current = store.list().unwrap().remove(0);

        let result = store
            .toggle_with_live(
                &current.id,
                current.revision,
                AppType::Gemini,
                true,
                |changes| live.apply(changes),
                |receipt| live.rollback(receipt).map_err(|error| error.to_string()),
            )
            .unwrap();

        assert!(matches!(result, Err(LiveError::Missing(_))));
        assert!(!store
            .list()
            .unwrap()
            .remove(0)
            .apps
            .enabled(&AppType::Gemini));
    }

    #[test]
    fn native_disable_keeps_application_owned_fields() {
        let directory = tempdir().unwrap();
        let root = directory.path().join(".config/opencode");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("opencode.json");
        fs::write(
            &path,
            r#"{"mcp":{"server":{"type":"local","command":["old"],"timeout":30}}}"#,
        )
        .unwrap();
        let live = config(directory.path());
        let previous = json!({"type":"stdio","command":"old"});
        let updated = json!({"type":"stdio","command":"new"});

        let mut disable = [McpLiveChange::Disable {
            app: AppType::OpenCode,
            id: "server".to_owned(),
            previous,
            server: updated.clone(),
            native_snapshot: None,
            link_state: McpNativeLinkState::Unowned,
        }];
        live.apply(&mut disable).unwrap();
        let disabled: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(disabled["mcp"]["server"]["command"], json!(["new"]));
        assert_eq!(disabled["mcp"]["server"]["timeout"], 30);
        assert_eq!(disabled["mcp"]["server"]["enabled"], false);

        let mut enable = [McpLiveChange::Upsert {
            app: AppType::OpenCode,
            id: "server".to_owned(),
            previous: Some(updated.clone()),
            server: updated,
            native_snapshot: None,
            link_state: McpNativeLinkState::Unowned,
        }];
        live.apply(&mut enable).unwrap();
        let enabled: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(enabled["mcp"]["server"]["timeout"], 30);
        assert_eq!(enabled["mcp"]["server"]["enabled"], true);
    }

    #[test]
    fn removable_native_entry_restores_application_owned_fields() {
        let directory = tempdir().unwrap();
        let root = directory.path().join(".gemini");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        fs::write(
            &path,
            r#"{"mcpServers":{"server":{"command":"old","timeout":30,"trust":true}}}"#,
        )
        .unwrap();
        let live = config(directory.path());
        let shared = json!({"type":"stdio","command":"old"});
        let mut disable = [McpLiveChange::Disable {
            app: AppType::Gemini,
            id: "server".to_owned(),
            previous: shared.clone(),
            server: shared.clone(),
            native_snapshot: None,
            link_state: McpNativeLinkState::Unowned,
        }];

        live.apply(&mut disable).unwrap();
        let snapshot = match &disable[0] {
            McpLiveChange::Disable {
                native_snapshot: Some(snapshot),
                ..
            } => snapshot.clone(),
            _ => panic!("Gemini disable must capture its native entry"),
        };
        let disabled: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(disabled["mcpServers"].get("server").is_none());

        let mut enable = [McpLiveChange::Upsert {
            app: AppType::Gemini,
            id: "server".to_owned(),
            previous: Some(shared),
            server: json!({"type":"stdio","command":"new"}),
            native_snapshot: Some(snapshot),
            link_state: McpNativeLinkState::Owned,
        }];
        live.apply(&mut enable).unwrap();
        let enabled: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(enabled["mcpServers"]["server"]["command"], "new");
        assert_eq!(enabled["mcpServers"]["server"]["timeout"], 30);
        assert_eq!(enabled["mcpServers"]["server"]["trust"], true);
    }

    #[test]
    fn owned_delete_rejects_a_diverged_native_entry() {
        let directory = tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".claude")).unwrap();
        let path = directory.path().join(".claude.json");
        let original = r#"{"mcpServers":{"server":{"command":"external"}}}"#;
        fs::write(&path, original).unwrap();

        let mut changes = [McpLiveChange::Remove {
            app: AppType::Claude,
            id: "server".to_owned(),
            server: json!({"type":"stdio","command":"shared"}),
        }];
        let result = config(directory.path()).apply(&mut changes);
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("an enabled link must not silently diverge"),
        };
        assert!(matches!(
            error,
            LiveError::Operation(OperationError::Conflict)
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn import_snapshot_detects_a_concurrent_live_change() {
        let directory = tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".claude")).unwrap();
        let path = directory.path().join(".claude.json");
        fs::write(&path, r#"{"mcpServers":{}}"#).unwrap();
        let live = config(directory.path());
        let snapshot = live.import_snapshot();

        fs::write(&path, r#"{"mcpServers":{"new":{"command":"npx"}}}"#).unwrap();

        assert!(!live.snapshot_is_current(&snapshot));
    }
}
