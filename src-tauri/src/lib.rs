use std::collections::HashSet;

use cc_switch_core::{
    builtin_app_adapter, builtin_app_registry, builtin_simple_provider_forms, AppCapability,
    AppDescriptor, SimpleProviderFormDescriptor,
};

mod live;
mod mcp;
mod mcp_live;
mod native_live;
mod operation;
mod provider;
mod skill;
mod skill_live;
mod store;

use live::{LiveConfig, LiveError};
use mcp::{McpError, McpImportReport, McpServer, McpStore};
use provider::{
    built_in_adapters, is_lite_simple_editable, is_lite_writable, native_adapter_reference,
    native_adapters, validate_simple_provider_values, AdapterDescriptor, CurrentProvider,
    ProviderDraft, ProviderRecord, SimpleProviderDraft, SimpleProviderUpdate,
};
use serde::Serialize;
use skill::{SkillError, SkillRecord, SkillStore};
use store::{ProviderStore, StoreError};
use tauri::{Manager, State};

#[tauri::command]
fn supported_apps() -> Vec<AppDescriptor> {
    builtin_app_registry().descriptors().cloned().collect()
}

#[tauri::command]
fn list_simple_provider_forms() -> Vec<SimpleProviderFormDescriptor> {
    builtin_simple_provider_forms().cloned().collect()
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

impl From<McpError> for CommandError {
    fn from(error: McpError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
        }
    }
}

impl From<SkillError> for CommandError {
    fn from(error: SkillError) -> Self {
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

fn decorate_provider(provider: &mut ProviderRecord) {
    let writable = provider
        .extensions
        .get("liteConfigWritable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or_else(|| is_lite_writable(provider));
    let simple_editable = provider
        .extensions
        .get("liteSimpleEditable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or_else(|| is_lite_simple_editable(provider));
    provider.extensions.insert(
        "liteConfigWritable".to_owned(),
        serde_json::Value::Bool(writable),
    );
    provider.extensions.insert(
        "liteSimpleEditable".to_owned(),
        serde_json::Value::Bool(simple_editable),
    );
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
        decorate_provider(provider);
    }
    Ok(providers)
}

#[tauri::command]
fn create_simple_provider(
    store: State<'_, ProviderStore>,
    provider: SimpleProviderDraft,
) -> CommandResult<ProviderRecord> {
    require_capability(
        &provider.app_id,
        AppCapability::ProviderManagement,
        "provider management",
    )?;
    let app = provider
        .app_id
        .parse::<cc_switch_core::AppType>()
        .map_err(|_| {
            CommandError::from(StoreError::InvalidProvider(format!(
                "application '{}' is not supported",
                provider.app_id
            )))
        })?;
    let adapter = builtin_app_adapter(&app);
    validate_simple_provider_values(adapter.simple_provider_form(), &provider.values)
        .map_err(StoreError::InvalidProvider)?;
    let settings = adapter
        .project_simple_provider_settings(&provider.name, &provider.values, None)
        .map_err(|error| StoreError::InvalidProvider(error.to_string()))?;
    let settings = settings.as_object().cloned().ok_or_else(|| {
        StoreError::InvalidProvider(
            "simple provider projection must produce native object settings".to_owned(),
        )
    })?;
    let mut created = store
        .create_native(ProviderDraft {
            app_id: provider.app_id,
            adapter: native_adapter_reference(&app),
            name: provider.name,
            settings,
        })
        .map_err(CommandError::from)?;
    decorate_provider(&mut created);
    Ok(created)
}

#[tauri::command]
fn update_simple_provider(
    store: State<'_, ProviderStore>,
    app_id: String,
    id: String,
    provider: SimpleProviderUpdate,
) -> CommandResult<ProviderRecord> {
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
    let adapter = builtin_app_adapter(&app);
    validate_simple_provider_values(adapter.simple_provider_form(), &provider.values)
        .map_err(StoreError::InvalidProvider)?;
    let mut updated = store.update_simple_from(
        &app_id,
        &id,
        provider.expected_revision,
        &provider.name,
        |stored_app, existing| {
            builtin_app_adapter(stored_app)
                .project_simple_provider_settings(&provider.name, &provider.values, Some(existing))
                .map_err(|error| StoreError::InvalidProvider(error.to_string()))
        },
    )??;
    decorate_provider(&mut updated);
    Ok(updated)
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

#[tauri::command]
fn list_mcp_servers(store: State<'_, McpStore>) -> CommandResult<Vec<McpServer>> {
    store.list().map_err(Into::into)
}

#[tauri::command]
fn upsert_mcp_server(
    store: State<'_, McpStore>,
    live: State<'_, LiveConfig>,
    server: McpServer,
) -> CommandResult<()> {
    match store
        .upsert_with_live(
            server,
            |changes| live.apply_mcp_recoverable(changes),
            |receipt| {
                live.rollback_mcp(receipt)
                    .map_err(|error| error.to_string())
            },
        )
        .map_err(CommandError::from)?
    {
        Ok(()) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[tauri::command]
fn toggle_mcp_app(
    store: State<'_, McpStore>,
    live: State<'_, LiveConfig>,
    server_id: String,
    app_id: String,
    enabled: bool,
    expected_revision: u64,
) -> CommandResult<()> {
    let app = app_id.parse::<cc_switch_core::AppType>().map_err(|_| {
        CommandError::from(McpError::InvalidServer(format!(
            "application '{app_id}' is not supported"
        )))
    })?;
    match store
        .toggle_with_live(
            &server_id,
            expected_revision,
            app,
            enabled,
            |changes| live.apply_mcp_recoverable(changes),
            |receipt| {
                live.rollback_mcp(receipt)
                    .map_err(|error| error.to_string())
            },
        )
        .map_err(CommandError::from)?
    {
        Ok(()) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[tauri::command]
fn delete_mcp_server(
    store: State<'_, McpStore>,
    live: State<'_, LiveConfig>,
    id: String,
    expected_revision: u64,
) -> CommandResult<()> {
    match store
        .delete_with_live(
            &id,
            expected_revision,
            |changes| live.apply_mcp_recoverable(changes),
            |receipt| {
                live.rollback_mcp(receipt)
                    .map_err(|error| error.to_string())
            },
        )
        .map_err(CommandError::from)?
    {
        Ok(()) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[tauri::command]
fn import_mcp_from_apps(
    store: State<'_, McpStore>,
    live: State<'_, LiveConfig>,
) -> CommandResult<McpImportReport> {
    match store
        .import_with_live(
            || live.observe_mcp(),
            |observation| live.mcp_observation_is_current(observation),
        )
        .map_err(CommandError::from)?
    {
        Ok(report) => Ok(report),
        Err(error) => Err(error.into()),
    }
}

#[tauri::command]
fn list_installed_skills(
    store: State<'_, SkillStore>,
    live: State<'_, LiveConfig>,
) -> CommandResult<Vec<SkillRecord>> {
    let mut skills = store.list()?;
    let requests = skills
        .iter()
        .map(|skill| (skill.id.clone(), skill.directory.clone()))
        .collect::<Vec<_>>();
    let observations = live.observe_skills(&requests)?;
    let indexes = skills
        .iter()
        .enumerate()
        .map(|(index, skill)| (skill.id.clone(), index))
        .collect::<std::collections::HashMap<_, _>>();
    for observation in observations {
        let Some(skill) = indexes
            .get(&observation.id)
            .and_then(|index| skills.get_mut(*index))
        else {
            continue;
        };
        if skill.issue.is_none() {
            skill.issue = observation.source_issue;
        }
        for (app_id, state) in observation.app_overrides {
            if let Some(enabled) = state.enabled {
                skill.apps.insert(app_id.clone(), enabled);
            }
            if let Some(issue) = state.issue {
                skill.app_issues.insert(app_id, issue);
            } else {
                skill.app_issues.remove(&app_id);
            }
        }
    }
    Ok(skills)
}

#[tauri::command]
fn toggle_skill_app(
    store: State<'_, SkillStore>,
    live: State<'_, LiveConfig>,
    skill_id: String,
    app_id: String,
    enabled: bool,
) -> CommandResult<()> {
    let app = app_id.parse::<cc_switch_core::AppType>().map_err(|_| {
        CommandError::from(SkillError::InvalidSkill(format!(
            "application '{app_id}' is not supported"
        )))
    })?;
    match store
        .toggle_with_live(&skill_id, app, enabled, |directory, app, enabled| {
            live.apply_skill_recoverable(directory, app, enabled)
        })
        .map_err(CommandError::from)?
    {
        Ok(()) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let home_dir = app.path().home_dir()?;
            let store = ProviderStore::from_home(&home_dir)?;
            let mcp_store = McpStore::open(store::database_path(&home_dir))?;
            let skill_store = SkillStore::open(store::database_path(&home_dir))?;
            // The shared database is authoritative; startup never imports old Lite files.
            app.manage(store);
            app.manage(mcp_store);
            app.manage(skill_store);
            app.manage(LiveConfig::from_home(&home_dir)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            supported_apps,
            list_simple_provider_forms,
            list_provider_adapters,
            list_providers,
            create_simple_provider,
            update_simple_provider,
            delete_provider,
            import_live_providers,
            switch_provider,
            remove_provider_from_live,
            current_providers,
            list_mcp_servers,
            upsert_mcp_server,
            toggle_mcp_app,
            delete_mcp_server,
            import_mcp_from_apps,
            list_installed_skills,
            toggle_skill_app,
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
    fn provider_decoration_exposes_only_writable_simple_records() {
        let app = cc_switch_core::AppType::Claude;
        let mut provider = ProviderRecord {
            id: "provider".to_owned(),
            revision: 1,
            app_id: app.as_str().to_owned(),
            adapter: native_adapter_reference(&app),
            name: "Provider".to_owned(),
            settings: Map::new(),
            category: None,
            metadata: json!({}),
            extensions: Map::from_iter([("simpleValues".to_owned(), json!({}))]),
        };

        decorate_provider(&mut provider);
        assert_eq!(provider.extensions["liteConfigWritable"], true);
        assert_eq!(provider.extensions["liteSimpleEditable"], true);

        provider.category = Some("official".to_owned());
        provider.extensions.remove("liteConfigWritable");
        provider.extensions.remove("liteSimpleEditable");
        decorate_provider(&mut provider);
        assert_eq!(provider.extensions["liteConfigWritable"], true);
        assert_eq!(provider.extensions["liteSimpleEditable"], false);
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
