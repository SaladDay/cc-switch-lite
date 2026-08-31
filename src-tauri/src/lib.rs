use std::collections::HashSet;

use cc_switch_core::{builtin_app_registry, AppCapability, AppDescriptor};

mod live;
mod native_live;
mod operation;
mod provider;
mod store;

use live::{LiveConfig, LiveError};
use provider::{
    adapter_for_reference, built_in_adapters, is_lite_writable, native_adapter_reference,
    native_adapters, validate_settings, AdapterDescriptor, CurrentProvider, ProviderDraft,
    ProviderRecord, ProviderUpdate,
};
use serde::Serialize;
use store::{ProviderStore, StoreError};
use tauri::{Manager, State};

#[tauri::command]
fn supported_apps() -> Vec<AppDescriptor> {
    builtin_app_registry().descriptors().cloned().collect()
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

type CommandResult<T> = Result<T, CommandError>;

fn unavailable_adapter() -> StoreError {
    StoreError::InvalidProvider("provider adapter is not available in Lite".to_owned())
}

fn require_capability(
    app_id: &str,
    capability: AppCapability,
    operation: &str,
) -> CommandResult<()> {
    if supports_capability(app_id, capability) {
        return Ok(());
    }

    Err(StoreError::InvalidProvider(format!(
        "application '{app_id}' does not support {operation}"
    ))
    .into())
}

fn supports_capability(app_id: &str, capability: AppCapability) -> bool {
    builtin_app_registry()
        .find(app_id)
        .is_some_and(|descriptor| descriptor.supports(capability))
}

fn require_provider_visibility(app_id: &str) -> CommandResult<()> {
    if supports_capability(app_id, AppCapability::ProviderManagement)
        || supports_capability(app_id, AppCapability::LiveConfiguration)
    {
        return Ok(());
    }

    Err(
        StoreError::InvalidProvider(format!("application '{app_id}' does not expose providers"))
            .into(),
    )
}

#[tauri::command]
fn list_provider_adapters() -> Vec<AdapterDescriptor> {
    let mut adapters = native_adapters();
    adapters.extend(built_in_adapters());
    adapters
}

#[tauri::command]
fn list_providers(
    store: State<'_, ProviderStore>,
    app_id: String,
) -> CommandResult<Vec<ProviderRecord>> {
    require_provider_visibility(&app_id)?;
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
    provider: ProviderDraft,
) -> CommandResult<ProviderRecord> {
    let app_id = provider.app_id.clone();
    require_capability(
        &app_id,
        AppCapability::ProviderManagement,
        "provider management",
    )?;
    if let Ok(app) = app_id.parse::<cc_switch_core::AppType>() {
        if provider
            .adapter
            .same_identity(&native_adapter_reference(&app))
        {
            return store.create_native(provider).map_err(Into::into);
        }
    }

    Ok(store.create_resolved_from(&app_id, false, || {
        let descriptor =
            adapter_for_reference(&app_id, &provider.adapter).ok_or_else(unavailable_adapter)?;
        validate_settings(&descriptor, &provider.settings).map_err(StoreError::InvalidProvider)?;
        Ok::<_, StoreError>((provider, descriptor))
    })??)
}

#[tauri::command]
fn update_provider(
    store: State<'_, ProviderStore>,
    app_id: String,
    id: String,
    provider: ProviderUpdate,
) -> CommandResult<ProviderRecord> {
    require_capability(
        &app_id,
        AppCapability::ProviderManagement,
        "provider management",
    )?;
    Ok(store.update_from(&app_id, &id, provider, |current, _| {
        adapter_for_reference(&current.app_id, &current.adapter).ok_or_else(unavailable_adapter)
    })??)
}

#[tauri::command]
fn delete_provider(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    app_id: String,
    id: String,
    expected_revision: u64,
) -> CommandResult<()> {
    require_capability(
        &app_id,
        AppCapability::ProviderManagement,
        "provider management",
    )?;
    let app = app_id.parse::<cc_switch_core::AppType>().map_err(|_| {
        CommandError::from(StoreError::InvalidProvider(format!(
            "application '{app_id}' is not supported"
        )))
    })?;
    if app.is_additive_mode() && supports_capability(&app_id, AppCapability::LiveConfiguration) {
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
    require_capability(
        &app_id,
        AppCapability::LiveConfiguration,
        "live configuration",
    )?;
    store.import_native_batch_from(&app_id, || {
        live.import_native_drafts(&app_id)
            .map_err(CommandError::from)
    })?
}

#[tauri::command]
fn switch_provider(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    app_id: String,
    id: String,
    expected_revision: u64,
) -> CommandResult<()> {
    require_capability(
        &app_id,
        AppCapability::LiveConfiguration,
        "live configuration",
    )?;
    store.switch_with_provider(
        &app_id,
        &id,
        expected_revision,
        |provider, common_snippet| {
            let app = provider
                .app_id
                .parse::<cc_switch_core::AppType>()
                .map_err(|_| {
                    CommandError::from(StoreError::InvalidProvider(
                        "application is not supported".to_owned(),
                    ))
                })?;
            if !provider
                .adapter
                .same_identity(&native_adapter_reference(&app))
            {
                return Err(CommandError::from(unavailable_adapter()));
            }
            live.switch_native_recoverable(provider, common_snippet)
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
    require_capability(
        &app_id,
        AppCapability::LiveConfiguration,
        "live configuration",
    )?;
    store.remove_from_live_with_provider(
        &app_id,
        &id,
        expected_revision,
        |provider| {
            let app = provider
                .app_id
                .parse::<cc_switch_core::AppType>()
                .map_err(|_| {
                    CommandError::from(StoreError::InvalidProvider(
                        "provider application is not supported".to_owned(),
                    ))
                })?;
            if !provider
                .adapter
                .same_identity(&native_adapter_reference(&app))
            {
                return Err(CommandError::from(StoreError::InvalidProvider(
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
        if !supports_capability(&app_id, AppCapability::ProviderManagement)
            && !supports_capability(&app_id, AppCapability::LiveConfiguration)
        {
            return Err(StoreError::InvalidProvider(format!(
                "application '{app_id}' does not expose provider state"
            ))
            .into());
        }
        return store.current(&app_id).map_err(Into::into);
    }

    if !supports_capability(&app_id, AppCapability::LiveConfiguration) {
        require_capability(&app_id, AppCapability::ProviderManagement, "provider state")?;
        return Ok(Vec::new());
    }

    require_capability(
        &app_id,
        AppCapability::LiveConfiguration,
        "live configuration",
    )?;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            let home_dir = app.path().home_dir()?;
            let store = ProviderStore::from_home(&home_dir)?;
            // The shared database is authoritative; startup never imports old Lite files.
            app.manage(store);
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
            import_live_providers,
            switch_provider,
            remove_provider_from_live,
            current_providers,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CC Switch Lite");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map};

    #[test]
    fn host_adapters_cover_every_core_application() {
        let app_ids: Vec<_> = native_adapters()
            .into_iter()
            .map(|adapter| adapter.app_id)
            .collect();
        let core_ids: Vec<_> = builtin_app_registry()
            .descriptors()
            .map(|descriptor| descriptor.id().to_owned())
            .collect();

        assert_eq!(app_ids, core_ids);
    }

    #[test]
    fn built_in_form_adapters_remain_explicit() {
        let adapters = built_in_adapters();
        let app_ids: Vec<_> = adapters
            .iter()
            .map(|adapter| adapter.app_id.as_str())
            .collect();

        assert_eq!(app_ids, ["claude", "codex"]);
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
        .expect("remove decision");
        assert!(result.is_none());
        assert!(!called.get());

        provider.metadata = json!({"liveConfigManaged": true});
        assert_eq!(
            remove_owned_additive_live(&provider, |_| Ok::<_, ()>(())).expect("managed provider"),
            Some(())
        );
        provider.metadata = json!({});
        assert_eq!(
            remove_owned_additive_live(&provider, |_| Ok::<_, ()>(()))
                .expect("legacy managed provider"),
            Some(())
        );
    }
}
