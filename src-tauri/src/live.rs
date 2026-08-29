use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::Mutex,
};

use cc_switch_core::fs::atomic_write_private;
use fs4::{FileExt, TryLockError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use toml_edit::{value, DocumentMut, Item, Table};

use crate::{
    native_live::NativeLiveConfig,
    operation::{
        read_optional, read_optional_no_follow, sha256, ContentExpectation, LivePaths,
        LogicalTarget, OperationError, OperationExecutor, OperationPlan, OperationReceipt,
        PlannedWrite, OPERATION_CONTRACT_MAJOR,
    },
    plugin::{PluginCapability, PluginRoute, PluginSlot, PluginSnapshot},
    provider::{
        adapter_for_reference, built_in_adapters, validate_settings, NativeImport, ProviderDraft,
        ProviderRecord,
    },
};

const LITE_CODEX_PROVIDER_PREFIX: &str = "cc-switch-lite-";
const DEFAULT_CODEX_BASE_URL: &str = "https://api.openai.com/v1";
const OWNERSHIP_VERSION: u32 = 1;
const RESERVED_CODEX_PROVIDER_IDS: [&str; 3] = ["openai", "ollama", "lmstudio"];
const CLAUDE_ROOT_CONFLICT_KEYS: [&str; 3] = ["model", "fallbackModel", "modelOverrides"];
const CLAUDE_ENV_CONFLICT_KEYS: [&str; 18] = [
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_CUSTOM_HEADERS",
    "ANTHROPIC_DEFAULT_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "CLAUDE_CODE_CLIENT_CERT",
    "CLAUDE_CODE_CLIENT_KEY",
    "CLAUDE_CODE_CLIENT_KEY_PASSPHRASE",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
];
const MAX_PLUGIN_SNAPSHOT_BYTES: usize = 256 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CodexRouteOwnership {
    version: u32,
    installation_id: String,
}

impl CodexRouteOwnership {
    fn new() -> Self {
        Self {
            version: OWNERSHIP_VERSION,
            installation_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

#[derive(Debug, Error)]
pub enum LiveError {
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error("live configuration is missing for {0}")]
    Missing(String),
    #[error("live configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("live configuration cannot be represented by the Lite adapter: {0}")]
    UnsupportedConfig(String),
    #[error("provider cannot be switched: {0}")]
    InvalidProvider(String),
    #[error("live configuration lock is unavailable")]
    LockUnavailable,
    #[error("live configuration I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl LiveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Operation(error) => error.code(),
            Self::Missing(_) => "live_missing",
            Self::InvalidConfig(_) => "invalid_live_config",
            Self::UnsupportedConfig(_) => "unsupported_live_config",
            Self::InvalidProvider(_) => "invalid_provider",
            Self::LockUnavailable => "lock_unavailable",
            Self::Io { .. } => "live_io_error",
        }
    }
}

pub struct LiveConfig {
    #[cfg(test)]
    claude_dir: PathBuf,
    native: NativeLiveConfig,
    lock_path: PathBuf,
    ownership_path: PathBuf,
    gate: Mutex<()>,
}

pub struct LiveWriteReceipt {
    operation: OperationReceipt,
    ownership: Option<OwnershipWriteReceipt>,
}

struct OwnershipWriteReceipt {
    written: Vec<u8>,
}

impl LiveConfig {
    pub fn from_home(home: &Path, lock_path: PathBuf) -> Result<Self, LiveError> {
        let claude_override = std::env::var_os("CLAUDE_CONFIG_DIR");
        let codex_override = std::env::var_os("CODEX_HOME");
        let claude_dir = config_root(
            claude_override.as_deref(),
            &home.join(".claude"),
            "CLAUDE_CONFIG_DIR",
        )?;
        let codex_dir = config_root(
            codex_override.as_deref(),
            &home.join(".codex"),
            "CODEX_HOME",
        )?;
        let ownership_path = lock_path.with_file_name("codex-route-ownership.json");
        let native = NativeLiveConfig::from_home(home, claude_dir.clone(), codex_dir.clone())?;
        Ok(Self {
            #[cfg(test)]
            claude_dir,
            native,
            lock_path,
            ownership_path,
            gate: Mutex::new(()),
        })
    }

    #[cfg(test)]
    fn with_roots(claude_dir: PathBuf, codex_dir: PathBuf, lock_path: PathBuf) -> Self {
        let ownership_path = lock_path.with_file_name("codex-route-ownership.json");
        let home = claude_dir
            .parent()
            .unwrap_or(claude_dir.as_path())
            .to_owned();
        let native = NativeLiveConfig::for_tests(&home, claude_dir.clone(), codex_dir.clone());
        Self {
            claude_dir,
            native,
            lock_path,
            ownership_path,
            gate: Mutex::new(()),
        }
    }

    fn paths(&self) -> LivePaths {
        self.native.paths()
    }

    pub fn import_native_drafts(&self, app_id: &str) -> Result<Vec<NativeImport>, LiveError> {
        let app = app_id.parse::<cc_switch_core::AppType>().map_err(|_| {
            LiveError::InvalidProvider("application is not available in Lite".to_owned())
        })?;
        self.with_lock(|| self.native.import_drafts(app))
    }

    pub fn switch_native_recoverable(
        &self,
        provider: &ProviderRecord,
        common_snippet: Option<&str>,
    ) -> Result<LiveWriteReceipt, LiveError> {
        let app = provider
            .app_id
            .parse::<cc_switch_core::AppType>()
            .map_err(|_| {
                LiveError::InvalidProvider("application is not available in Lite".to_owned())
            })?;
        if !provider
            .adapter
            .same_identity(&crate::provider::native_adapter_reference(&app))
        {
            return Err(LiveError::InvalidProvider(
                "provider does not use its native application adapter".to_owned(),
            ));
        }
        self.with_lock(|| {
            let prepared = self.native.prepare_apply_plan(provider, common_snippet)?;
            self.execute_recoverable_plan(&prepared.paths, &prepared.plan, None)
        })
    }

    pub fn remove_native_recoverable(
        &self,
        provider: &ProviderRecord,
    ) -> Result<LiveWriteReceipt, LiveError> {
        let app = provider
            .app_id
            .parse::<cc_switch_core::AppType>()
            .map_err(|_| {
                LiveError::InvalidProvider("application is not available in Lite".to_owned())
            })?;
        if !app.is_additive_mode()
            || !provider
                .adapter
                .same_identity(&crate::provider::native_adapter_reference(&app))
        {
            return Err(LiveError::InvalidProvider(
                "provider does not use an additive native adapter".to_owned(),
            ));
        }
        self.with_lock(|| {
            let prepared = self.native.prepare_remove_plan(provider)?;
            self.execute_recoverable_plan(&prepared.paths, &prepared.plan, None)
        })
    }

    pub fn import_draft(&self, app_id: &str) -> Result<ProviderDraft, LiveError> {
        self.with_lock(|| {
            let paths = self.paths();
            let mut settings = match app_id {
                "claude" => Self::read_claude_settings(
                    read_optional(&paths.claude_settings)?.as_deref(),
                    true,
                )?
                .ok_or_else(|| LiveError::Missing("Claude Code".to_owned()))?,
                "codex" => Self::read_codex_settings(
                    read_optional(&paths.codex_config)?.as_deref(),
                    read_optional(&paths.codex_auth)?.as_deref(),
                    true,
                    self.read_ownership()?.as_ref(),
                )?
                .ok_or_else(|| LiveError::Missing("Codex".to_owned()))?,
                _ => {
                    return Err(LiveError::InvalidProvider(
                        "application is not available in Lite".to_owned(),
                    ));
                }
            };
            settings.remove("hostProviderId");
            let descriptor = built_in_adapters()
                .into_iter()
                .find(|adapter| adapter.app_id == app_id)
                .ok_or_else(|| {
                    LiveError::InvalidProvider("built-in adapter is unavailable".to_owned())
                })?;
            validate_settings(&descriptor, &settings).map_err(LiveError::InvalidProvider)?;
            Ok(ProviderDraft {
                app_id: app_id.to_owned(),
                adapter: descriptor.reference,
                name: match app_id {
                    "claude" => "Imported Claude Code",
                    "codex" => "Imported Codex",
                    _ => unreachable!(),
                }
                .to_owned(),
                settings,
            })
        })
    }

    #[cfg(test)]
    pub fn switch(&self, provider: &ProviderRecord) -> Result<(), LiveError> {
        self.switch_recoverable(provider).map(drop)
    }

    pub fn switch_recoverable(
        &self,
        provider: &ProviderRecord,
    ) -> Result<LiveWriteReceipt, LiveError> {
        let descriptor = adapter_for_reference(&provider.app_id, &provider.adapter)
            .ok_or_else(|| LiveError::InvalidProvider("adapter is unavailable".to_owned()))?;
        validate_settings(&descriptor, &provider.settings).map_err(LiveError::InvalidProvider)?;

        self.with_lock(|| {
            let paths = self.paths();
            let (paths, plan, ownership) = match provider.app_id.as_str() {
                "claude" => {
                    let paths = paths.resolved_for_write(LogicalTarget::ClaudeSettings)?;
                    let original = read_optional(&paths.claude_settings)?;
                    let plan = Self::claude_plan(original.as_deref(), provider)?;
                    (paths, plan, None)
                }
                "codex" => {
                    let paths = paths.resolved_for_write(LogicalTarget::CodexConfig)?;
                    let original = read_optional(&paths.codex_config)?;
                    let stored_ownership = self.read_ownership()?;
                    let needs_save = stored_ownership.is_none();
                    let ownership = stored_ownership.unwrap_or_else(CodexRouteOwnership::new);
                    let plan = Self::codex_plan(original.as_deref(), provider, &ownership)?;
                    (paths, plan, needs_save.then_some(ownership))
                }
                _ => {
                    return Err(LiveError::InvalidProvider(
                        "application is not available in Lite".to_owned(),
                    ));
                }
            };
            self.execute_recoverable_plan(&paths, &plan, ownership.as_ref())
        })
    }

    pub fn rollback(&self, receipt: LiveWriteReceipt) -> Result<(), LiveError> {
        self.with_lock(|| {
            let mut failures = Vec::new();
            if let Err(error) = receipt.operation.rollback() {
                failures.push(error.to_string());
            }
            if let Some(ownership) = receipt.ownership {
                if let Err(error) = self.rollback_ownership(&ownership) {
                    failures.push(error.to_string());
                }
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err(OperationError::Rollback(failures.join("; ")).into())
            }
        })
    }

    pub fn with_plugin_snapshots<T, E>(
        &self,
        app_id: &str,
        capabilities: &[PluginCapability],
        action: impl FnOnce(Vec<PluginSnapshot>) -> Result<T, E>,
    ) -> Result<Result<T, E>, LiveError> {
        self.with_lock(|| {
            let paths = self.paths();
            let snapshots = plugin_snapshots(&paths, app_id, capabilities)?;
            Ok(action(snapshots))
        })
    }

    #[cfg(test)]
    pub fn execute_plugin_route<E>(
        &self,
        provider: &ProviderRecord,
        capabilities: &[PluginCapability],
        router: impl FnOnce(Vec<PluginSnapshot>) -> Result<PluginRoute, E>,
    ) -> Result<Result<(), E>, LiveError> {
        self.execute_plugin_route_recoverable(provider, capabilities, router)
            .map(|result| result.map(drop))
    }

    pub fn execute_plugin_route_recoverable<E>(
        &self,
        provider: &ProviderRecord,
        capabilities: &[PluginCapability],
        router: impl FnOnce(Vec<PluginSnapshot>) -> Result<PluginRoute, E>,
    ) -> Result<Result<LiveWriteReceipt, E>, LiveError> {
        let app_id = provider.app_id.as_str();
        self.with_lock(|| {
            let target = match app_id {
                "claude" => LogicalTarget::ClaudeSettings,
                "codex" => LogicalTarget::CodexConfig,
                _ => {
                    return Err(LiveError::InvalidProvider(
                        "application is not available in Lite".to_owned(),
                    ));
                }
            };
            if !plugin_can_write(capabilities, target) {
                return Err(LiveError::InvalidProvider(
                    "plugin adapter lacks its approved provider-routing capability".to_owned(),
                ));
            }
            let paths = self.paths();
            let snapshots = plugin_snapshots(&paths, app_id, capabilities)?;
            let route = match router(snapshots) {
                Ok(route) => route,
                Err(error) => return Ok(Err(error)),
            };
            let mut routed = provider.clone();
            routed.settings = route_settings(route);
            let descriptor = built_in_adapters()
                .into_iter()
                .find(|adapter| adapter.app_id == app_id)
                .ok_or_else(|| {
                    LiveError::InvalidProvider("built-in route adapter is unavailable".to_owned())
                })?;
            validate_settings(&descriptor, &routed.settings).map_err(LiveError::InvalidProvider)?;

            let (paths, plan, ownership) = match app_id {
                "claude" => {
                    let paths = paths.resolved_for_write(target)?;
                    let original = read_optional(&paths.claude_settings)?;
                    let plan = Self::claude_plan(original.as_deref(), &routed)?;
                    (paths, plan, None)
                }
                "codex" => {
                    let paths = paths.resolved_for_write(target)?;
                    let original = read_optional(&paths.codex_config)?;
                    let stored_ownership = self.read_ownership()?;
                    let needs_save = stored_ownership.is_none();
                    let ownership = stored_ownership.unwrap_or_else(CodexRouteOwnership::new);
                    let plan = Self::codex_plan(original.as_deref(), &routed, &ownership)?;
                    (paths, plan, needs_save.then_some(ownership))
                }
                _ => unreachable!(),
            };
            let receipt = self.execute_recoverable_plan(&paths, &plan, ownership.as_ref())?;
            Ok(Ok(receipt))
        })
    }

    fn claude_plan(
        original: Option<&[u8]>,
        provider: &ProviderRecord,
    ) -> Result<OperationPlan, LiveError> {
        let mut root = parse_json_object(original, "Claude settings.json")?;
        if let Some(field) = claude_conflicting_field(&root) {
            return Err(LiveError::UnsupportedConfig(format!(
                "remove Claude provider or model override '{field}' before switching a Lite provider"
            )));
        }
        let env = ensure_object_field(&mut root, "env", "Claude settings.json")?;
        set_or_remove(env, "ANTHROPIC_BASE_URL", setting(provider, "baseUrl"));
        set_or_remove(env, "ANTHROPIC_MODEL", setting(provider, "model"));
        env.insert(
            "ANTHROPIC_API_KEY".to_owned(),
            Value::String(required_setting(provider, "apiKey")?.to_owned()),
        );

        Ok(OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "claude".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::ClaudeSettings,
                expected: ContentExpectation::for_contents(original),
                contents: Some(pretty_json_object(root)?),
            }],
        })
    }

    fn codex_plan(
        original_config: Option<&[u8]>,
        provider: &ProviderRecord,
        ownership: &CodexRouteOwnership,
    ) -> Result<OperationPlan, LiveError> {
        let mut config = parse_toml_document(original_config)?;
        reject_active_codex_profile(&config)?;
        if let Some(reserved_id) = reserved_codex_provider_id(&config)? {
            return Err(LiveError::UnsupportedConfig(format!(
                "Codex built-in provider '{reserved_id}' cannot be redefined"
            )));
        }

        let base_url =
            normalize_base_url(setting(provider, "baseUrl").or(Some(DEFAULT_CODEX_BASE_URL)));
        let mut route = Table::new();
        route["name"] = value("CC Switch Lite");
        route["base_url"] = value(base_url);
        route["wire_api"] = value("responses");
        route["experimental_bearer_token"] = value(required_setting(provider, "apiKey")?);
        let route = Item::Table(route);
        let route_digest = managed_codex_route_digest(&route).ok_or_else(|| {
            LiveError::InvalidProvider("failed to build the managed Codex route".to_owned())
        })?;
        let route_id = lite_codex_route_id(&provider.id, &route_digest, ownership)?;

        match setting(provider, "model") {
            Some(model) => config["model"] = value(model),
            None => {
                config.remove("model");
            }
        }

        if !config.contains_key("model_providers") {
            config["model_providers"] = Item::Table(Table::new());
        }
        let providers = config["model_providers"]
            .as_table_like_mut()
            .ok_or_else(|| {
                LiveError::InvalidConfig("Codex model_providers must be a table".to_owned())
            })?;

        if let Some(existing) = providers.get(&route_id) {
            if !is_owned_codex_route(&route_id, existing, ownership) {
                return Err(LiveError::UnsupportedConfig(format!(
                    "Codex route '{route_id}' already exists and is not owned by Lite"
                )));
            }
        }
        providers.insert(&route_id, route);
        config["model_provider"] = value(&route_id);

        Ok(OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "codex".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::CodexConfig,
                expected: ContentExpectation::for_contents(original_config),
                contents: Some(config.to_string()),
            }],
        })
    }

    fn read_claude_settings(
        contents: Option<&[u8]>,
        strict: bool,
    ) -> Result<Option<Map<String, Value>>, LiveError> {
        let Some(contents) = contents else {
            return Ok(None);
        };
        let root = parse_json_object(Some(contents), "Claude settings.json")?;
        if let Some(field) = claude_conflicting_field(&root) {
            return unsupported_settings(
                strict,
                format!("Claude provider or model override '{field}' cannot be represented"),
            );
        }
        let env = match root.get("env") {
            Some(Value::Object(env)) => env,
            Some(_) if strict => {
                return Err(LiveError::InvalidConfig(
                    "Claude env must be an object".to_owned(),
                ));
            }
            _ => return Ok(None),
        };
        let api_key = secret_string_field(env, "ANTHROPIC_API_KEY", strict)?;
        let Some(api_key) = api_key.filter(|value| !value.trim().is_empty()) else {
            return if strict {
                Err(LiveError::UnsupportedConfig(
                    "Claude config has no API key managed by Lite".to_owned(),
                ))
            } else {
                Ok(None)
            };
        };

        let mut settings = Map::new();
        settings.insert("apiKey".to_owned(), Value::String(api_key.to_owned()));
        insert_optional_string(
            &mut settings,
            "baseUrl",
            string_field(env, "ANTHROPIC_BASE_URL", strict)?,
        );
        insert_optional_string(
            &mut settings,
            "model",
            string_field(env, "ANTHROPIC_MODEL", strict)?,
        );
        Ok(Some(settings))
    }

    fn read_codex_settings(
        config_contents: Option<&[u8]>,
        auth_contents: Option<&[u8]>,
        strict: bool,
        ownership: Option<&CodexRouteOwnership>,
    ) -> Result<Option<Map<String, Value>>, LiveError> {
        let config = parse_toml_document(config_contents)?;
        if config.get("profile").is_some() {
            return unsupported_settings(
                strict,
                "active Codex profiles are not supported by the Lite adapter",
            );
        }
        let active_id = optional_toml_string(config.get("model_provider"), "Codex model_provider")?
            .unwrap_or("openai");
        let configured_providers = match config.get("model_providers") {
            Some(item) => Some(item.as_table_like().ok_or_else(|| {
                LiveError::InvalidConfig("Codex model_providers must be a table".to_owned())
            })?),
            None => None,
        };
        if let Some(reserved_id) = reserved_codex_provider_id(&config)? {
            return unsupported_settings(
                strict,
                format!("Codex built-in provider '{reserved_id}' cannot be redefined"),
            );
        }
        let route_item = configured_providers.and_then(|providers| providers.get(active_id));
        let route = route_item.and_then(Item::as_table_like);

        if active_id != "openai" && route.is_none() {
            return unsupported_settings(
                strict,
                format!("active Codex provider '{active_id}' is not defined"),
            );
        }

        let base_url = if active_id == "openai" && route.is_none() {
            optional_toml_string(config.get("openai_base_url"), "Codex openai_base_url")?
        } else {
            route
                .and_then(|route| route.get("base_url"))
                .map(|item| optional_toml_string(Some(item), "Codex provider base_url"))
                .transpose()?
                .flatten()
        };
        let route_name = route
            .and_then(|route| route.get("name"))
            .map(|item| optional_toml_string(Some(item), "Codex provider name"))
            .transpose()?
            .flatten();
        let model = optional_toml_string(config.get("model"), "Codex model")?;

        if active_id != "openai" && route_name.is_none_or(|name| name.trim().is_empty()) {
            return unsupported_settings(strict, "a custom Codex provider must have a name");
        }
        if route_name == Some("OpenAI") {
            return unsupported_settings(
                strict,
                "a custom Codex provider named OpenAI has first-party behavior Lite cannot reproduce",
            );
        }

        let provider_token = route
            .and_then(|route| route.get("experimental_bearer_token"))
            .map(|item| optional_toml_string(Some(item), "Codex provider bearer token"))
            .transpose()?
            .flatten()
            .filter(|token| !token.trim().is_empty());
        let uses_auth_file = route
            .and_then(|route| route.get("requires_openai_auth"))
            .map(|item| optional_toml_bool(Some(item), "Codex requires_openai_auth"))
            .transpose()?
            .flatten()
            .unwrap_or(active_id == "openai" && route.is_none());

        let allowed_fields: &[&str] = if provider_token.is_some() {
            &["name", "base_url", "wire_api", "experimental_bearer_token"]
        } else {
            &["name", "base_url", "wire_api", "requires_openai_auth"]
        };
        if let Some(route) = route {
            let has_unsupported_fields =
                route.iter().any(|(key, _)| !allowed_fields.contains(&key));
            let wire_api = route
                .get("wire_api")
                .map(|item| optional_toml_string(Some(item), "Codex wire_api"))
                .transpose()?
                .flatten()
                .unwrap_or("responses");
            if has_unsupported_fields || wire_api != "responses" {
                return unsupported_settings(
                    strict,
                    "only basic Codex Responses providers can be imported",
                );
            }
        }

        let api_key = if let Some(token) = provider_token {
            if uses_auth_file {
                return unsupported_settings(
                    strict,
                    "a Codex provider cannot combine its own token with shared login auth",
                );
            }
            token.to_owned()
        } else {
            if !uses_auth_file {
                return unsupported_settings(
                    strict,
                    "the active Codex provider does not expose an API key Lite can import",
                );
            }
            if auth_contents.is_none() && config_contents.is_none() {
                return Ok(None);
            }
            let auth = parse_json_object(auth_contents, "Codex auth.json")?;
            if !matches!(
                string_field(&auth, "auth_mode", strict)?,
                None | Some("apikey")
            ) || codex_auth_has_other_credentials(&auth)
            {
                return unsupported_settings(strict, "Codex auth.json is not in API-key mode");
            }
            let Some(api_key) = secret_string_field(&auth, "OPENAI_API_KEY", strict)?
                .filter(|value| !value.trim().is_empty())
            else {
                return unsupported_settings(
                    strict,
                    "Codex API-key auth is missing OPENAI_API_KEY",
                );
            };
            api_key.to_owned()
        };

        if active_id == "openai" && route.is_none() {
            return unsupported_settings(
                strict,
                "Codex built-in OpenAI API-key auth cannot be imported without changing shared auth.json",
            );
        }

        let mut settings = Map::new();
        settings.insert("apiKey".to_owned(), Value::String(api_key));
        insert_optional_string(&mut settings, "baseUrl", base_url);
        insert_optional_string(&mut settings, "model", model);
        if let (Some(route), Some(ownership)) = (route_item, ownership) {
            if let Some(provider_id) = owned_provider_id(active_id, route, ownership) {
                settings.insert(
                    "hostProviderId".to_owned(),
                    Value::String(provider_id.to_owned()),
                );
            }
        }
        Ok(Some(settings))
    }

    fn read_ownership(&self) -> Result<Option<CodexRouteOwnership>, LiveError> {
        let Some(contents) =
            read_optional_no_follow(&self.ownership_path, MAX_PLUGIN_SNAPSHOT_BYTES)?
        else {
            return Ok(None);
        };
        let ownership: CodexRouteOwnership = serde_json::from_slice(&contents).map_err(|_| {
            LiveError::InvalidConfig("Lite Codex route ownership file is invalid".to_owned())
        })?;
        if ownership.version != OWNERSHIP_VERSION
            || uuid::Uuid::parse_str(&ownership.installation_id).is_err()
        {
            return Err(LiveError::InvalidConfig(
                "Lite Codex route ownership file is invalid".to_owned(),
            ));
        }
        Ok(Some(ownership))
    }

    fn execute_recoverable_plan(
        &self,
        paths: &LivePaths,
        plan: &OperationPlan,
        ownership: Option<&CodexRouteOwnership>,
    ) -> Result<LiveWriteReceipt, LiveError> {
        let ownership = ownership
            .map(|value| self.write_new_ownership(value))
            .transpose()?;
        match OperationExecutor::new(paths).execute_recoverable(plan) {
            Ok(operation) => Ok(LiveWriteReceipt {
                operation,
                ownership,
            }),
            Err(error) => {
                if let Some(ownership) = ownership {
                    if let Err(rollback_error) = self.rollback_ownership(&ownership) {
                        return Err(OperationError::Rollback(format!(
                            "operation error: {error}; ownership rollback error: {rollback_error}"
                        ))
                        .into());
                    }
                }
                Err(error.into())
            }
        }
    }

    fn write_new_ownership(
        &self,
        ownership: &CodexRouteOwnership,
    ) -> Result<OwnershipWriteReceipt, LiveError> {
        if read_optional_no_follow(&self.ownership_path, MAX_PLUGIN_SNAPSHOT_BYTES)?.is_some() {
            return Err(OperationError::Conflict.into());
        }
        let written = ownership_contents(ownership)?;
        atomic_write_private(&self.ownership_path, &written).map_err(OperationError::File)?;
        Ok(OwnershipWriteReceipt { written })
    }

    fn rollback_ownership(&self, receipt: &OwnershipWriteReceipt) -> Result<(), OperationError> {
        let current = read_optional_no_follow(&self.ownership_path, MAX_PLUGIN_SNAPSHOT_BYTES)?;
        match current {
            None => Ok(()),
            Some(current) if current == receipt.written => {
                fs::remove_file(&self.ownership_path).map_err(|source| OperationError::Io {
                    path: self.ownership_path.clone(),
                    source,
                })
            }
            Some(_) => Err(OperationError::Rollback(
                "Codex route ownership changed after Lite wrote it; external contents were preserved"
                    .to_owned(),
            )),
        }
    }

    #[cfg(test)]
    fn read_or_create_ownership(&self) -> Result<CodexRouteOwnership, LiveError> {
        if let Some(ownership) = self.read_ownership()? {
            return Ok(ownership);
        }
        let ownership = CodexRouteOwnership::new();
        self.write_ownership(&ownership)?;
        Ok(ownership)
    }

    #[cfg(test)]
    fn write_ownership(&self, ownership: &CodexRouteOwnership) -> Result<(), LiveError> {
        let contents = ownership_contents(ownership)?;
        atomic_write_private(&self.ownership_path, &contents).map_err(OperationError::File)?;
        Ok(())
    }

    fn with_lock<T>(&self, action: impl FnOnce() -> Result<T, LiveError>) -> Result<T, LiveError> {
        let _guard = self
            .gate
            .try_lock()
            .map_err(|_| LiveError::LockUnavailable)?;
        let _file_lock = self.lock_file()?;
        action()
    }

    fn lock_file(&self) -> Result<File, LiveError> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent).map_err(|source| LiveError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&self.lock_path)
            .map_err(|source| LiveError::Io {
                path: self.lock_path.clone(),
                source,
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            lock.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|source| LiveError::Io {
                    path: self.lock_path.clone(),
                    source,
                })?;
        }
        FileExt::try_lock(&lock).map_err(|error| match error {
            TryLockError::WouldBlock => LiveError::LockUnavailable,
            TryLockError::Error(source) => LiveError::Io {
                path: self.lock_path.clone(),
                source,
            },
        })?;
        Ok(lock)
    }
}

fn ownership_contents(ownership: &CodexRouteOwnership) -> Result<Vec<u8>, LiveError> {
    let mut contents = serde_json::to_vec_pretty(ownership).map_err(|_| {
        LiveError::InvalidConfig("Lite Codex route ownership could not be saved".to_owned())
    })?;
    contents.push(b'\n');
    Ok(contents)
}

fn plugin_snapshots(
    paths: &LivePaths,
    app_id: &str,
    capabilities: &[PluginCapability],
) -> Result<Vec<PluginSnapshot>, LiveError> {
    let allowed = |capability| capabilities.contains(&capability);
    let requested = match app_id {
        "claude" => vec![(
            PluginCapability::ReadClaudeSettings,
            PluginSlot::ClaudeSettings,
            &paths.claude_settings,
        )],
        "codex" => vec![
            (
                PluginCapability::ReadCodexConfig,
                PluginSlot::CodexConfig,
                &paths.codex_config,
            ),
            (
                PluginCapability::ReadCodexAuth,
                PluginSlot::CodexAuth,
                &paths.codex_auth,
            ),
        ],
        _ => {
            return Err(LiveError::InvalidProvider(
                "application is not available in Lite".to_owned(),
            ));
        }
    };
    requested
        .into_iter()
        .filter(|(capability, _, _)| allowed(*capability))
        .map(|(_, slot, path)| {
            let contents = if slot == PluginSlot::CodexAuth {
                plugin_codex_api_key_snapshot(path)?
            } else {
                read_optional_no_follow(path, MAX_PLUGIN_SNAPSHOT_BYTES)?
            };
            let digest = contents.as_deref().map(sha256);
            let contents = contents
                .map(|contents| {
                    String::from_utf8(contents).map_err(|_| {
                        LiveError::InvalidConfig(
                            "plugin-readable live configuration is not UTF-8".to_owned(),
                        )
                    })
                })
                .transpose()?;
            Ok(PluginSnapshot {
                slot,
                contents,
                digest,
            })
        })
        .collect()
}

fn plugin_codex_api_key_snapshot(path: &Path) -> Result<Option<Vec<u8>>, LiveError> {
    let Some(contents) = read_optional_no_follow(path, MAX_PLUGIN_SNAPSHOT_BYTES)? else {
        return Ok(None);
    };
    let auth = parse_json_object(Some(&contents), "Codex auth.json")?;
    if !matches!(
        string_field(&auth, "auth_mode", true)?,
        None | Some("apikey")
    ) || codex_auth_has_other_credentials(&auth)
    {
        return Ok(None);
    }
    let Some(api_key) = secret_string_field(&auth, "OPENAI_API_KEY", true)?
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    serde_json::to_vec(&serde_json::json!({ "OPENAI_API_KEY": api_key }))
        .map(Some)
        .map_err(|_| LiveError::InvalidConfig("Codex API-key snapshot is invalid".to_owned()))
}

fn plugin_can_write(capabilities: &[PluginCapability], target: LogicalTarget) -> bool {
    match target {
        LogicalTarget::ClaudeSettings => {
            capabilities.contains(&PluginCapability::WriteClaudeSettings)
        }
        LogicalTarget::CodexConfig => capabilities.contains(&PluginCapability::WriteCodexConfig),
        _ => false,
    }
}

fn route_settings(route: PluginRoute) -> Map<String, Value> {
    let mut settings = Map::new();
    settings.insert("apiKey".to_owned(), Value::String(route.api_key));
    if let Some(base_url) = route.base_url {
        settings.insert("baseUrl".to_owned(), Value::String(base_url));
    }
    if let Some(model) = route.model {
        settings.insert("model".to_owned(), Value::String(model));
    }
    settings
}

fn parse_json_object(
    contents: Option<&[u8]>,
    label: &str,
) -> Result<Map<String, Value>, LiveError> {
    let Some(contents) = contents else {
        return Ok(Map::new());
    };
    let value: Value = serde_json::from_slice(contents)
        .map_err(|error| LiveError::InvalidConfig(format!("{label}: {error}")))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| LiveError::InvalidConfig(format!("{label} must contain an object")))
}

fn config_root(
    override_value: Option<&OsStr>,
    default: &Path,
    variable: &str,
) -> Result<PathBuf, LiveError> {
    let configured = override_value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default.to_owned());
    if !configured.is_absolute() {
        return Err(LiveError::InvalidConfig(format!(
            "{variable} must be an absolute path"
        )));
    }

    let mut ancestor = configured.clone();
    let mut missing = Vec::new();
    loop {
        match fs::metadata(&ancestor) {
            Ok(metadata) if metadata.is_dir() => {
                let mut resolved = fs::canonicalize(&ancestor).map_err(|source| LiveError::Io {
                    path: ancestor.clone(),
                    source,
                })?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Ok(_) => {
                return Err(LiveError::InvalidConfig(format!(
                    "{variable} must point to a directory"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    LiveError::InvalidConfig(format!("{variable} has no existing parent directory"))
                })?;
                missing.push(name.to_owned());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| {
                        LiveError::InvalidConfig(format!(
                            "{variable} has no existing parent directory"
                        ))
                    })?
                    .to_owned();
            }
            Err(source) => {
                return Err(LiveError::Io {
                    path: ancestor,
                    source,
                });
            }
        }
    }
}

fn parse_toml_document(contents: Option<&[u8]>) -> Result<DocumentMut, LiveError> {
    let Some(contents) = contents else {
        return Ok(DocumentMut::new());
    };
    let text = std::str::from_utf8(contents)
        .map_err(|error| LiveError::InvalidConfig(format!("Codex config.toml: {error}")))?;
    if text.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    text.parse::<DocumentMut>()
        .map_err(|_| LiveError::InvalidConfig("Codex config.toml could not be parsed".to_owned()))
}

fn optional_toml_string<'a>(
    item: Option<&'a Item>,
    label: &str,
) -> Result<Option<&'a str>, LiveError> {
    match item {
        Some(item) => item
            .as_str()
            .map(Some)
            .ok_or_else(|| LiveError::InvalidConfig(format!("{label} must be a string"))),
        None => Ok(None),
    }
}

fn optional_toml_bool(item: Option<&Item>, label: &str) -> Result<Option<bool>, LiveError> {
    match item {
        Some(item) => item
            .as_bool()
            .map(Some)
            .ok_or_else(|| LiveError::InvalidConfig(format!("{label} must be a boolean"))),
        None => Ok(None),
    }
}

fn unsupported_settings(
    strict: bool,
    message: impl Into<String>,
) -> Result<Option<Map<String, Value>>, LiveError> {
    if strict {
        Err(LiveError::UnsupportedConfig(message.into()))
    } else {
        Ok(None)
    }
}

fn lite_codex_route_id(
    provider_id: &str,
    digest: &str,
    ownership: &CodexRouteOwnership,
) -> Result<String, LiveError> {
    uuid::Uuid::parse_str(provider_id).map_err(|_| {
        LiveError::InvalidProvider("provider ID is not a valid host UUID".to_owned())
    })?;
    Ok(format!(
        "{LITE_CODEX_PROVIDER_PREFIX}{}-{provider_id}-{digest}",
        ownership.installation_id
    ))
}

struct ParsedLiteRoute<'a> {
    installation_id: &'a str,
    provider_id: &'a str,
    digest: &'a str,
}

fn parse_lite_route_id(route_id: &str) -> Option<ParsedLiteRoute<'_>> {
    let value = route_id.strip_prefix(LITE_CODEX_PROVIDER_PREFIX)?;
    if value.len() != 138 || value.as_bytes().get(36) != Some(&b'-') {
        return None;
    }
    if value.as_bytes().get(73) != Some(&b'-') {
        return None;
    }
    let installation_id = value.get(..36)?;
    let provider_id = value.get(37..73)?;
    let digest = value.get(74..)?;
    uuid::Uuid::parse_str(installation_id).ok()?;
    uuid::Uuid::parse_str(provider_id).ok()?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    Some(ParsedLiteRoute {
        installation_id,
        provider_id,
        digest,
    })
}

fn reject_active_codex_profile(config: &DocumentMut) -> Result<(), LiveError> {
    if config.get("profile").is_some() {
        return Err(LiveError::UnsupportedConfig(
            "active Codex profiles are not supported by the Lite adapter".to_owned(),
        ));
    }
    Ok(())
}

fn reserved_codex_provider_id(config: &DocumentMut) -> Result<Option<&str>, LiveError> {
    let Some(item) = config.get("model_providers") else {
        return Ok(None);
    };
    let providers = item.as_table_like().ok_or_else(|| {
        LiveError::InvalidConfig("Codex model_providers must be a table".to_owned())
    })?;
    Ok(providers
        .iter()
        .map(|(id, _)| id)
        .find(|id| RESERVED_CODEX_PROVIDER_IDS.contains(id)))
}

fn codex_auth_has_other_credentials(auth: &Map<String, Value>) -> bool {
    [
        "tokens",
        "agent_identity",
        "personal_access_token",
        "bedrock_api_key",
        "bedrock_access_keys",
    ]
    .into_iter()
    .any(|key| auth.get(key).is_some_and(value_is_configured))
}

fn is_owned_codex_route(route_id: &str, item: &Item, ownership: &CodexRouteOwnership) -> bool {
    let Some(route) = parse_lite_route_id(route_id) else {
        return false;
    };
    route.installation_id == ownership.installation_id
        && managed_codex_route_digest(item).is_some_and(|actual| actual == route.digest)
}

fn owned_provider_id<'a>(
    route_id: &'a str,
    item: &Item,
    ownership: &CodexRouteOwnership,
) -> Option<&'a str> {
    if !is_owned_codex_route(route_id, item, ownership) {
        return None;
    }
    parse_lite_route_id(route_id).map(|route| route.provider_id)
}

fn managed_codex_route_digest(item: &Item) -> Option<String> {
    let route = item.as_table_like()?;
    if route.iter().any(|(key, _)| {
        !matches!(
            key,
            "name" | "base_url" | "wire_api" | "experimental_bearer_token"
        )
    }) {
        return None;
    }
    let name = route.get("name").and_then(Item::as_str)?;
    let base_url = route.get("base_url").and_then(Item::as_str)?;
    if name != "CC Switch Lite" || route.get("wire_api").and_then(Item::as_str) != Some("responses")
    {
        return None;
    }
    let token = route
        .get("experimental_bearer_token")
        .and_then(Item::as_str)
        .filter(|token| !token.trim().is_empty())?;
    let canonical = serde_json::to_vec(&(name, base_url, "responses", token)).ok()?;
    Some(sha256(&canonical))
}

fn claude_conflicting_field(root: &Map<String, Value>) -> Option<&'static str> {
    for key in CLAUDE_ROOT_CONFLICT_KEYS {
        if root.get(key).is_some_and(value_is_configured) {
            return Some(key);
        }
    }
    let env = root.get("env").and_then(Value::as_object)?;
    CLAUDE_ENV_CONFLICT_KEYS
        .into_iter()
        .find(|key| env.get(*key).is_some_and(value_is_configured))
}

fn value_is_configured(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

fn ensure_object_field<'a>(
    root: &'a mut Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<&'a mut Map<String, Value>, LiveError> {
    if !root.contains_key(key) {
        root.insert(key.to_owned(), Value::Object(Map::new()));
    }
    root.get_mut(key)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| LiveError::InvalidConfig(format!("{label} field '{key}' must be an object")))
}

fn pretty_json_object(root: Map<String, Value>) -> Result<String, LiveError> {
    let mut contents = serde_json::to_string_pretty(&Value::Object(root))
        .map_err(|error| LiveError::InvalidConfig(format!("JSON serialization failed: {error}")))?;
    contents.push('\n');
    Ok(contents)
}

fn required_setting<'a>(provider: &'a ProviderRecord, key: &str) -> Result<&'a str, LiveError> {
    provider
        .settings
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| LiveError::InvalidProvider(format!("setting '{key}' is required")))
}

fn setting<'a>(provider: &'a ProviderRecord, key: &str) -> Option<&'a str> {
    provider
        .settings
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn set_or_remove(target: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    match value {
        Some(value) => {
            target.insert(key.to_owned(), Value::String(value.to_owned()));
        }
        None => {
            target.remove(key);
        }
    }
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    strict: bool,
) -> Result<Option<&'a str>, LiveError> {
    match object.get(key) {
        Some(Value::String(value)) => Ok(Some(value.trim())),
        Some(_) if strict => Err(LiveError::UnsupportedConfig(format!(
            "field '{key}' is not a string"
        ))),
        _ => Ok(None),
    }
}

fn secret_string_field<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    strict: bool,
) -> Result<Option<&'a str>, LiveError> {
    match object.get(key) {
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) if strict => Err(LiveError::UnsupportedConfig(format!(
            "field '{key}' is not a string"
        ))),
        _ => Ok(None),
    }
}

fn insert_optional_string(target: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        target.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn normalize_base_url(value: Option<&str>) -> &str {
    value.unwrap_or("").trim().trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::built_in_adapters;
    use serde_json::json;

    const PROVIDER_ID: &str = "015a3541-8381-43f2-b11f-45dd66c1f4b8";

    fn live(directory: &Path) -> LiveConfig {
        LiveConfig::with_roots(
            directory.join(".claude"),
            directory.join(".codex"),
            directory.join("lite/live.lock"),
        )
    }

    fn provider(app_id: &str, settings: Value) -> ProviderRecord {
        let descriptor = built_in_adapters()
            .into_iter()
            .find(|adapter| adapter.app_id == app_id)
            .expect("built-in adapter");
        ProviderRecord {
            id: PROVIDER_ID.to_owned(),
            revision: 1,
            app_id: app_id.to_owned(),
            adapter: descriptor.reference,
            name: "Work".to_owned(),
            settings: settings.as_object().unwrap().clone(),
            category: None,
            metadata: Value::Object(Map::new()),
            extensions: Map::new(),
        }
    }

    fn codex_route(base_url: &str, api_key: &str) -> Item {
        let mut route = Table::new();
        route["name"] = value("CC Switch Lite");
        route["base_url"] = value(base_url);
        route["wire_api"] = value("responses");
        route["experimental_bearer_token"] = value(api_key);
        Item::Table(route)
    }

    fn expected_route_id(ownership: &CodexRouteOwnership, api_key: &str) -> String {
        let route = codex_route(DEFAULT_CODEX_BASE_URL, api_key);
        let digest = managed_codex_route_digest(&route).unwrap();
        lite_codex_route_id(PROVIDER_ID, &digest, ownership).unwrap()
    }

    #[test]
    fn claude_switch_preserves_unmanaged_settings() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(
            &paths.claude_settings,
            serde_json::to_vec_pretty(&json!({
                "permissions": {"allow": ["Read"]},
                "env": {
                    "KEEP_ME": "yes",
                    "ANTHROPIC_BASE_URL": "https://old.example"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        live.switch(&provider(
            "claude",
            json!({
                "apiKey": "new-secret",
                "baseUrl": "https://new.example",
                "model": "claude-sonnet-4-6"
            }),
        ))
        .expect("switch Claude provider");

        let written: Value =
            serde_json::from_slice(&fs::read(&paths.claude_settings).expect("read settings"))
                .unwrap();
        assert_eq!(written["permissions"]["allow"][0], "Read");
        assert_eq!(written["env"]["KEEP_ME"], "yes");
        assert_eq!(written["env"]["ANTHROPIC_API_KEY"], "new-secret");
    }

    #[test]
    fn recoverable_switch_restores_the_previous_live_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        let original = br#"{"env":{"ANTHROPIC_API_KEY":"old"},"keep":true}"#;
        fs::write(&paths.claude_settings, original).unwrap();

        let receipt = live
            .switch_recoverable(&provider("claude", json!({"apiKey": "new"})))
            .unwrap();
        assert_ne!(fs::read(&paths.claude_settings).unwrap(), original);

        live.rollback(receipt).unwrap();
        assert_eq!(fs::read(&paths.claude_settings).unwrap(), original);
    }

    #[test]
    fn claude_switch_rejects_unmanaged_auth_and_model_fields() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(
            &paths.claude_settings,
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"keep","ANTHROPIC_DEFAULT_HAIKU_MODEL":"keep"}}"#,
        )
        .unwrap();

        let result = live.switch(&provider("claude", json!({"apiKey": "new"})));

        assert!(matches!(result, Err(LiveError::UnsupportedConfig(_))));
        assert_eq!(
            fs::read_to_string(paths.claude_settings).unwrap(),
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"keep","ANTHROPIC_DEFAULT_HAIKU_MODEL":"keep"}}"#
        );
    }

    #[test]
    fn claude_import_and_switch_reject_custom_request_credentials() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        let original = r#"{"env":{"ANTHROPIC_API_KEY":"old","ANTHROPIC_CUSTOM_HEADERS":"X-Tenant: old","CLAUDE_CODE_CLIENT_CERT":"/old/cert.pem"}}"#;
        fs::write(&paths.claude_settings, original).unwrap();

        assert!(matches!(
            live.import_draft("claude"),
            Err(LiveError::UnsupportedConfig(_))
        ));
        assert!(matches!(
            live.switch(&provider("claude", json!({"apiKey": "new"}))),
            Err(LiveError::UnsupportedConfig(_))
        ));
        assert_eq!(fs::read_to_string(paths.claude_settings).unwrap(), original);
    }

    #[test]
    fn claude_host_managed_provider_is_not_imported_or_switched() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        let original =
            r#"{"env":{"ANTHROPIC_API_KEY":"secret","CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST":"1"}}"#;
        fs::write(&paths.claude_settings, original).unwrap();
        let provider = provider("claude", json!({"apiKey": "secret"}));

        assert!(matches!(
            live.import_draft("claude"),
            Err(LiveError::UnsupportedConfig(_))
        ));
        assert!(matches!(
            live.switch(&provider),
            Err(LiveError::UnsupportedConfig(_))
        ));
        assert_eq!(fs::read_to_string(paths.claude_settings).unwrap(), original);
    }

    #[test]
    fn switches_preserve_nonempty_api_key_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());

        live.switch(&provider("claude", json!({"apiKey": " secret "})))
            .expect("switch Claude provider");
        live.switch(&provider("codex", json!({"apiKey": " secret "})))
            .expect("switch Codex provider");

        let paths = live.paths();
        let claude: Value = serde_json::from_slice(&fs::read(paths.claude_settings).unwrap())
            .expect("parse Claude settings");
        let codex = fs::read_to_string(paths.codex_config)
            .unwrap()
            .parse::<DocumentMut>()
            .expect("parse Codex config");
        let route_id = codex["model_provider"].as_str().unwrap();
        assert_eq!(claude["env"]["ANTHROPIC_API_KEY"], " secret ");
        assert_eq!(
            codex["model_providers"][route_id]["experimental_bearer_token"].as_str(),
            Some(" secret ")
        );
    }

    #[test]
    fn codex_switch_preserves_shared_auth_and_unmanaged_toml() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_auth.parent().unwrap()).unwrap();
        let auth = r#"{"auth_mode":"chatgpt","tokens":{"access_token":"keep"}}"#;
        fs::write(&paths.codex_auth, auth).unwrap();
        fs::write(
            &paths.codex_config,
            "approval_policy = \"on-request\"\n\n[model_providers.other]\nname = \"Other\"\nbase_url = \"https://other.example/v1\"\n",
        )
        .unwrap();

        live.switch(&provider(
            "codex",
            json!({
                "apiKey": "new-secret",
                "baseUrl": "https://gateway.example/v1",
                "model": "gpt-5"
            }),
        ))
        .expect("switch Codex provider");

        let config = fs::read_to_string(&paths.codex_config).unwrap();
        let parsed = config.parse::<DocumentMut>().unwrap();
        let route_id = parsed["model_provider"].as_str().unwrap().to_owned();
        assert_eq!(fs::read_to_string(paths.codex_auth).unwrap(), auth);
        assert_eq!(parsed["approval_policy"].as_str(), Some("on-request"));
        assert_eq!(parsed["model_provider"].as_str(), Some(route_id.as_str()));
        assert_eq!(
            parsed["model_providers"]["other"]["name"].as_str(),
            Some("Other")
        );
        assert_eq!(
            parsed["model_providers"][&route_id]["base_url"].as_str(),
            Some("https://gateway.example/v1")
        );
        assert_eq!(
            parsed["model_providers"][&route_id]["experimental_bearer_token"].as_str(),
            Some("new-secret")
        );
        assert!(parsed["model_providers"][&route_id]
            .get("requires_openai_auth")
            .is_none());
    }

    #[test]
    fn codex_switch_refuses_a_colliding_unowned_route() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_config.parent().unwrap()).unwrap();
        let ownership = live.read_or_create_ownership().unwrap();
        let route_id = expected_route_id(&ownership, "secret");
        let original = format!(
            "[model_providers.{route_id}]\nname = \"User route\"\nbase_url = \"https://user.example/v1\"\nhttp_headers = {{ X-Test = \"keep\" }}\n"
        );
        fs::write(&paths.codex_config, &original).unwrap();

        let result = live.switch(&provider("codex", json!({"apiKey": "secret"})));

        assert!(matches!(result, Err(LiveError::UnsupportedConfig(_))));
        assert_eq!(fs::read_to_string(paths.codex_config).unwrap(), original);
    }

    #[test]
    fn codex_switch_preserves_an_exact_route_from_another_installation() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_config.parent().unwrap()).unwrap();
        let external_ownership = CodexRouteOwnership::new();
        let route_id = expected_route_id(&external_ownership, "external");
        let original = format!(
            "[model_providers.{route_id}]\nname = \"CC Switch Lite\"\nbase_url = \"https://api.openai.com/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"external\"\n"
        );
        fs::write(&paths.codex_config, &original).unwrap();

        live.switch(&provider("codex", json!({"apiKey": "secret"})))
            .expect("switch provider");
        let written = fs::read_to_string(paths.codex_config).unwrap();

        assert!(written.contains(&original));
    }

    #[test]
    fn codex_switch_refuses_an_externally_modified_owned_route() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        let provider = provider("codex", json!({"apiKey": "secret"}));
        live.switch(&provider).expect("initial switch");
        let modified = fs::read_to_string(&paths.codex_config).unwrap().replace(
            "experimental_bearer_token = \"secret\"",
            "experimental_bearer_token = \"external\"",
        );
        fs::write(&paths.codex_config, &modified).unwrap();

        let result = live.switch(&provider);

        assert!(matches!(result, Err(LiveError::UnsupportedConfig(_))));
        assert_eq!(fs::read_to_string(paths.codex_config).unwrap(), modified);
    }

    #[test]
    fn codex_switch_preserves_previous_lite_routes_referenced_by_profiles() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        let first = provider("codex", json!({"apiKey": "first"}));
        live.switch(&first).expect("initial switch");

        let mut config = fs::read_to_string(&paths.codex_config)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let first_route = config["model_provider"].as_str().unwrap().to_owned();
        config["profiles"]["saved"]["model_provider"] = value(&first_route);
        fs::write(&paths.codex_config, config.to_string()).unwrap();

        live.switch(&provider("codex", json!({"apiKey": "second"})))
            .expect("second switch");
        let config = fs::read_to_string(paths.codex_config)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();

        assert!(config["model_providers"].get(&first_route).is_some());
        assert_eq!(
            config["profiles"]["saved"]["model_provider"].as_str(),
            Some(first_route.as_str())
        );
    }

    #[test]
    fn import_extracts_only_settings_owned_by_lite() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(
            &paths.claude_settings,
            serde_json::to_vec(&json!({
                "permissions": {"allow": ["Read"]},
                "env": {
                    "ANTHROPIC_API_KEY": "secret",
                    "ANTHROPIC_BASE_URL": "https://proxy.example/v1",
                    "ANTHROPIC_MODEL": "claude-sonnet-4-6",
                    "UNMANAGED": "not-imported"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let draft = live.import_draft("claude").expect("import Claude");

        assert_eq!(draft.settings.len(), 3);
        assert_eq!(draft.settings["apiKey"], "secret");
        assert!(!draft.settings.contains_key("UNMANAGED"));
    }

    #[test]
    fn import_rejects_codex_routes_lite_cannot_reproduce() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_auth.parent().unwrap()).unwrap();
        fs::write(
            &paths.codex_auth,
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"secret"}"#,
        )
        .unwrap();
        fs::write(
            &paths.codex_config,
            "model_provider = \"chat\"\n\n[model_providers.chat]\nname = \"Chat\"\nbase_url = \"https://chat.example/v1\"\nwire_api = \"chat\"\nrequires_openai_auth = true\n",
        )
        .unwrap();

        let result = live.import_draft("codex");

        assert!(matches!(result, Err(LiveError::UnsupportedConfig(_))));
    }

    #[test]
    fn import_rejects_chatgpt_auth_even_when_an_api_key_field_exists() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_auth.parent().unwrap()).unwrap();
        fs::write(
            &paths.codex_auth,
            r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":"must-not-use","tokens":{"access_token":"oauth"}}"#,
        )
        .unwrap();
        fs::write(
            &paths.codex_config,
            "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"Custom\"\nbase_url = \"https://gateway.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n",
        )
        .unwrap();

        let result = live.import_draft("codex");

        assert!(matches!(result, Err(LiveError::UnsupportedConfig(_))));
    }

    #[test]
    fn plugin_auth_snapshot_withholds_oauth_and_sanitizes_api_key_mode() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_auth.parent().unwrap()).unwrap();
        fs::write(
            &paths.codex_auth,
            r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":"must-not-use","tokens":{"access_token":"oauth"}}"#,
        )
        .unwrap();

        let snapshots =
            plugin_snapshots(&paths, "codex", &[PluginCapability::ReadCodexAuth]).unwrap();

        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].contents, None);
        assert_eq!(snapshots[0].digest, None);

        fs::write(
            &paths.codex_auth,
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"secret","last_refresh":null}"#,
        )
        .unwrap();
        let snapshots =
            plugin_snapshots(&paths, "codex", &[PluginCapability::ReadCodexAuth]).unwrap();
        assert_eq!(
            snapshots[0].contents.as_deref(),
            Some(r#"{"OPENAI_API_KEY":"secret"}"#)
        );
    }

    #[test]
    fn import_accepts_explicit_codex_api_key_mode() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_auth.parent().unwrap()).unwrap();
        fs::write(
            &paths.codex_auth,
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"secret"}"#,
        )
        .unwrap();
        fs::write(
            &paths.codex_config,
            "model_provider = \"custom\"\nmodel = \"gpt-5\"\n\n[model_providers.custom]\nname = \"Custom\"\nbase_url = \"https://gateway.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n",
        )
        .unwrap();

        let draft = live.import_draft("codex").expect("import Codex");

        assert_eq!(draft.settings["apiKey"], "secret");
        assert_eq!(draft.settings["baseUrl"], "https://gateway.example/v1");
        assert_eq!(draft.settings["model"], "gpt-5");
    }

    #[test]
    fn import_accepts_legacy_codex_api_key_mode_without_mixed_auth() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_auth.parent().unwrap()).unwrap();
        fs::write(
            &paths.codex_auth,
            r#"{"OPENAI_API_KEY":"secret","tokens":null,"last_refresh":null}"#,
        )
        .unwrap();
        fs::write(
            &paths.codex_config,
            "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"Custom\"\nbase_url = \"https://gateway.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n",
        )
        .unwrap();

        let draft = live
            .import_draft("codex")
            .expect("import legacy Codex auth");

        assert_eq!(draft.settings["apiKey"], "secret");

        fs::write(
            &paths.codex_auth,
            r#"{"OPENAI_API_KEY":"secret","tokens":{"access_token":"oauth"}}"#,
        )
        .unwrap();
        assert!(matches!(
            live.import_draft("codex"),
            Err(LiveError::UnsupportedConfig(_))
        ));
    }

    #[test]
    fn import_rejects_codex_built_in_openai_auth() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_auth.parent().unwrap()).unwrap();
        fs::write(
            &paths.codex_auth,
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"secret"}"#,
        )
        .unwrap();
        fs::write(
            &paths.codex_config,
            "openai_base_url = \"https://gateway.example/v1\"\n",
        )
        .unwrap();

        assert!(matches!(
            live.import_draft("codex"),
            Err(LiveError::UnsupportedConfig(_))
        ));
    }

    #[test]
    fn import_rejects_reserved_codex_provider_tables_and_empty_names() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_config.parent().unwrap()).unwrap();
        for reserved_id in RESERVED_CODEX_PROVIDER_IDS {
            fs::write(
                &paths.codex_config,
                format!(
                    "model_provider = \"custom\"\n\n[model_providers.{reserved_id}]\nname = \"Override\"\n\n[model_providers.custom]\nname = \"Custom\"\nbase_url = \"https://gateway.example/v1\"\nexperimental_bearer_token = \"secret\"\n"
                ),
            )
            .unwrap();
            assert!(matches!(
                live.import_draft("codex"),
                Err(LiveError::UnsupportedConfig(_))
            ));
            assert!(matches!(
                live.switch(&provider("codex", json!({"apiKey": "new"}))),
                Err(LiveError::UnsupportedConfig(_))
            ));
        }

        fs::write(
            &paths.codex_config,
            "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"   \"\nbase_url = \"https://gateway.example/v1\"\nexperimental_bearer_token = \"secret\"\n",
        )
        .unwrap();
        assert!(matches!(
            live.import_draft("codex"),
            Err(LiveError::UnsupportedConfig(_))
        ));
    }

    #[test]
    fn codex_active_profiles_are_rejected_without_touching_config() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_config.parent().unwrap()).unwrap();
        let original = "profile = \"work\"\n\n[profiles.work]\nmodel_provider = \"other\"\n";
        fs::write(&paths.codex_config, original).unwrap();

        let result = live.switch(&provider("codex", json!({"apiKey": "secret"})));

        assert!(matches!(result, Err(LiveError::UnsupportedConfig(_))));
        assert_eq!(fs::read_to_string(paths.codex_config).unwrap(), original);
    }

    #[test]
    fn import_rejects_distinct_claude_model_overrides() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(
            &paths.claude_settings,
            serde_json::to_vec(&json!({
                "env": {
                    "ANTHROPIC_API_KEY": "secret",
                    "ANTHROPIC_MODEL": "claude-sonnet-4-6",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-haiku-4-5"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let result = live.import_draft("claude");

        assert!(matches!(result, Err(LiveError::UnsupportedConfig(_))));
    }

    #[test]
    fn claude_cloud_and_root_model_overrides_are_rejected() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(
            &paths.claude_settings,
            r#"{"model":"opus","env":{"ANTHROPIC_API_KEY":"secret","CLAUDE_CODE_USE_BEDROCK":"1"}}"#,
        )
        .unwrap();

        assert!(matches!(
            live.import_draft("claude"),
            Err(LiveError::UnsupportedConfig(_))
        ));
        assert!(matches!(
            live.switch(&provider("claude", json!({"apiKey": "new"}))),
            Err(LiveError::UnsupportedConfig(_))
        ));
    }

    #[test]
    fn import_rejects_claude_bearer_auth_mode() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(
            &paths.claude_settings,
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"secret"}}"#,
        )
        .unwrap();

        assert!(matches!(
            live.import_draft("claude"),
            Err(LiveError::UnsupportedConfig(_))
        ));
    }

    #[test]
    fn claude_settings_path_does_not_fall_back_to_an_unowned_legacy_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        fs::create_dir_all(&live.claude_dir).unwrap();
        fs::write(live.claude_dir.join("claude.json"), "{}").unwrap();

        assert!(live.paths().claude_settings.ends_with("settings.json"));
        assert!(matches!(
            live.import_draft("claude"),
            Err(LiveError::Missing(_))
        ));
    }

    #[test]
    fn config_roots_require_absolute_paths_and_allow_missing_directories() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let default = directory.path().join("default");

        assert!(matches!(
            config_root(Some(OsStr::new("relative")), &default, "CODEX_HOME"),
            Err(LiveError::InvalidConfig(_))
        ));
        assert!(matches!(
            config_root(Some(OsStr::new("~/.codex")), &default, "CODEX_HOME"),
            Err(LiveError::InvalidConfig(_))
        ));
        assert_eq!(
            config_root(None, &default, "CODEX_HOME").unwrap(),
            fs::canonicalize(directory.path()).unwrap().join("default")
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_roots_resolve_existing_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let actual = directory.path().join("actual");
        let linked = directory.path().join("linked");
        fs::create_dir(&actual).unwrap();
        symlink(&actual, &linked).unwrap();

        assert_eq!(
            config_root(Some(linked.as_os_str()), &actual, "CODEX_HOME").unwrap(),
            fs::canonicalize(actual).unwrap()
        );
    }

    #[test]
    fn invalid_codex_toml_does_not_echo_a_secret() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let paths = live.paths();
        fs::create_dir_all(paths.codex_config.parent().unwrap()).unwrap();
        fs::write(
            &paths.codex_config,
            "experimental_bearer_token = \"must-not-echo\n",
        )
        .unwrap();

        let error = live.import_draft("codex").unwrap_err().to_string();

        assert!(!error.contains("must-not-echo"));
    }

    #[test]
    fn plugin_route_cannot_write_without_approval() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let path = live.paths().claude_settings;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{}").unwrap();
        let route = PluginRoute {
            api_key: "secret".to_owned(),
            base_url: None,
            model: None,
        };

        let provider = provider("claude", json!({"apiKey": "stored"}));
        let result = live.execute_plugin_route::<()>(&provider, &[], |_| Ok(route));

        assert!(matches!(result, Err(LiveError::InvalidProvider(_))));
        assert_eq!(fs::read_to_string(path).unwrap(), "{}");
    }

    #[test]
    fn approved_plugin_route_is_merged_by_the_host() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let path = live.paths().claude_settings;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"hooks":{"SessionStart":[{"command":"keep"}]}}"#).unwrap();
        let route = PluginRoute {
            api_key: "secret".to_owned(),
            base_url: Some("https://gateway.example".to_owned()),
            model: None,
        };
        let provider = provider("claude", json!({"apiKey": "stored"}));

        live.execute_plugin_route::<()>(
            &provider,
            &[PluginCapability::WriteClaudeSettings],
            |_| Ok(route),
        )
        .expect("live operation")
        .expect("plugin router");

        let written: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(written["hooks"]["SessionStart"][0]["command"], "keep");
        assert_eq!(written["env"]["ANTHROPIC_API_KEY"], "secret");
        assert_eq!(
            written["env"]["ANTHROPIC_BASE_URL"],
            "https://gateway.example"
        );
    }

    #[test]
    fn plugin_route_values_are_validated_by_the_host() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let path = live.paths().claude_settings;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{}").unwrap();
        let route = PluginRoute {
            api_key: "secret".to_owned(),
            base_url: Some("http://remote.example".to_owned()),
            model: None,
        };
        let provider = provider("claude", json!({"apiKey": "stored"}));

        let result = live.execute_plugin_route::<()>(
            &provider,
            &[PluginCapability::WriteClaudeSettings],
            |_| Ok(route),
        );

        assert!(matches!(result, Err(LiveError::InvalidProvider(_))));
        assert_eq!(fs::read_to_string(path).unwrap(), "{}");
    }

    #[cfg(unix)]
    #[test]
    fn plugin_snapshot_rejects_a_symbolic_link() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let live = live(directory.path());
        let path = live.paths().claude_settings;
        let secret = directory.path().join("secret.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&secret, r#"{"secret":"must-not-read"}"#).unwrap();
        symlink(&secret, &path).unwrap();

        let result = live.with_plugin_snapshots::<(), ()>(
            "claude",
            &[PluginCapability::ReadClaudeSettings],
            |_| panic!("plugin must not receive a symlink target"),
        );

        assert!(matches!(
            result,
            Err(LiveError::Operation(OperationError::InvalidTarget(_)))
        ));
    }
}
