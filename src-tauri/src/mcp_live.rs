use std::{fs, path::PathBuf};

use cc_switch_core::{
    builtin_app_adapter, builtin_app_registry, fs::atomic_write, mcp_servers_equivalent,
    AppCapability, AppType, McpConfigTarget, McpServerProjection,
};

use crate::{
    live::LiveError,
    mcp::{McpImportsByApp, McpLiveChange},
    operation::{read_optional, resolve_write_path, OperationError},
};

struct McpPath {
    target: McpConfigTarget,
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
        claude: (PathBuf, PathBuf),
        codex: (PathBuf, PathBuf),
        gemini: (PathBuf, PathBuf),
        grok: (PathBuf, PathBuf),
        opencode: (PathBuf, PathBuf),
        hermes: (PathBuf, PathBuf),
    ) -> Self {
        Self {
            paths: vec![
                McpPath {
                    target: McpConfigTarget::Claude,
                    path: claude.0,
                    install_marker: claude.1,
                },
                McpPath {
                    target: McpConfigTarget::Codex,
                    path: codex.0,
                    install_marker: codex.1,
                },
                McpPath {
                    target: McpConfigTarget::Gemini,
                    path: gemini.0,
                    install_marker: gemini.1,
                },
                McpPath {
                    target: McpConfigTarget::GrokBuild,
                    path: grok.0,
                    install_marker: grok.1,
                },
                McpPath {
                    target: McpConfigTarget::OpenCode,
                    path: opencode.0,
                    install_marker: opencode.1,
                },
                McpPath {
                    target: McpConfigTarget::Hermes,
                    path: hermes.0,
                    install_marker: hermes.1,
                },
            ],
        }
    }

    pub fn apply(&self, changes: &[McpLiveChange]) -> Result<McpLiveReceipt, LiveError> {
        let mut receipt = McpLiveReceipt { writes: Vec::new() };
        for change in changes {
            if let Err(error) = self.apply_one(change, &mut receipt) {
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
        change: &McpLiveChange,
        receipt: &mut McpLiveReceipt,
    ) -> Result<(), LiveError> {
        let configured = self.path_for_app(change.app())?;
        if !configured.path.exists() && !configured.install_marker.exists() {
            return Ok(());
        }
        let path = resolve_write_path(&configured.path)?;
        let before = read_optional(&path)?;
        let adapter = builtin_app_adapter(change.app());
        let imports = match adapter.import_mcp_servers(before.as_deref()) {
            Ok(imports) => imports,
            Err(_) if !change.is_strict() => return Ok(()),
            Err(error) => return Err(LiveError::InvalidConfig(error.to_string())),
        };
        let current = imports.iter().find(|entry| entry.id == change.id());
        let (id, projection) = match change {
            McpLiveChange::Upsert {
                id,
                previous,
                server,
                ..
            } => {
                if current.is_some_and(|current| {
                    !mcp_servers_equivalent(change.app(), &current.server, server)
                        && previous.as_ref().is_none_or(|previous| {
                            !mcp_servers_equivalent(change.app(), &current.server, previous)
                        })
                }) {
                    return Err(OperationError::Conflict.into());
                }
                (id.as_str(), McpServerProjection::Enable(server))
            }
            McpLiveChange::Disable {
                id,
                previous,
                server,
                strict,
                ..
            } => {
                let Some(current) = current else {
                    return Ok(());
                };
                if !mcp_servers_equivalent(change.app(), &current.server, previous)
                    && !mcp_servers_equivalent(change.app(), &current.server, server)
                {
                    if *strict {
                        return Err(OperationError::Conflict.into());
                    }
                    return Ok(());
                }
                (id.as_str(), McpServerProjection::Disable(server))
            }
            McpLiveChange::Remove {
                id, server, strict, ..
            } => {
                let Some(current) = current else {
                    return Ok(());
                };
                if !mcp_servers_equivalent(change.app(), &current.server, server) {
                    if *strict {
                        return Err(OperationError::Conflict.into());
                    }
                    return Ok(());
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
        let target = builtin_app_adapter(app)
            .mcp_config_target()
            .ok_or_else(|| {
                LiveError::InvalidConfig(format!(
                    "application '{}' does not support MCP",
                    app.as_str()
                ))
            })?;
        self.paths
            .iter()
            .find(|path| path.target == target)
            .ok_or_else(|| LiveError::InvalidConfig("MCP path is not configured".to_owned()))
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
    use serde_json::{json, Value};
    use std::path::Path;
    use tempfile::tempdir;

    fn config(home: &Path) -> McpLiveConfig {
        McpLiveConfig::new(
            (home.join(".claude.json"), home.join(".claude")),
            (home.join(".codex/config.toml"), home.join(".codex")),
            (home.join(".gemini/settings.json"), home.join(".gemini")),
            (home.join(".grok/config.toml"), home.join(".grok")),
            (
                home.join(".config/opencode/opencode.json"),
                home.join(".config/opencode"),
            ),
            (home.join(".hermes/config.yaml"), home.join(".hermes")),
        )
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
        let changes = [
            McpLiveChange::Upsert {
                app: AppType::Claude,
                id: "server".to_owned(),
                previous: None,
                server: json!({"type":"stdio","command":"npx"}),
            },
            McpLiveChange::Upsert {
                app: AppType::Codex,
                id: "server".to_owned(),
                previous: None,
                server: json!({"type":"stdio","command":"npx"}),
            },
        ];
        let receipt = live.apply(&changes).unwrap();
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
    fn skips_writes_for_apps_that_are_not_installed() {
        let directory = tempdir().unwrap();
        let live = config(directory.path());
        let receipt = live
            .apply(&[McpLiveChange::Upsert {
                app: AppType::Gemini,
                id: "server".to_owned(),
                previous: None,
                server: json!({"type":"stdio","command":"npx"}),
            }])
            .unwrap();
        assert!(receipt.writes.is_empty());
        assert!(!directory.path().join(".gemini/settings.json").exists());
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

        live.apply(&[McpLiveChange::Disable {
            app: AppType::OpenCode,
            id: "server".to_owned(),
            previous,
            server: updated.clone(),
            strict: true,
        }])
        .unwrap();
        let disabled: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(disabled["mcp"]["server"]["command"], json!(["new"]));
        assert_eq!(disabled["mcp"]["server"]["timeout"], 30);
        assert_eq!(disabled["mcp"]["server"]["enabled"], false);

        live.apply(&[McpLiveChange::Upsert {
            app: AppType::OpenCode,
            id: "server".to_owned(),
            previous: Some(updated.clone()),
            server: updated,
        }])
        .unwrap();
        let enabled: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(enabled["mcp"]["server"]["timeout"], 30);
        assert_eq!(enabled["mcp"]["server"]["enabled"], true);
    }

    #[test]
    fn catalog_delete_does_not_remove_a_conflicting_native_entry() {
        let directory = tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".claude")).unwrap();
        let path = directory.path().join(".claude.json");
        let original = r#"{"mcpServers":{"server":{"command":"external"}}}"#;
        fs::write(&path, original).unwrap();

        let receipt = config(directory.path())
            .apply(&[McpLiveChange::Remove {
                app: AppType::Claude,
                id: "server".to_owned(),
                server: json!({"type":"stdio","command":"shared"}),
                strict: false,
            }])
            .unwrap();

        assert!(receipt.writes.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        let result = config(directory.path()).apply(&[McpLiveChange::Remove {
            app: AppType::Claude,
            id: "server".to_owned(),
            server: json!({"type":"stdio","command":"shared"}),
            strict: true,
        }]);
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
