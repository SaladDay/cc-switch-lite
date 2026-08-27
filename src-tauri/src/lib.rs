mod live;
mod operation;
mod provider;
mod store;

use live::{LiveConfig, LiveError};
use provider::{
    built_in_adapters, AdapterDescriptor, CurrentProvider, ProviderDraft, ProviderRecord,
    ProviderUpdate,
};
use serde::Serialize;
use store::{ProviderStore, StoreError};
use tauri::{Manager, State};

fn lite_apps() -> [cc_switch_core::AppType; 2] {
    [
        cc_switch_core::AppType::Claude,
        cc_switch_core::AppType::Codex,
    ]
}

#[tauri::command]
fn supported_apps() -> Vec<String> {
    lite_apps()
        .into_iter()
        .map(|app| app.as_str().to_owned())
        .collect()
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

#[tauri::command]
fn list_provider_adapters() -> Vec<AdapterDescriptor> {
    built_in_adapters()
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
    provider: ProviderDraft,
) -> CommandResult<ProviderRecord> {
    store.create(provider).map_err(Into::into)
}

#[tauri::command]
fn update_provider(
    store: State<'_, ProviderStore>,
    id: String,
    provider: ProviderUpdate,
) -> CommandResult<ProviderRecord> {
    store.update(&id, provider).map_err(Into::into)
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
    app_id: String,
) -> CommandResult<ProviderRecord> {
    store
        .create_from(&app_id, || live.import_draft(&app_id))?
        .map_err(Into::into)
}

#[tauri::command]
fn switch_provider(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    app_id: String,
    id: String,
    expected_revision: u64,
) -> CommandResult<()> {
    store
        .with_provider(&app_id, &id, expected_revision, |provider| {
            live.switch(provider)
        })?
        .map_err(Into::into)
}

#[tauri::command]
fn current_providers(
    store: State<'_, ProviderStore>,
    live: State<'_, LiveConfig>,
    app_id: String,
) -> CommandResult<Vec<CurrentProvider>> {
    store
        .with_providers(&app_id, |providers| {
            live.current_providers(&app_id, providers)
        })?
        .map_err(Into::into)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            let store_path = app_data_dir.join("providers.json");
            let home_dir = app.path().home_dir()?;
            app.manage(ProviderStore::new(store_path));
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
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CC Switch Lite");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lite_boundary_contains_only_claude_and_codex() {
        assert_eq!(supported_apps(), ["claude", "codex"]);
    }

    #[test]
    fn built_in_adapters_follow_the_core_application_boundary() {
        let adapters = list_provider_adapters();
        let app_ids: Vec<_> = adapters
            .iter()
            .map(|adapter| adapter.app_id.as_str())
            .collect();

        assert_eq!(app_ids, supported_apps());
        assert!(adapters
            .iter()
            .all(|adapter| adapter.reference.plugin_id == "org.cc-switch.builtin"));
    }
}
