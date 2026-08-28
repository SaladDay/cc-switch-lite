use std::collections::HashSet;

mod live;
mod native_live;
mod operation;
mod plugin;
mod provider;
mod store;

use live::{LiveConfig, LiveError};
use plugin::{
    InstallSelection, InstalledPlugin, MarketplaceCatalog, PluginCapability, PluginError,
    PluginManager, PluginRequest, PluginResponse, RegistryDraft, RegistrySource,
};
use provider::{
    built_in_adapters, is_lite_writable, native_adapter_reference, AdapterDescriptor,
    AdapterReference, CurrentProvider, ProviderDraft, ProviderRecord, ProviderUpdate,
    BUILTIN_PLUGIN_ID, CONTRACT_MAJOR,
};
use serde::Serialize;
use store::{ProviderStore, StoreError};
use tauri::{Manager, State};

fn lite_apps() -> impl Iterator<Item = cc_switch_core::AppType> {
    cc_switch_core::AppType::all()
}

#[tauri::command]
fn supported_apps() -> Vec<String> {
    lite_apps().map(|app| app.as_str().to_owned()).collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandError {
    code: &'static str,
    message: String,
}

impl From<StoreError> for CommandError {
    fn from(error: StoreError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
        }
    }
}

impl From<LiveError> for CommandError {
    fn from(error: LiveError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
        }
    }
}

impl From<PluginError> for CommandError {
    fn from(error: PluginError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
        }
    }
}

type CommandResult<T> = Result<T, CommandError>;
#[tauri::command]
fn list_provider_adapters(plugins: State<'_, PluginManager>) -> Vec<AdapterDescriptor> {
    plugins.adapters()
}

#[tauri::command]
fn list_providers(
    store: State<'_, ProviderStore>,
    app_id: String,
) -> CommandResult<Vec<ProviderRecord>> {
    let mut providers = store.list(&app_id)?;
    for provider in &mut providers {
        provider.extensions.insert(
            "liteConfigWritable".to_owned(),
            serde_json::Value::Bool(is_lite_writable(provider)),
        );
    }
    Ok(providers)
}

#[tauri::command]
fn create_provider(
    store: State<'_, ProviderStore>,
    plugins: State<'_, PluginManager>,
    provider: ProviderDraft,
) -> CommandResult<ProviderRecord> {
    let app_id = provider.app_id.clone();
    if let Ok(app) = app_id.parse::<cc_switch_core::AppType>() {
        if provider
            .adapter
            .same_identity(&native_adapter_reference(&app))
        {
            return store.create_native(provider).map_err(Into::into);
        }
    }
    let created = store.create_resolved_from(&app_id, false, || {
        let descriptor = plugins
            .adapter_for_reference(&app_id, &provider.adapter)?
            .ok_or(PluginError::NotFound)?;
        plugins.validate_provider(&descriptor, &provider.settings)?;
        Ok::<_, PluginError>((provider, descriptor))
    })??;
    Ok(created)
}

#[tauri::command]
fn update_provider(
    store: State<'_, ProviderStore>,
    plugins: State<'_, PluginManager>,
    app_id: String,
    id: String,
    provider: ProviderUpdate,
) -> CommandResult<ProviderRecord> {
    store
        .update_from(&app_id, &id, provider, |current, update| {
            let descriptor = plugins
                .adapter_for_reference(&current.app_id, &current.adapter)?
                .ok_or(PluginError::NotFound)?;
            let declared = descriptor
                .fields
                .iter()
                .filter_map(|field| {
                    update
                        .settings
                        .get(&field.key)
                        .cloned()
                        .map(|value| (field.key.clone(), value))
                })
                .collect();
            plugins.validate_provider(&descriptor, &declared)?;
            Ok::<_, PluginError>(descriptor)
        })?
        .map_err(Into::into)
}

#[tauri::command]
fn delete_provider(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    app_id: String,
    id: String,
    expected_revision: u64,
) -> CommandResult<()> {
    let app = app_id.parse::<cc_switch_core::AppType>().map_err(|_| {
        CommandError::from(StoreError::InvalidProvider(format!(
            "application '{app_id}' is not supported"
        )))
    })?;
    if app.is_additive_mode() {
        return store.delete_additive_with_provider(
            &app_id,
            &id,
            expected_revision,
            |provider| {
                if provider
                    .adapter
                    .same_identity(&native_adapter_reference(&app))
                {
                    remove_owned_additive_live(provider, |provider| {
                        live.remove_native_recoverable(provider)
                            .map_err(CommandError::from)
                    })
                } else {
                    Ok(None)
                }
            },
            |receipt| match receipt {
                Some(receipt) => live.rollback(receipt).map_err(|error| error.to_string()),
                None => Ok(()),
            },
        )?;
    }
    store
        .delete(&app_id, &id, expected_revision)
        .map_err(Into::into)
}

fn remove_owned_additive_live<T, E>(
    provider: &ProviderRecord,
    remove: impl FnOnce(&ProviderRecord) -> Result<T, E>,
) -> Result<Option<T>, E> {
    let db_only = provider
        .metadata
        .get("liveConfigManaged")
        .and_then(serde_json::Value::as_bool)
        == Some(false);
    if db_only {
        return Ok(None);
    }
    remove(provider).map(Some)
}

#[tauri::command]
fn import_live_providers(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    app_id: String,
) -> CommandResult<Vec<ProviderRecord>> {
    store.import_native_batch_from(&app_id, || {
        live.import_native_drafts(&app_id)
            .map_err(CommandError::from)
    })?
}

#[tauri::command]
fn import_live_provider(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    plugins: State<'_, PluginManager>,
    app_id: String,
    adapter: Option<AdapterReference>,
) -> CommandResult<ProviderRecord> {
    let reference = match adapter {
        Some(reference) => reference,
        None => built_in_adapters()
            .into_iter()
            .find(|candidate| candidate.app_id == app_id)
            .map(|candidate| candidate.reference)
            .ok_or_else(|| {
                CommandError::from(PluginError::Invalid(
                    "application is not available in Lite".to_owned(),
                ))
            })?,
    };
    store.create_resolved_from(&app_id, true, || {
        let descriptor = plugins
            .adapter_for_reference(&app_id, &reference)?
            .ok_or(PluginError::NotFound)?;
        let draft = if reference.plugin_id == BUILTIN_PLUGIN_ID {
            live.import_draft(&app_id).map_err(CommandError::from)?
        } else {
            let capabilities = plugins.capabilities_for_reference(&app_id, &reference)?;
            let response = live
                .with_plugin_snapshots(&app_id, &capabilities, |snapshots| {
                    plugins.invoke(
                        &reference,
                        &PluginRequest::Import {
                            contract_major: CONTRACT_MAJOR,
                            app_id: app_id.clone(),
                            adapter_id: reference.adapter_id.clone(),
                            snapshots,
                        },
                    )
                })
                .map_err(CommandError::from)??;
            match response {
                PluginResponse::Imported { provider } => provider,
                _ => return Err(CommandError::from(PluginError::Runtime)),
            }
        };
        if !draft.adapter.same_identity(&reference) {
            return Err(CommandError::from(PluginError::Invalid(
                "imported provider does not use the requested adapter".to_owned(),
            )));
        }
        let mut draft = draft;
        if reference.plugin_id != BUILTIN_PLUGIN_ID {
            draft.name = format!("Imported {}", descriptor.display_name);
        }
        plugins
            .validate_provider(&descriptor, &draft.settings)
            .map_err(CommandError::from)?;
        Ok::<_, CommandError>((draft, descriptor))
    })?
}

#[tauri::command]
fn switch_provider(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    plugins: State<'_, PluginManager>,
    app_id: String,
    id: String,
    expected_revision: u64,
) -> CommandResult<()> {
    store.switch_with_provider(
        &app_id,
        &id,
        expected_revision,
        |provider, common_snippet| {
            if let Ok(app) = provider.app_id.parse::<cc_switch_core::AppType>() {
                if provider
                    .adapter
                    .same_identity(&native_adapter_reference(&app))
                {
                    return live
                        .switch_native_recoverable(provider, common_snippet)
                        .map_err(CommandError::from);
                }
            }
            if provider.adapter.plugin_id == BUILTIN_PLUGIN_ID {
                return live
                    .switch_recoverable(provider)
                    .map_err(CommandError::from);
            }
            let capabilities =
                plugins.capabilities_for_reference(&provider.app_id, &provider.adapter)?;
            live.execute_plugin_route_recoverable(provider, &capabilities, |snapshots| {
                let response = plugins.invoke(
                    &provider.adapter,
                    &PluginRequest::Plan {
                        contract_major: CONTRACT_MAJOR,
                        provider: Box::new(provider.clone()),
                        snapshots,
                    },
                )?;
                match response {
                    PluginResponse::Routed { route } => Ok(route),
                    _ => Err(PluginError::Runtime),
                }
            })
            .map_err(CommandError::from)?
            .map_err(CommandError::from)
        },
        |receipt| live.rollback(receipt).map_err(|error| error.to_string()),
    )?
}

#[tauri::command]
fn remove_provider_from_live(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    app_id: String,
    id: String,
    expected_revision: u64,
) -> CommandResult<()> {
    store.remove_from_live_with_provider(
        &app_id,
        &id,
        expected_revision,
        |provider| {
            let app = provider
                .app_id
                .parse::<cc_switch_core::AppType>()
                .map_err(|_| {
                    CommandError::from(PluginError::Invalid(
                        "provider application is not supported".to_owned(),
                    ))
                })?;
            if !provider
                .adapter
                .same_identity(&native_adapter_reference(&app))
            {
                return Err(CommandError::from(PluginError::Invalid(
                    "only native additive providers can be removed from live configuration"
                        .to_owned(),
                )));
            }
            live.remove_native_recoverable(provider)
                .map_err(CommandError::from)
        },
        |receipt| live.rollback(receipt).map_err(|error| error.to_string()),
    )?
}

#[tauri::command]
fn current_providers(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    app_id: String,
) -> CommandResult<Vec<CurrentProvider>> {
    let app = app_id.parse::<cc_switch_core::AppType>().map_err(|_| {
        CommandError::from(StoreError::InvalidProvider(format!(
            "application '{app_id}' is not supported"
        )))
    })?;
    if !app.is_additive_mode() {
        return store.current(&app_id).map_err(Into::into);
    }

    let live_ids = match live.import_native_drafts(&app_id) {
        Ok(providers) => providers
            .into_iter()
            .map(|provider| provider.native_id)
            .collect::<HashSet<_>>(),
        Err(LiveError::Missing(_)) => HashSet::new(),
        Err(error) => return Err(error.into()),
    };
    store
        .current_with_native_live_ids(&app_id, &live_ids)
        .map_err(Into::into)
}

#[tauri::command]
fn list_plugin_registries(plugins: State<'_, PluginManager>) -> CommandResult<Vec<RegistrySource>> {
    plugins.registries().map_err(Into::into)
}

#[tauri::command]
fn save_plugin_registry(
    plugins: State<'_, PluginManager>,
    registry: RegistryDraft,
) -> CommandResult<RegistrySource> {
    plugins.save_registry(registry).map_err(Into::into)
}

#[tauri::command]
fn remove_plugin_registry(
    plugins: State<'_, PluginManager>,
    id: String,
    expected_revision: u64,
) -> CommandResult<()> {
    plugins
        .remove_registry(&id, expected_revision)
        .map_err(Into::into)
}

#[tauri::command]
async fn refresh_plugin_marketplace(
    plugins: State<'_, PluginManager>,
) -> CommandResult<MarketplaceCatalog> {
    plugins.refresh().await.map_err(Into::into)
}

#[tauri::command]
fn list_installed_plugins(
    plugins: State<'_, PluginManager>,
) -> CommandResult<Vec<InstalledPlugin>> {
    plugins.installed().map_err(Into::into)
}

#[tauri::command]
async fn install_plugin(
    store: State<'_, ProviderStore>,
    plugins: State<'_, PluginManager>,
    plugin: InstallSelection,
    approved_capabilities: Vec<PluginCapability>,
) -> CommandResult<InstalledPlugin> {
    let prepared = plugins.prepare_install(&plugin).await?;
    store
        .with_all_providers(|providers| {
            plugins.activate(prepared, &approved_capabilities, providers)
        })?
        .map_err(Into::into)
}

#[tauri::command]
fn uninstall_plugin(
    store: State<'_, ProviderStore>,
    plugins: State<'_, PluginManager>,
    plugin_id: String,
) -> CommandResult<()> {
    store
        .with_all_providers(|providers| plugins.remove(&plugin_id, providers))?
        .map_err(Into::into)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            let home_dir = app.path().home_dir()?;
            let store = ProviderStore::from_home(&home_dir)?;
            store.migrate_legacy(&app_data_dir.join("providers.json"))?;
            app.manage(store);
            app.manage(PluginManager::new(app_data_dir.join("plugins"))?);
            app.manage(LiveConfig::from_home(
                &home_dir,
                app_data_dir.join("live-config.lock"),
            )?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            supported_apps,
            list_provider_adapters,
            list_providers,
            create_provider,
            update_provider,
            delete_provider,
            import_live_provider,
            import_live_providers,
            switch_provider,
            remove_provider_from_live,
            current_providers,
            list_plugin_registries,
            save_plugin_registry,
            remove_plugin_registry,
            refresh_plugin_marketplace,
            list_installed_plugins,
            install_plugin,
            uninstall_plugin,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CC Switch Lite");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map};

    #[test]
    fn lite_boundary_follows_every_core_application() {
        assert_eq!(
            supported_apps(),
            [
                "claude",
                "claude-desktop",
                "codex",
                "gemini",
                "grokbuild",
                "opencode",
                "openclaw",
                "hermes",
                "pi",
            ]
        );
    }

    #[test]
    fn built_in_adapters_remain_explicit_live_capabilities() {
        let adapters = built_in_adapters();
        let app_ids: Vec<_> = adapters
            .iter()
            .map(|adapter| adapter.app_id.as_str())
            .collect();

        assert_eq!(app_ids, ["claude", "codex"]);
        assert!(adapters
            .iter()
            .all(|adapter| adapter.reference.plugin_id == "org.cc-switch.builtin"));
    }

    #[test]
    fn db_only_additive_providers_do_not_own_same_id_live_entries() {
        use std::cell::Cell;

        let app = cc_switch_core::AppType::OpenCode;
        let mut provider = ProviderRecord {
            id: "external-id".to_owned(),
            revision: 1,
            app_id: app.as_str().to_owned(),
            adapter: native_adapter_reference(&app),
            name: "DB only".to_owned(),
            settings: Map::new(),
            category: None,
            metadata: json!({"liveConfigManaged": false}),
            extensions: Map::new(),
        };

        let called = Cell::new(false);
        let result = remove_owned_additive_live(&provider, |_| {
            called.set(true);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert!(result.is_none());
        assert!(!called.get());

        provider.metadata = json!({"liveConfigManaged": true});
        assert_eq!(
            remove_owned_additive_live(&provider, |_| Ok::<_, ()>(())).unwrap(),
            Some(())
        );
        provider.metadata = json!({});
        assert_eq!(
            remove_owned_additive_live(&provider, |_| Ok::<_, ()>(())).unwrap(),
            Some(())
        );
    }
}
