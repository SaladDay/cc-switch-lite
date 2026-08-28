mod live;
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
    built_in_adapters, native_adapter_reference, AdapterDescriptor, AdapterReference,
    CurrentProvider, ProviderDraft, ProviderRecord, ProviderUpdate, BUILTIN_PLUGIN_ID,
    CONTRACT_MAJOR,
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
    store.list(&app_id).map_err(Into::into)
}

#[tauri::command]
fn create_provider(
    store: State<'_, ProviderStore>,
    plugins: State<'_, PluginManager>,
    provider: ProviderDraft,
) -> CommandResult<ProviderRecord> {
    let app_id = provider.app_id.clone();
    if let Ok(app) = app_id.parse::<cc_switch_core::AppType>() {
        if provider.adapter == native_adapter_reference(&app) {
            return store.create_native(provider).map_err(Into::into);
        }
    }
    let created = store.create_resolved_from(&app_id, || {
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
            plugins.validate_provider(&descriptor, &update.settings)?;
            Ok::<_, PluginError>(descriptor)
        })?
        .map_err(Into::into)
}

#[tauri::command]
fn delete_provider(
    store: State<'_, ProviderStore>,
    app_id: String,
    id: String,
    expected_revision: u64,
) -> CommandResult<()> {
    store
        .delete(&app_id, &id, expected_revision)
        .map_err(Into::into)
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
    let imported = store.create_resolved_from(&app_id, || {
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
        if draft.adapter != reference {
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
    })??;
    if app_id
        .parse::<cc_switch_core::AppType>()
        .is_ok_and(|app| !app.is_additive_mode())
    {
        return store
            .set_current(&app_id, &imported.id, imported.revision)
            .map_err(Into::into);
    }
    Ok(imported)
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
    let switched = store.with_provider(&app_id, &id, expected_revision, |provider| {
        if provider.adapter.plugin_id == BUILTIN_PLUGIN_ID {
            return live.switch(provider).map_err(CommandError::from);
        }
        let capabilities =
            plugins.capabilities_for_reference(&provider.app_id, &provider.adapter)?;
        live.execute_plugin_route(provider, &capabilities, |snapshots| {
            let response = plugins.invoke(
                &provider.adapter,
                &PluginRequest::Plan {
                    contract_major: CONTRACT_MAJOR,
                    provider: provider.clone(),
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
    })?;
    switched?;
    if app_id
        .parse::<cc_switch_core::AppType>()
        .is_ok_and(|app| !app.is_additive_mode())
    {
        store.set_current(&app_id, &id, expected_revision)?;
    }
    Ok(())
}

#[tauri::command]
fn current_providers(
    store: State<'_, ProviderStore>,
    app_id: String,
) -> CommandResult<Vec<CurrentProvider>> {
    store.current(&app_id).map_err(Into::into)
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
            switch_provider,
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
}
