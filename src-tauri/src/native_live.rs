use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsStr,
    path::{Path, PathBuf},
};

use cc_switch_core::{
    builtin_app_adapter, claude, claude_desktop, codex, common_config, gemini, grokbuild, hermes,
    openclaw, opencode, pi, AppType, ProviderEntry,
};
use json_five::rt::parser::{
    from_str as parse_round_trip_json5, JSONKeyValuePair, JSONObjectContext, JSONText, JSONValue,
    KeyValuePairContext,
};
use serde_json::{json, Map, Value};

use crate::{
    live::LiveError,
    operation::{
        duplicate_yaml_top_level_key, read_optional, ContentExpectation, LivePaths, LogicalTarget,
        OperationPlan, PlannedWrite, OPERATION_CONTRACT_MAJOR,
    },
    provider::{native_adapter_reference, NativeImport, ProviderDraft, ProviderRecord},
};

const CLAUDE_DESKTOP_PROFILE_ID: &str = "00000000-0000-4000-8000-000000157210";
const CLAUDE_DESKTOP_PROFILE_NAME: &str = "CC Switch";
const CLAUDE_DESKTOP_OFFICIAL_ID: &str = "claude-desktop-official";
const GEMINI_OFFICIAL_PARTNER_KEY: &str = "google-official";
const HERMES_SOURCE_FIELD: &str = "_cc_source";
const HERMES_CUSTOM_SOURCE: &str = "custom_providers";
const HERMES_DICT_SOURCE: &str = "providers_dict";
const OPENCLAW_DEFAULT_SOURCE: &str =
    "{\n  models: {\n    mode: 'merge',\n    providers: {},\n  },\n}\n";

pub struct NativeLiveConfig {
    paths: LivePaths,
}

impl NativeLiveConfig {
    pub fn from_home(
        home: &Path,
        claude_dir: PathBuf,
        codex_dir: PathBuf,
    ) -> Result<Self, LiveError> {
        let hermes_dir = config_root(
            std::env::var_os("HERMES_HOME").as_deref(),
            &default_hermes_dir(home),
            "HERMES_HOME",
        )?;
        let pi_dir = config_root(
            std::env::var_os("PI_CODING_AGENT_DIR").as_deref(),
            &home.join(".pi").join("agent"),
            "PI_CODING_AGENT_DIR",
        )?;
        let (normal, threep, profile, meta) = claude_desktop_paths(home);
        Ok(Self {
            paths: LivePaths {
                claude_settings: claude_dir.join("settings.json"),
                claude_desktop_normal_config: normal,
                claude_desktop_threep_config: threep,
                claude_desktop_profile: profile,
                claude_desktop_meta: meta,
                codex_auth: codex_dir.join("auth.json"),
                codex_config: codex_dir.join("config.toml"),
                codex_model_catalog: codex_dir.join(codex::MODEL_CATALOG_FILENAME),
                gemini_env: home.join(".gemini").join(".env"),
                gemini_settings: home.join(".gemini").join("settings.json"),
                grok_config: home.join(".grok").join("config.toml"),
                opencode_config: home.join(".config").join("opencode").join("opencode.json"),
                openclaw_config: home.join(".openclaw").join("openclaw.json"),
                hermes_config: hermes_dir.join("config.yaml"),
                pi_models: pi_dir.join("models.json"),
            },
        })
    }

    #[cfg(test)]
    pub fn for_tests(home: &Path, claude_dir: PathBuf, codex_dir: PathBuf) -> Self {
        let (normal, threep, profile, meta) = test_claude_desktop_paths(home);
        Self {
            paths: LivePaths {
                claude_settings: claude_dir.join("settings.json"),
                claude_desktop_normal_config: normal,
                claude_desktop_threep_config: threep,
                claude_desktop_profile: profile,
                claude_desktop_meta: meta,
                codex_auth: codex_dir.join("auth.json"),
                codex_config: codex_dir.join("config.toml"),
                codex_model_catalog: codex_dir.join(codex::MODEL_CATALOG_FILENAME),
                gemini_env: home.join(".gemini/.env"),
                gemini_settings: home.join(".gemini/settings.json"),
                grok_config: home.join(".grok/config.toml"),
                opencode_config: home.join(".config/opencode/opencode.json"),
                openclaw_config: home.join(".openclaw/openclaw.json"),
                hermes_config: home.join(".hermes/config.yaml"),
                pi_models: home.join(".pi/agent/models.json"),
            },
        }
    }

    pub fn paths(&self) -> LivePaths {
        self.paths.clone()
    }

    pub fn resolved_for_app(&self, app: AppType) -> Result<Self, LiveError> {
        let mut paths = self.paths.clone();
        for target in builtin_app_adapter(&app).targets() {
            paths = paths.resolved_for_write(*target)?;
        }
        Ok(Self { paths })
    }

    pub fn import_drafts(&self, app: AppType) -> Result<Vec<NativeImport>, LiveError> {
        match app {
            AppType::Claude => self.import_claude(),
            AppType::Codex => self.import_codex(),
            AppType::Gemini => self.import_gemini(),
            AppType::GrokBuild => self.import_grokbuild(),
            AppType::OpenCode => self.import_json_entries(
                app.clone(),
                &self.paths.opencode_config,
                &["provider"],
                "OpenCode",
                |key, value| opencode::prepare_provider_entry(key, value).map(drop),
            ),
            AppType::OpenClaw => self.import_json_entries(
                app,
                &self.paths.openclaw_config,
                &["models", "providers"],
                "OpenClaw",
                |key, value| openclaw::prepare_provider_entry(key, value).map(drop),
            ),
            AppType::ClaudeDesktop => self.import_claude_desktop(),
            AppType::Hermes => self.import_hermes(),
            AppType::Pi => self.import_json_entries(
                app,
                &self.paths.pi_models,
                &["providers"],
                "Pi",
                |key, value| pi::prepare_provider_entry(key, value).map(drop),
            ),
        }
    }

    pub fn apply_plan(
        &self,
        provider: &ProviderRecord,
        common_snippet: Option<&str>,
    ) -> Result<OperationPlan, LiveError> {
        let app = provider
            .app_id
            .parse::<AppType>()
            .map_err(|_| LiveError::InvalidProvider("application is not supported".to_owned()))?;
        ensure_native_category_supported(&app, provider)?;
        let stored_settings = Value::Object(provider.settings.clone());
        let common_enabled = provider
            .metadata
            .get("commonConfigEnabled")
            .and_then(Value::as_bool)
            == Some(true);
        let settings = common_config::apply(&app, &stored_settings, common_snippet, common_enabled)
            .map_err(|error| invalid_provider(error.to_string()))?;
        match app {
            AppType::Claude => {
                let snapshot = claude::prepare_live_snapshot(&settings)
                    .map_err(|error| invalid_provider(error.to_string()))?;
                self.single_write(
                    app,
                    LogicalTarget::ClaudeSettings,
                    pretty_json(&snapshot.settings)?,
                )
            }
            AppType::Codex => {
                let snapshot = codex::prepare_strict_live_snapshot(&settings)
                    .map_err(|error| invalid_provider(error.to_string()))?;
                let config_original = read_optional(&self.paths.codex_config)?;
                let auth_original = read_optional(&self.paths.codex_auth)?;
                let config = codex::prepare_provider_live_config(
                    &snapshot.auth,
                    snapshot.config.as_deref().unwrap_or_default(),
                )
                .map_err(|error| invalid_provider(error.to_string()))?;
                let catalog = codex::prepare_native_model_catalog(
                    &settings,
                    &config,
                    codex::NativeCatalogOwnership::default(),
                )
                .map_err(|error| invalid_provider(error.to_string()))?;
                let catalog_managed = catalog.managed;
                let config =
                    preserve_live_mcp_toml(config_original.as_deref(), &catalog.config, "Codex")?;
                let mut writes = Vec::with_capacity(3);
                let category = is_official(provider, "codex-official").then_some("official");
                if codex::should_write_auth(category, &snapshot.auth, true) {
                    writes.push(planned(
                        LogicalTarget::CodexAuth,
                        auth_original.as_deref(),
                        Some(pretty_json(&snapshot.auth)?),
                    ));
                } else if category == Some("official")
                    && parse_optional_json_object(auth_original.as_deref(), "Codex auth.json")?
                        .as_ref()
                        .is_some_and(codex::live_auth_is_stale_third_party_residue)
                {
                    writes.push(planned(
                        LogicalTarget::CodexAuth,
                        auth_original.as_deref(),
                        None,
                    ));
                }
                writes.push(planned(
                    LogicalTarget::CodexConfig,
                    config_original.as_deref(),
                    Some(config),
                ));
                if catalog_managed {
                    let catalog_original = read_optional(&self.paths.codex_model_catalog)?;
                    writes.push(planned(
                        LogicalTarget::CodexModelCatalog,
                        catalog_original.as_deref(),
                        catalog.catalog.as_ref().map(pretty_json).transpose()?,
                    ));
                }
                self.plan(app, writes)
            }
            AppType::Gemini => {
                let env_original = read_optional(&self.paths.gemini_env)?;
                let settings_original = read_optional(&self.paths.gemini_settings)?;
                let existing = parse_optional_json_object(
                    settings_original.as_deref(),
                    "Gemini settings.json",
                )?;
                let mode = gemini_mode(provider);
                let snapshot = gemini::prepare_live_snapshot(&settings, existing.as_ref(), mode)
                    .map_err(|error| invalid_provider(error.to_string()))?;
                self.plan(
                    app,
                    vec![
                        planned(
                            LogicalTarget::GeminiEnv,
                            env_original.as_deref(),
                            Some(serialize_env(&snapshot.env)?),
                        ),
                        planned(
                            LogicalTarget::GeminiSettings,
                            settings_original.as_deref(),
                            Some(pretty_json(&snapshot.settings)?),
                        ),
                    ],
                )
            }
            AppType::GrokBuild => {
                let mode = if is_official(provider, "grokbuild-official") {
                    grokbuild::ProviderMode::Official
                } else {
                    grokbuild::ProviderMode::Custom
                };
                let snapshot = grokbuild::prepare_live_snapshot(&settings, mode)
                    .map_err(|error| invalid_provider(error.to_string()))?;
                let original = read_optional(&self.paths.grok_config)?;
                let config =
                    preserve_live_mcp_toml(original.as_deref(), &snapshot.config, "Grok Build")?;
                self.plan(
                    app,
                    vec![planned(
                        LogicalTarget::GrokConfig,
                        original.as_deref(),
                        Some(config),
                    )],
                )
            }
            AppType::OpenCode => {
                let entry = opencode::prepare_provider_entry(&provider.id, &settings)
                    .map_err(|error| invalid_provider(error.to_string()))?;
                self.json_entry_plan(app, entry, &["provider"], JsonRoot::Empty)
            }
            AppType::OpenClaw => {
                let entry = openclaw::prepare_provider_entry(&provider.id, &settings)
                    .map_err(|error| invalid_provider(error.to_string()))?;
                self.json_entry_plan(app, entry, &["models", "providers"], JsonRoot::OpenClaw)
            }
            AppType::ClaudeDesktop => self.claude_desktop_plan(provider, &settings),
            AppType::Hermes => self.hermes_plan(provider),
            AppType::Pi => {
                let entry = pi::prepare_provider_entry(&provider.id, &settings)
                    .map_err(|error| invalid_provider(error.to_string()))?;
                self.json_entry_plan(app, entry, &["providers"], JsonRoot::Empty)
            }
        }
    }

    pub fn remove_plan(&self, provider: &ProviderRecord) -> Result<OperationPlan, LiveError> {
        let app = provider
            .app_id
            .parse::<AppType>()
            .map_err(|_| LiveError::InvalidProvider("application is not supported".to_owned()))?;
        ensure_native_category_supported(&app, provider)?;
        match app {
            AppType::OpenCode => {
                self.json_remove_plan(app, &provider.id, &["provider"], JsonRoot::Empty)
            }
            AppType::OpenClaw => self.json_remove_plan(
                app,
                &provider.id,
                &["models", "providers"],
                JsonRoot::OpenClaw,
            ),
            AppType::Hermes => self.hermes_remove_plan(provider),
            AppType::Pi => {
                self.json_remove_plan(app, &provider.id, &["providers"], JsonRoot::Empty)
            }
            _ => Err(LiveError::InvalidProvider(
                "only additive native providers are removed from live configuration".to_owned(),
            )),
        }
    }

    fn import_claude(&self) -> Result<Vec<NativeImport>, LiveError> {
        let settings = read_required_json_object(&self.paths.claude_settings, "Claude Code")?;
        let snapshot = claude::prepare_live_snapshot(&settings)
            .map_err(|error| LiveError::InvalidConfig(error.to_string()))?;
        Ok(vec![native_draft(
            AppType::Claude,
            "default".to_owned(),
            "Imported Claude Code".to_owned(),
            snapshot.settings,
        )?])
    }

    fn import_codex(&self) -> Result<Vec<NativeImport>, LiveError> {
        let config = read_text_optional(&self.paths.codex_config, "Codex")?;
        let auth = read_json_object_optional(&self.paths.codex_auth, "Codex")?;
        if config.is_none() && auth.is_none() {
            return Err(LiveError::Missing("Codex".to_owned()));
        }
        let settings = json!({
            "auth": auth.unwrap_or_else(|| json!({})),
            "config": config.unwrap_or_default()
        });
        codex::prepare_strict_live_snapshot(&settings)
            .map_err(|error| LiveError::InvalidConfig(error.to_string()))?;
        let official = codex_auth_is_official(&settings["auth"]);
        Ok(vec![native_draft_with_metadata(
            AppType::Codex,
            if official {
                "codex-official".to_owned()
            } else {
                "default".to_owned()
            },
            "Imported Codex".to_owned(),
            settings,
            Some(if official { "official" } else { "custom" }.to_owned()),
            json!({}),
        )?])
    }

    fn import_gemini(&self) -> Result<Vec<NativeImport>, LiveError> {
        let env_text = read_text_optional(&self.paths.gemini_env, "Gemini")?;
        let config = read_json_object_optional(&self.paths.gemini_settings, "Gemini")?;
        if env_text.is_none() && config.is_none() {
            return Err(LiveError::Missing("Gemini".to_owned()));
        }
        let env = parse_env(env_text.as_deref().unwrap_or_default())?;
        let settings = json!({"env": env, "config": config.unwrap_or_else(|| json!({}))});
        let mode = if settings
            .pointer("/config/security/auth/selectedType")
            .and_then(Value::as_str)
            == Some("oauth-personal")
        {
            gemini::AuthMode::OAuthPersonal
        } else {
            gemini::AuthMode::ApiKey
        };
        gemini::prepare_live_snapshot(&settings, None, mode)
            .map_err(|error| LiveError::InvalidConfig(error.to_string()))?;
        let official = mode == gemini::AuthMode::OAuthPersonal;
        Ok(vec![native_draft_with_metadata(
            AppType::Gemini,
            if official {
                "gemini-official".to_owned()
            } else {
                "default".to_owned()
            },
            if official {
                "Google".to_owned()
            } else {
                "Imported Gemini".to_owned()
            },
            settings,
            Some(if official { "official" } else { "custom" }.to_owned()),
            json!({}),
        )?])
    }

    fn import_grokbuild(&self) -> Result<Vec<NativeImport>, LiveError> {
        let config = read_text_optional(&self.paths.grok_config, "Grok Build")?
            .ok_or_else(|| LiveError::Missing("Grok Build".to_owned()))?;
        let settings = json!({"config": config});
        let mode = if grok_config_is_official(settings["config"].as_str().unwrap_or_default()) {
            grokbuild::ProviderMode::Official
        } else {
            grokbuild::ProviderMode::Custom
        };
        grokbuild::prepare_live_snapshot(&settings, mode)
            .map_err(|error| LiveError::InvalidConfig(error.to_string()))?;
        let official = mode == grokbuild::ProviderMode::Official;
        Ok(vec![native_draft_with_metadata(
            AppType::GrokBuild,
            if official {
                "grokbuild-official".to_owned()
            } else {
                "default".to_owned()
            },
            "Imported Grok Build".to_owned(),
            settings,
            Some(if official { "official" } else { "custom" }.to_owned()),
            json!({}),
        )?])
    }

    fn import_json_entries<E>(
        &self,
        app: AppType,
        path: &Path,
        keys: &[&str],
        label: &str,
        validate: impl Fn(&str, &Value) -> Result<(), E>,
    ) -> Result<Vec<NativeImport>, LiveError> {
        let root = read_required_json5_object(path, label)?;
        let entries =
            nested_object(&root, keys).ok_or_else(|| LiveError::Missing(label.to_owned()))?;
        let mut drafts = Vec::with_capacity(entries.len());
        for (key, settings) in entries {
            if validate(key, settings).is_err() {
                return Err(LiveError::InvalidConfig(format!(
                    "{label} provider '{key}' is invalid"
                )));
            }
            let name = settings
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(key)
                .to_owned();
            drafts.push(native_draft(
                app.clone(),
                key.clone(),
                name,
                settings.clone(),
            )?);
        }
        if drafts.is_empty() {
            return Err(LiveError::Missing(format!("{label} providers")));
        }
        Ok(drafts)
    }

    fn import_claude_desktop(&self) -> Result<Vec<NativeImport>, LiveError> {
        ensure_claude_desktop_supported()?;
        let profile =
            read_json_object_optional(&self.paths.claude_desktop_profile, "Claude Desktop")?;
        let Some(profile) = profile else {
            let normal = read_json_object_optional(
                &self.paths.claude_desktop_normal_config,
                "Claude Desktop",
            )?;
            let threep = read_json_object_optional(
                &self.paths.claude_desktop_threep_config,
                "Claude Desktop",
            )?;
            let official = [normal.as_ref(), threep.as_ref()]
                .into_iter()
                .flatten()
                .any(|value| value.get("deploymentMode").and_then(Value::as_str) == Some("1p"));
            if !official {
                return Err(LiveError::Missing(
                    "Claude Desktop direct profile".to_owned(),
                ));
            }
            return Ok(vec![native_draft_with_metadata(
                AppType::ClaudeDesktop,
                CLAUDE_DESKTOP_OFFICIAL_ID.to_owned(),
                "Claude Desktop Official".to_owned(),
                json!({}),
                Some("official".to_owned()),
                json!({}),
            )?]);
        };
        let base_url = profile
            .get("inferenceGatewayBaseUrl")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                LiveError::InvalidConfig("Claude Desktop profile has no gateway URL".to_owned())
            })?;
        let token = profile
            .get("inferenceGatewayApiKey")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                LiveError::InvalidConfig("Claude Desktop profile has no gateway token".to_owned())
            })?;
        let settings = json!({
            "env": {
                "ANTHROPIC_BASE_URL": base_url,
                "ANTHROPIC_AUTH_TOKEN": token
            }
        });
        let (route_metadata, routes) = desktop_routes_from_profile(&profile)?;
        claude_desktop::prepare_live_action(
            &settings,
            claude_desktop::ProviderMode::Direct,
            Some(&routes),
        )
        .map_err(|error| LiveError::InvalidConfig(error.to_string()))?;
        Ok(vec![native_draft_with_metadata(
            AppType::ClaudeDesktop,
            "default".to_owned(),
            "Imported Claude Desktop".to_owned(),
            settings,
            Some("custom".to_owned()),
            json!({
                "claudeDesktopMode": "direct",
                "claudeDesktopModelRoutes": route_metadata
            }),
        )?])
    }

    fn import_hermes(&self) -> Result<Vec<NativeImport>, LiveError> {
        let raw = read_text_optional(&self.paths.hermes_config, "Hermes")?
            .ok_or_else(|| LiveError::Missing("Hermes".to_owned()))?;
        let root = parse_yaml(&raw, "Hermes")?;
        let mut providers = Map::new();
        if let Some(section) = root.get("custom_providers") {
            let entries = section.as_sequence().ok_or_else(|| {
                LiveError::InvalidConfig("Hermes custom_providers must be a sequence".to_owned())
            })?;
            for entry in entries {
                let name = entry
                    .get("name")
                    .and_then(serde_yaml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        LiveError::InvalidConfig(
                            "Hermes custom_providers contains an unnamed provider".to_owned(),
                        )
                    })?;
                let mut value = yaml_to_json(entry)?;
                if !value.is_object() {
                    return Err(LiveError::InvalidConfig(format!(
                        "Hermes provider '{name}' is invalid"
                    )));
                }
                denormalize_hermes_models(&mut value);
                if let Some(object) = value.as_object_mut() {
                    object.insert(HERMES_SOURCE_FIELD.to_owned(), json!(HERMES_CUSTOM_SOURCE));
                }
                if providers.insert(name.to_owned(), value).is_some() {
                    return Err(LiveError::InvalidConfig(format!(
                        "Hermes provider '{name}' is defined more than once"
                    )));
                }
            }
        }
        if let Some(section) = root.get("providers") {
            let entries = section.as_mapping().ok_or_else(|| {
                LiveError::InvalidConfig("Hermes providers must be a mapping".to_owned())
            })?;
            let mut dictionary_names = HashSet::new();
            for (key, entry) in entries {
                let key = key
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        LiveError::InvalidConfig(
                            "Hermes providers contains an invalid provider key".to_owned(),
                        )
                    })?;
                let mut value = yaml_to_json(entry)?;
                let object = value.as_object_mut().ok_or_else(|| {
                    LiveError::InvalidConfig(format!("Hermes provider '{key}' is invalid"))
                })?;
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(key)
                    .to_owned();
                if !dictionary_names.insert(name.clone()) {
                    return Err(LiveError::InvalidConfig(format!(
                        "Hermes provider '{name}' is defined more than once"
                    )));
                }
                if providers.contains_key(&name) {
                    continue;
                }
                object.insert("name".to_owned(), json!(name));
                object.insert("provider_key".to_owned(), json!(key));
                object.insert(HERMES_SOURCE_FIELD.to_owned(), json!(HERMES_DICT_SOURCE));
                denormalize_hermes_models(&mut value);
                providers.insert(name, value);
            }
        }
        let mut drafts = Vec::with_capacity(providers.len());
        for (name, settings) in providers {
            if hermes::prepare_provider_entry(&name, &settings).is_err() {
                return Err(LiveError::InvalidConfig(format!(
                    "Hermes provider '{name}' is invalid"
                )));
            }
            drafts.push(native_draft(AppType::Hermes, name.clone(), name, settings)?);
        }
        if drafts.is_empty() {
            return Err(LiveError::Missing("Hermes providers".to_owned()));
        }
        Ok(drafts)
    }

    fn json_entry_plan(
        &self,
        app: AppType,
        entry: ProviderEntry,
        keys: &[&str],
        default: JsonRoot,
    ) -> Result<OperationPlan, LiveError> {
        let target = json_target(&app)?;
        let path = self.paths.path_for(target);
        let original = read_optional(path)?;
        let mut root = parse_json5_object(original.as_deref(), app.as_str(), default.value())?;
        ensure_nested_object(&mut root, keys)?.insert(entry.key, entry.config);
        let contents = serialize_json_root(app.clone(), &root, original.as_deref())?;
        self.plan(
            app,
            vec![PlannedWrite {
                target,
                expected: ContentExpectation::for_contents(original.as_deref()),
                contents: Some(contents),
            }],
        )
    }

    fn json_remove_plan(
        &self,
        app: AppType,
        key: &str,
        keys: &[&str],
        default: JsonRoot,
    ) -> Result<OperationPlan, LiveError> {
        let target = json_target(&app)?;
        let path = self.paths.path_for(target);
        let original = read_optional(path)?;
        let mut root = parse_json5_object(original.as_deref(), app.as_str(), default.value())?;
        if let Some(entries) = nested_object_mut(&mut root, keys) {
            entries.remove(key);
        }
        let contents = serialize_json_root(app.clone(), &root, original.as_deref())?;
        self.plan(
            app,
            vec![PlannedWrite {
                target,
                expected: ContentExpectation::for_contents(original.as_deref()),
                contents: Some(contents),
            }],
        )
    }

    fn hermes_plan(&self, provider: &ProviderRecord) -> Result<OperationPlan, LiveError> {
        if provider
            .settings
            .get(HERMES_SOURCE_FIELD)
            .and_then(Value::as_str)
            == Some(HERMES_DICT_SOURCE)
        {
            return Err(LiveError::InvalidProvider(
                "this Hermes provider is managed by the native providers dictionary".to_owned(),
            ));
        }
        let settings = Value::Object(provider.settings.clone());
        let entry = hermes::prepare_provider_entry(&provider.id, &settings)
            .map_err(|error| invalid_provider(error.to_string()))?;
        let original = read_optional(&self.paths.hermes_config)?;
        let raw = optional_utf8(original.as_deref(), "Hermes")?;
        let root = parse_yaml(raw, "Hermes")?;
        if hermes_dict_only(&root, &provider.id) {
            return Err(LiveError::InvalidProvider(
                "this Hermes provider is managed by the native providers dictionary".to_owned(),
            ));
        }
        let mut providers = root
            .get("custom_providers")
            .and_then(serde_yaml::Value::as_sequence)
            .cloned()
            .unwrap_or_default();
        let mut next = json_to_yaml(&entry.config)?;
        if let Some(existing) = providers.iter_mut().find(|value| {
            value.get("name").and_then(serde_yaml::Value::as_str) == Some(provider.id.as_str())
        }) {
            if let (Some(existing), Some(next)) = (existing.as_mapping(), next.as_mapping_mut()) {
                for (key, value) in existing {
                    next.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
            *existing = next;
        } else {
            providers.push(next);
        }
        let contents = replace_yaml_section(
            raw,
            "custom_providers",
            &serde_yaml::Value::Sequence(providers),
        )?;
        let current_model = root.get("model").map(yaml_to_json).transpose()?;
        let model = hermes::prepare_model_defaults(&provider.id, &settings, current_model.as_ref())
            .map_err(|error| invalid_provider(error.to_string()))?;
        let model = json_to_yaml(&model)?;
        let contents = replace_yaml_section(&contents, "model", &model)?;
        self.plan(
            AppType::Hermes,
            vec![PlannedWrite {
                target: LogicalTarget::HermesConfig,
                expected: ContentExpectation::for_contents(original.as_deref()),
                contents: Some(contents),
            }],
        )
    }

    fn hermes_remove_plan(&self, provider: &ProviderRecord) -> Result<OperationPlan, LiveError> {
        if provider
            .settings
            .get(HERMES_SOURCE_FIELD)
            .and_then(Value::as_str)
            == Some(HERMES_DICT_SOURCE)
        {
            return Err(LiveError::InvalidProvider(
                "this Hermes provider is managed by the native providers dictionary".to_owned(),
            ));
        }
        let original = read_optional(&self.paths.hermes_config)?;
        let raw = optional_utf8(original.as_deref(), "Hermes")?;
        let root = parse_yaml(raw, "Hermes")?;
        if hermes_dict_only(&root, &provider.id) {
            return Err(LiveError::InvalidProvider(
                "this Hermes provider is managed by the native providers dictionary".to_owned(),
            ));
        }
        let providers = root
            .get("custom_providers")
            .and_then(serde_yaml::Value::as_sequence)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|value| {
                value.get("name").and_then(serde_yaml::Value::as_str) != Some(provider.id.as_str())
            })
            .collect();
        let contents = replace_yaml_section(
            raw,
            "custom_providers",
            &serde_yaml::Value::Sequence(providers),
        )?;
        let contents = match root.get("model").and_then(serde_yaml::Value::as_mapping) {
            Some(model)
                if model.get("provider").and_then(serde_yaml::Value::as_str)
                    == Some(provider.id.as_str()) =>
            {
                let mut model = model.clone();
                model.remove("provider");
                model.remove("default");
                replace_yaml_section(&contents, "model", &serde_yaml::Value::Mapping(model))?
            }
            _ => contents,
        };
        self.plan(
            AppType::Hermes,
            vec![PlannedWrite {
                target: LogicalTarget::HermesConfig,
                expected: ContentExpectation::for_contents(original.as_deref()),
                contents: Some(contents),
            }],
        )
    }

    fn claude_desktop_plan(
        &self,
        provider: &ProviderRecord,
        settings: &Value,
    ) -> Result<OperationPlan, LiveError> {
        ensure_claude_desktop_supported()?;
        let official = is_official(provider, CLAUDE_DESKTOP_OFFICIAL_ID);
        let mode = provider
            .metadata
            .get("claudeDesktopMode")
            .and_then(Value::as_str);
        if !official && mode == Some("proxy") {
            return Err(LiveError::InvalidProvider(
                "Claude Desktop proxy providers are not available in Lite".to_owned(),
            ));
        }
        let routes = desktop_routes(&provider.metadata)?;
        let action = claude_desktop::prepare_live_action(
            settings,
            if official {
                claude_desktop::ProviderMode::Official
            } else {
                claude_desktop::ProviderMode::Direct
            },
            Some(&routes),
        )
        .map_err(|error| invalid_provider(error.to_string()))?;
        let normal_original = read_optional(&self.paths.claude_desktop_normal_config)?;
        let threep_original = read_optional(&self.paths.claude_desktop_threep_config)?;
        let profile_original = read_optional(&self.paths.claude_desktop_profile)?;
        let meta_original = read_optional(&self.paths.claude_desktop_meta)?;
        let mut normal = parse_json_object_or_empty(normal_original.as_deref(), "Claude Desktop")?;
        let mut threep = parse_json_object_or_empty(threep_original.as_deref(), "Claude Desktop")?;
        let mut meta = parse_json_object_or_empty(meta_original.as_deref(), "Claude Desktop")?;
        let mut writes = Vec::with_capacity(4);

        match action {
            claude_desktop::PreparedLiveAction::RestoreOfficial => {
                normal.insert("deploymentMode".to_owned(), json!("1p"));
                threep.insert("deploymentMode".to_owned(), json!("1p"));
                remove_desktop_enterprise_config(&mut threep);
                update_desktop_meta(&mut meta, false);
                writes.push(planned(
                    LogicalTarget::ClaudeDesktopProfile,
                    profile_original.as_deref(),
                    None,
                ));
            }
            claude_desktop::PreparedLiveAction::ApplyDirect { profile } => {
                normal.insert("deploymentMode".to_owned(), json!("3p"));
                threep.insert("deploymentMode".to_owned(), json!("3p"));
                update_desktop_meta(&mut meta, true);
                writes.push(planned(
                    LogicalTarget::ClaudeDesktopProfile,
                    profile_original.as_deref(),
                    Some(pretty_json(&profile)?),
                ));
            }
        }
        writes.push(planned(
            LogicalTarget::ClaudeDesktopNormalConfig,
            normal_original.as_deref(),
            Some(pretty_json(&Value::Object(normal))?),
        ));
        writes.push(planned(
            LogicalTarget::ClaudeDesktopThreepConfig,
            threep_original.as_deref(),
            Some(pretty_json(&Value::Object(threep))?),
        ));
        writes.push(planned(
            LogicalTarget::ClaudeDesktopMeta,
            meta_original.as_deref(),
            Some(pretty_json(&Value::Object(meta))?),
        ));
        self.plan(AppType::ClaudeDesktop, writes)
    }

    fn single_write(
        &self,
        app: AppType,
        target: LogicalTarget,
        contents: String,
    ) -> Result<OperationPlan, LiveError> {
        self.plan(app, vec![self.write(target, Some(contents))?])
    }

    fn write(
        &self,
        target: LogicalTarget,
        contents: Option<String>,
    ) -> Result<PlannedWrite, LiveError> {
        let original = read_optional(self.paths.path_for(target))?;
        Ok(planned(target, original.as_deref(), contents))
    }

    fn plan(&self, app: AppType, writes: Vec<PlannedWrite>) -> Result<OperationPlan, LiveError> {
        Ok(OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: app.as_str().to_owned(),
            writes,
        })
    }
}

fn native_draft(
    app: AppType,
    native_id: String,
    name: String,
    settings: Value,
) -> Result<NativeImport, LiveError> {
    native_draft_with_metadata(app, native_id, name, settings, None, json!({}))
}

fn native_draft_with_metadata(
    app: AppType,
    native_id: String,
    name: String,
    settings: Value,
    category: Option<String>,
    metadata: Value,
) -> Result<NativeImport, LiveError> {
    let name_is_explicit = app.is_additive_mode()
        && settings
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|name| !name.is_empty());
    let settings = settings.as_object().cloned().ok_or_else(|| {
        LiveError::InvalidConfig("native provider settings must be an object".to_owned())
    })?;
    Ok(NativeImport {
        native_id,
        draft: ProviderDraft {
            app_id: app.as_str().to_owned(),
            adapter: native_adapter_reference(&app),
            name,
            settings,
        },
        name_is_explicit,
        category,
        metadata,
    })
}

fn invalid_provider(message: String) -> LiveError {
    LiveError::InvalidProvider(message)
}

fn planned(
    target: LogicalTarget,
    original: Option<&[u8]>,
    contents: Option<String>,
) -> PlannedWrite {
    PlannedWrite {
        target,
        expected: ContentExpectation::for_contents(original),
        contents,
    }
}

fn read_text_optional(path: &Path, label: &str) -> Result<Option<String>, LiveError> {
    read_optional(path)?
        .map(|contents| {
            String::from_utf8(contents)
                .map_err(|_| LiveError::InvalidConfig(format!("{label} config is not UTF-8")))
        })
        .transpose()
}

fn optional_utf8<'a>(contents: Option<&'a [u8]>, label: &str) -> Result<&'a str, LiveError> {
    match contents {
        Some(contents) => std::str::from_utf8(contents)
            .map_err(|_| LiveError::InvalidConfig(format!("{label} config is not UTF-8"))),
        None => Ok(""),
    }
}

fn read_json_object_optional(path: &Path, label: &str) -> Result<Option<Value>, LiveError> {
    read_optional(path)?
        .map(|contents| parse_json_value(&contents, label))
        .transpose()
}

fn read_required_json_object(path: &Path, label: &str) -> Result<Value, LiveError> {
    read_json_object_optional(path, label)?.ok_or_else(|| LiveError::Missing(label.to_owned()))
}

fn parse_json_value(contents: &[u8], label: &str) -> Result<Value, LiveError> {
    let value: Value = serde_json::from_slice(contents)
        .map_err(|_| LiveError::InvalidConfig(format!("{label} JSON could not be parsed")))?;
    if !value.is_object() {
        return Err(LiveError::InvalidConfig(format!(
            "{label} JSON root must be an object"
        )));
    }
    Ok(value)
}

fn read_required_json5_object(path: &Path, label: &str) -> Result<Map<String, Value>, LiveError> {
    let contents = read_optional(path)?.ok_or_else(|| LiveError::Missing(label.to_owned()))?;
    parse_json5_object(Some(&contents), label, Value::Object(Map::new()))
}

fn parse_json5_object(
    contents: Option<&[u8]>,
    label: &str,
    default: Value,
) -> Result<Map<String, Value>, LiveError> {
    let value = match contents {
        Some(contents) => {
            let text = std::str::from_utf8(contents)
                .map_err(|_| LiveError::InvalidConfig(format!("{label} config is not UTF-8")))?;
            json5::from_str::<Value>(text).map_err(|_| {
                LiveError::InvalidConfig(format!("{label} JSON5 could not be parsed"))
            })?
        }
        None => default,
    };
    value.as_object().cloned().ok_or_else(|| {
        LiveError::InvalidConfig(format!("{label} configuration root must be an object"))
    })
}

fn parse_json_object_or_empty(
    contents: Option<&[u8]>,
    label: &str,
) -> Result<Map<String, Value>, LiveError> {
    let Some(contents) = contents else {
        return Ok(Map::new());
    };
    let value: Value = serde_json::from_slice(contents)
        .map_err(|_| LiveError::InvalidConfig(format!("{label} JSON could not be parsed")))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| LiveError::InvalidConfig(format!("{label} JSON root must be an object")))
}

fn parse_optional_json_object(
    contents: Option<&[u8]>,
    label: &str,
) -> Result<Option<Value>, LiveError> {
    contents
        .map(|contents| parse_json_value(contents, label))
        .transpose()
}

fn pretty_json(value: &Value) -> Result<String, LiveError> {
    let mut contents = serde_json::to_string_pretty(value)
        .map_err(|_| LiveError::InvalidConfig("JSON could not be serialized".to_owned()))?;
    contents.push('\n');
    Ok(contents)
}

fn preserve_live_mcp_toml(
    original: Option<&[u8]>,
    provider_config: &str,
    label: &str,
) -> Result<String, LiveError> {
    let mut next = provider_config
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| {
            LiveError::InvalidProvider(format!("{label} provider TOML could not be parsed"))
        })?;
    let current = original
        .map(|contents| {
            let contents = std::str::from_utf8(contents)
                .map_err(|_| LiveError::InvalidConfig(format!("{label} live TOML is not UTF-8")))?;
            contents.parse::<toml_edit::DocumentMut>().map_err(|_| {
                LiveError::InvalidConfig(format!("{label} live TOML could not be parsed"))
            })
        })
        .transpose()?;

    next.as_table_mut().remove("mcp_servers");
    if let Some(mcp) = next
        .get_mut("mcp")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        mcp.remove("servers");
        if mcp.is_empty() {
            next.as_table_mut().remove("mcp");
        }
    }

    let Some(current) = current else {
        return Ok(next.to_string());
    };
    if let Some(servers) = current.get("mcp_servers") {
        next.as_table_mut().insert("mcp_servers", servers.clone());
    }
    if let Some(servers) = current
        .get("mcp")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|mcp| mcp.get("servers"))
    {
        if next.get("mcp").is_none() {
            next["mcp"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let mcp = next
            .get_mut("mcp")
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or_else(|| {
                LiveError::InvalidProvider(format!("{label} provider mcp field must be a table"))
            })?;
        mcp.insert("servers", servers.clone());
    }
    Ok(next.to_string())
}

fn parse_env(contents: &str) -> Result<Map<String, Value>, LiveError> {
    let mut env = Map::new();
    for (index, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(|| {
            LiveError::InvalidConfig(format!(
                "Gemini .env line {} has no '=' separator",
                index + 1
            ))
        })?;
        let key = key.trim();
        if !valid_env_key(key) {
            return Err(LiveError::InvalidConfig(format!(
                "Gemini .env line {} has an invalid variable name",
                index + 1
            )));
        }
        let value = value.trim();
        if value.contains(['\r', '\n', '\0']) {
            return Err(LiveError::InvalidConfig(format!(
                "Gemini .env line {} has an invalid value",
                index + 1
            )));
        }
        env.insert(key.to_owned(), Value::String(value.to_owned()));
    }
    Ok(env)
}

fn serialize_env(env: &BTreeMap<String, String>) -> Result<String, LiveError> {
    let mut lines = Vec::with_capacity(env.len());
    for (key, value) in env {
        if !valid_env_key(key) || value.contains(['\r', '\n', '\0']) {
            return Err(LiveError::InvalidProvider(
                "Gemini environment contains an unsafe key or value".to_owned(),
            ));
        }
        lines.push(format!("{key}={value}"));
    }
    Ok(lines.join("\n"))
}

fn valid_env_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn nested_object<'a>(
    root: &'a Map<String, Value>,
    keys: &[&str],
) -> Option<&'a Map<String, Value>> {
    let mut current = root;
    for key in keys {
        current = current.get(*key)?.as_object()?;
    }
    Some(current)
}

fn nested_object_mut<'a>(
    root: &'a mut Map<String, Value>,
    keys: &[&str],
) -> Option<&'a mut Map<String, Value>> {
    let mut current = root;
    for key in keys {
        current = current.get_mut(*key)?.as_object_mut()?;
    }
    Some(current)
}

fn ensure_nested_object<'a>(
    root: &'a mut Map<String, Value>,
    keys: &[&str],
) -> Result<&'a mut Map<String, Value>, LiveError> {
    let mut current = root;
    for key in keys {
        let value = current
            .entry((*key).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        current = value.as_object_mut().ok_or_else(|| {
            LiveError::InvalidConfig("native provider container must be an object".to_owned())
        })?;
    }
    Ok(current)
}

enum JsonRoot {
    Empty,
    OpenClaw,
}

impl JsonRoot {
    fn value(&self) -> Value {
        match self {
            Self::Empty => json!({}),
            Self::OpenClaw => json!({"models": {"mode": "merge", "providers": {}}}),
        }
    }
}

fn serialize_json_root(
    app: AppType,
    root: &Map<String, Value>,
    original: Option<&[u8]>,
) -> Result<String, LiveError> {
    if app != AppType::OpenClaw {
        return pretty_json(&Value::Object(root.clone()));
    }
    let source = optional_utf8(original, "OpenClaw")?;
    let source = if source.trim().is_empty() {
        OPENCLAW_DEFAULT_SOURCE
    } else {
        source
    };
    let models = root
        .get("models")
        .ok_or_else(|| LiveError::InvalidConfig("OpenClaw models section is missing".to_owned()))?;
    replace_json5_root_section(source, "models", models)
}

fn replace_json5_root_section(source: &str, key: &str, value: &Value) -> Result<String, LiveError> {
    let mut text: JSONText = parse_round_trip_json5(source).map_err(|_| {
        LiveError::InvalidConfig("OpenClaw round-trip JSON5 could not be parsed".to_owned())
    })?;
    let JSONValue::JSONObject {
        key_value_pairs,
        context,
    } = &mut text.value
    else {
        return Err(LiveError::InvalidConfig(
            "OpenClaw configuration root must be an object".to_owned(),
        ));
    };
    if key_value_pairs.is_empty()
        && context
            .as_ref()
            .is_none_or(|context| context.wsc.0.is_empty())
    {
        *context = Some(JSONObjectContext {
            wsc: ("\n  ".to_owned(),),
        });
    }
    let leading = context
        .as_ref()
        .map(|context| context.wsc.0.clone())
        .unwrap_or_default();
    let separator = if leading.contains('\n') {
        format!("\n{}", trailing_indent(&leading))
    } else {
        String::new()
    };
    let value = json_to_round_trip_value(value, &trailing_indent(&leading))?;
    if let Some(existing) = key_value_pairs
        .iter_mut()
        .find(|pair| json5_key_name(&pair.key) == Some(key))
    {
        existing.value = value;
        return Ok(text.to_string());
    }
    let closing = if let Some(last) = key_value_pairs.last_mut() {
        let context = last.context.get_or_insert_with(|| KeyValuePairContext {
            wsc: (String::new(), " ".to_owned(), String::new(), None),
        });
        if let Some(after_comma) = context.wsc.3.clone() {
            context.wsc.3 = Some(separator);
            after_comma
        } else {
            let closing = std::mem::take(&mut context.wsc.2);
            context.wsc.3 = Some(separator);
            closing
        }
    } else {
        closing_whitespace(&leading)
    };
    key_value_pairs.push(JSONKeyValuePair {
        key: json5_key(key),
        value,
        context: Some(KeyValuePairContext {
            wsc: (String::new(), " ".to_owned(), closing, None),
        }),
    });
    Ok(text.to_string())
}

fn json_to_round_trip_value(value: &Value, parent_indent: &str) -> Result<JSONValue, LiveError> {
    let source = serde_json::to_string_pretty(value).map_err(|_| {
        LiveError::InvalidConfig("OpenClaw models could not be serialized".to_owned())
    })?;
    let adjusted = if parent_indent.is_empty() || !source.contains('\n') {
        source
    } else {
        let mut lines = source.lines();
        let mut adjusted = lines.next().unwrap_or_default().to_owned();
        for line in lines {
            adjusted.push('\n');
            adjusted.push_str(parent_indent);
            adjusted.push_str(line);
        }
        adjusted
    };
    parse_round_trip_json5(&adjusted)
        .map(|text| text.value)
        .map_err(|_| LiveError::InvalidConfig("OpenClaw models could not be projected".to_owned()))
}

fn trailing_indent(value: &str) -> String {
    value
        .rsplit_once('\n')
        .map(|(_, indent)| indent.to_owned())
        .unwrap_or_default()
}

fn closing_whitespace(value: &str) -> String {
    let Some((prefix, indent)) = value.rsplit_once('\n') else {
        return String::new();
    };
    let indent = indent
        .strip_suffix('\t')
        .or_else(|| indent.strip_suffix("  "))
        .or_else(|| indent.strip_suffix(' '))
        .unwrap_or(indent);
    format!("{prefix}\n{indent}")
}

fn json5_key(key: &str) -> JSONValue {
    let mut chars = key.chars();
    let identifier = chars
        .next()
        .is_some_and(|first| matches!(first, 'a'..='z' | 'A'..='Z' | '_' | '$'))
        && chars
            .all(|character| matches!(character, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '$'));
    if identifier {
        JSONValue::Identifier(key.to_owned())
    } else {
        JSONValue::DoubleQuotedString(key.to_owned())
    }
}

fn json5_key_name(key: &JSONValue) -> Option<&str> {
    match key {
        JSONValue::Identifier(value)
        | JSONValue::DoubleQuotedString(value)
        | JSONValue::SingleQuotedString(value) => Some(value),
        _ => None,
    }
}

fn json_target(app: &AppType) -> Result<LogicalTarget, LiveError> {
    match app {
        AppType::OpenCode => Ok(LogicalTarget::OpenCodeConfig),
        AppType::OpenClaw => Ok(LogicalTarget::OpenClawConfig),
        AppType::Pi => Ok(LogicalTarget::PiModels),
        _ => Err(LiveError::InvalidProvider(
            "application is not a JSON additive target".to_owned(),
        )),
    }
}

fn gemini_mode(provider: &ProviderRecord) -> gemini::AuthMode {
    let official_meta = provider
        .metadata
        .get("partnerPromotionKey")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(GEMINI_OFFICIAL_PARTNER_KEY));
    if provider.id == "gemini-official"
        || provider.category.as_deref() == Some("official")
        || official_meta
    {
        gemini::AuthMode::OAuthPersonal
    } else {
        gemini::AuthMode::ApiKey
    }
}

fn ensure_native_category_supported(
    app: &AppType,
    provider: &ProviderRecord,
) -> Result<(), LiveError> {
    if *app == AppType::OpenCode
        && matches!(provider.category.as_deref(), Some("omo") | Some("omo-slim"))
    {
        return Err(LiveError::InvalidProvider(
            "OMO and OMO Slim use dedicated OpenCode configuration and are not managed by Lite"
                .to_owned(),
        ));
    }
    Ok(())
}

fn is_official(provider: &ProviderRecord, official_id: &str) -> bool {
    provider.id == official_id || provider.category.as_deref() == Some("official")
}

fn grok_config_is_official(config: &str) -> bool {
    config
        .parse::<toml_edit::DocumentMut>()
        .is_ok_and(|document| !document.contains_key("models") && !document.contains_key("model"))
}

fn codex_auth_is_official(auth: &Value) -> bool {
    let has_api_key = auth
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    !has_api_key && codex::auth_has_login_material(auth)
}

fn desktop_routes_from_profile(
    profile: &Value,
) -> Result<(Map<String, Value>, Vec<claude_desktop::DirectModelRoute>), LiveError> {
    let mut metadata = Map::new();
    if let Some(entries) = profile.get("inferenceModels") {
        let entries = entries.as_array().ok_or_else(|| {
            LiveError::InvalidConfig("Claude Desktop inferenceModels must be an array".to_owned())
        })?;
        for entry in entries {
            let (name, label, supports_1m) = match entry {
                Value::String(name) => (name.as_str(), None, false),
                Value::Object(entry) => {
                    let name = entry.get("name").and_then(Value::as_str).ok_or_else(|| {
                        LiveError::InvalidConfig(
                            "Claude Desktop model route is missing its name".to_owned(),
                        )
                    })?;
                    (
                        name,
                        entry.get("labelOverride").and_then(Value::as_str),
                        entry
                            .get("supports1m")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    )
                }
                _ => {
                    return Err(LiveError::InvalidConfig(
                        "Claude Desktop model route is invalid".to_owned(),
                    ));
                }
            };
            let name = name.trim();
            if name.is_empty() {
                return Err(LiveError::InvalidConfig(
                    "Claude Desktop model route is empty".to_owned(),
                ));
            }
            let mut route = json!({"model": name});
            if let Some(label) = label.map(str::trim).filter(|value| !value.is_empty()) {
                route["labelOverride"] = json!(label);
            }
            if supports_1m {
                route["supports1m"] = json!(true);
            }
            metadata.insert(name.to_owned(), route);
        }
    }
    let routes = desktop_routes(&json!({"claudeDesktopModelRoutes": metadata.clone()}))?;
    Ok((metadata, routes))
}

fn desktop_routes(metadata: &Value) -> Result<Vec<claude_desktop::DirectModelRoute>, LiveError> {
    metadata
        .get("claudeDesktopModelRoutes")
        .and_then(Value::as_object)
        .map(|routes| {
            routes
                .iter()
                .map(|(route_id, value)| {
                    let model = value.get("model").and_then(Value::as_str).ok_or_else(|| {
                        LiveError::InvalidProvider(
                            "Claude Desktop model route is missing its upstream model".to_owned(),
                        )
                    })?;
                    Ok(claude_desktop::DirectModelRoute {
                        route_id: route_id.clone(),
                        upstream_model: model.to_owned(),
                        label_override: value
                            .get("labelOverride")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        supports_1m: value
                            .get("supports1m")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn remove_desktop_enterprise_config(root: &mut Map<String, Value>) {
    let Some(enterprise) = root
        .get_mut("enterpriseConfig")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    for key in [
        "disableDeploymentModeChooser",
        "inferenceGatewayApiKey",
        "inferenceGatewayAuthScheme",
        "inferenceGatewayBaseUrl",
        "inferenceProvider",
    ] {
        enterprise.remove(key);
    }
    if enterprise.is_empty() {
        root.remove("enterpriseConfig");
    }
}

fn update_desktop_meta(root: &mut Map<String, Value>, apply: bool) {
    let mut entries = root
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    entries
        .retain(|entry| entry.get("id").and_then(Value::as_str) != Some(CLAUDE_DESKTOP_PROFILE_ID));
    if apply {
        entries.push(json!({
            "id": CLAUDE_DESKTOP_PROFILE_ID,
            "name": CLAUDE_DESKTOP_PROFILE_NAME
        }));
        root.insert("appliedId".to_owned(), json!(CLAUDE_DESKTOP_PROFILE_ID));
    } else if root.get("appliedId").and_then(Value::as_str) == Some(CLAUDE_DESKTOP_PROFILE_ID) {
        match entries
            .iter()
            .find_map(|entry| entry.get("id").and_then(Value::as_str))
        {
            Some(id) => {
                root.insert("appliedId".to_owned(), json!(id));
            }
            None => {
                root.remove("appliedId");
            }
        }
    }
    root.insert("entries".to_owned(), Value::Array(entries));
}

fn parse_yaml(contents: &str, label: &str) -> Result<serde_yaml::Value, LiveError> {
    if contents.trim().is_empty() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }
    if let Some(key) = duplicate_yaml_top_level_key(contents) {
        return Err(LiveError::InvalidConfig(format!(
            "{label} YAML contains duplicate top-level key '{key}'"
        )));
    }
    let value: serde_yaml::Value = serde_yaml::from_str(contents)
        .map_err(|_| LiveError::InvalidConfig(format!("{label} YAML could not be parsed")))?;
    if !value.is_mapping() {
        return Err(LiveError::InvalidConfig(format!(
            "{label} YAML root must be a mapping"
        )));
    }
    Ok(value)
}

fn yaml_to_json(value: &serde_yaml::Value) -> Result<Value, LiveError> {
    serde_json::to_value(value)
        .map_err(|_| LiveError::InvalidConfig("Hermes provider is not JSON-compatible".to_owned()))
}

fn json_to_yaml(value: &Value) -> Result<serde_yaml::Value, LiveError> {
    serde_yaml::to_value(value).map_err(|_| {
        LiveError::InvalidProvider("Hermes provider could not be serialized".to_owned())
    })
}

fn denormalize_hermes_models(value: &mut Value) {
    let Some(models) = value
        .as_object_mut()
        .and_then(|object| object.get_mut("models"))
    else {
        return;
    };
    let Some(entries) = models.as_object() else {
        return;
    };
    *models = Value::Array(
        entries
            .iter()
            .filter_map(|(id, value)| {
                let mut value = match value {
                    Value::Object(value) => value.clone(),
                    Value::Null => Map::new(),
                    _ => return None,
                };
                value.insert("id".to_owned(), json!(id));
                Some(Value::Object(value))
            })
            .collect(),
    );
}

fn hermes_dict_only(root: &serde_yaml::Value, name: &str) -> bool {
    let list_has = root
        .get("custom_providers")
        .and_then(serde_yaml::Value::as_sequence)
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.get("name").and_then(serde_yaml::Value::as_str) == Some(name))
        });
    !list_has
        && root
            .get("providers")
            .and_then(serde_yaml::Value::as_mapping)
            .is_some_and(|entries| {
                entries.iter().any(|(key, value)| {
                    key.as_str() == Some(name)
                        || value.get("name").and_then(serde_yaml::Value::as_str) == Some(name)
                })
            })
}

fn replace_yaml_section(
    raw: &str,
    key: &str,
    value: &serde_yaml::Value,
) -> Result<String, LiveError> {
    let mut section = serde_yaml::Mapping::new();
    section.insert(serde_yaml::Value::String(key.to_owned()), value.clone());
    let serialized = serde_yaml::to_string(&serde_yaml::Value::Mapping(section))
        .map_err(|_| LiveError::InvalidConfig("Hermes YAML could not be serialized".to_owned()))?;
    let Some((start, end)) = yaml_section_range(raw, key) else {
        let mut result = raw.to_owned();
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(&serialized);
        return Ok(result);
    };
    let mut result = String::with_capacity(raw.len() + serialized.len());
    result.push_str(&raw[..start]);
    result.push_str(&serialized);
    result.push_str(&remove_yaml_sections(&raw[end..], key));
    Ok(result)
}

fn remove_yaml_sections(raw: &str, key: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut remaining = raw;
    while let Some((start, end)) = yaml_section_range(remaining, key) {
        result.push_str(&remaining[..start]);
        remaining = &remaining[end..];
    }
    result.push_str(remaining);
    result
}

fn yaml_section_range(raw: &str, key: &str) -> Option<(usize, usize)> {
    let mut start = None;
    let mut offset = 0;
    for line in raw.split('\n') {
        let top_level = !line.is_empty()
            && !line.starts_with([' ', '\t', '#', '-'])
            && line.find(':').is_some_and(|index| {
                let suffix = &line[index + 1..];
                suffix.is_empty() || suffix.starts_with([' ', '\t', '\r'])
            });
        if start.is_none() && top_level && yaml_key_matches(line, key) {
            start = Some(offset);
        } else if start.is_some() && top_level {
            return Some((start.unwrap(), offset));
        }
        offset += line.len() + 1;
    }
    start.map(|start| (start, raw.len()))
}

fn yaml_key_matches(line: &str, key: &str) -> bool {
    let quoted = format!("\"{key}\"");
    let single_quoted = format!("'{key}'");
    let matches = [key, quoted.as_str(), single_quoted.as_str()]
        .into_iter()
        .any(|candidate| {
            line.strip_prefix(candidate)
                .map(str::trim_start)
                .and_then(|suffix| suffix.strip_prefix(':'))
                .is_some_and(|suffix| {
                    suffix.is_empty() || suffix.chars().next().is_some_and(char::is_whitespace)
                })
        });
    matches
}

fn config_root(
    override_value: Option<&OsStr>,
    default: &Path,
    variable: &str,
) -> Result<PathBuf, LiveError> {
    let path = override_value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default.to_owned());
    if !path.is_absolute() {
        return Err(LiveError::InvalidConfig(format!(
            "{variable} must be an absolute path"
        )));
    }
    Ok(path)
}

#[cfg(target_os = "windows")]
fn default_hermes_dir(home: &Path) -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Local"))
        .join("hermes")
}

#[cfg(not(target_os = "windows"))]
fn default_hermes_dir(home: &Path) -> PathBuf {
    home.join(".hermes")
}

#[cfg(target_os = "macos")]
fn claude_desktop_paths(home: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let support = home.join("Library/Application Support");
    desktop_paths_from_dirs(support.join("Claude"), support.join("Claude-3p"))
}

#[cfg(target_os = "windows")]
fn claude_desktop_paths(home: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Local"));
    let normal = windows_claude_dir(&local, false).unwrap_or_else(|| local.join("Claude"));
    let threep = windows_claude_dir(&local, true).unwrap_or_else(|| local.join("Claude-3p"));
    desktop_paths_from_dirs(normal, threep)
}

#[cfg(target_os = "windows")]
fn windows_claude_dir(local: &Path, threep: bool) -> Option<PathBuf> {
    let exact = local.join(if threep { "Claude-3p" } else { "Claude" });
    if exact.exists() {
        return Some(exact);
    }
    let mut candidates = std::fs::read_dir(local)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("Claude") && name.contains("-3p") == threep)
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.into_iter().next()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn claude_desktop_paths(home: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    test_claude_desktop_paths(&home.join(".config/cc-switch-lite/unsupported-desktop"))
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "windows"))))]
fn test_claude_desktop_paths(home: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    desktop_paths_from_dirs(home.join("Claude"), home.join("Claude-3p"))
}

fn desktop_paths_from_dirs(
    normal: PathBuf,
    threep: PathBuf,
) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let library = threep.join("configLibrary");
    (
        normal.join("claude_desktop_config.json"),
        threep.join("claude_desktop_config.json"),
        library.join(format!("{CLAUDE_DESKTOP_PROFILE_ID}.json")),
        library.join("_meta.json"),
    )
}

fn ensure_claude_desktop_supported() -> Result<(), LiveError> {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        Ok(())
    } else {
        Err(LiveError::InvalidProvider(
            "Claude Desktop configuration is supported on macOS and Windows".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::OperationExecutor;

    fn provider(app: AppType, id: &str, settings: Value) -> ProviderRecord {
        ProviderRecord {
            id: id.to_owned(),
            revision: 1,
            app_id: app.as_str().to_owned(),
            adapter: native_adapter_reference(&app),
            name: id.to_owned(),
            settings: settings.as_object().unwrap().clone(),
            category: None,
            metadata: json!({}),
            extensions: Map::new(),
        }
    }

    fn imported_provider(imported: NativeImport) -> ProviderRecord {
        ProviderRecord {
            id: imported.native_id,
            revision: 1,
            app_id: imported.draft.app_id,
            adapter: imported.draft.adapter,
            name: imported.draft.name,
            settings: imported.draft.settings,
            category: imported.category,
            metadata: imported.metadata,
            extensions: Map::new(),
        }
    }

    #[test]
    fn additive_json_plans_preserve_unrelated_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.opencode_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.opencode_config,
            r#"{"theme":"dark","provider":{"existing":{"options":{}}}}"#,
        )
        .unwrap();

        let plan = native
            .apply_plan(
                &provider(
                    AppType::OpenCode,
                    "new",
                    json!({"npm": "@ai-sdk/openai-compatible"}),
                ),
                None,
            )
            .unwrap();
        let written: Value =
            serde_json::from_str(plan.writes[0].contents.as_deref().unwrap()).unwrap();

        assert_eq!(written["theme"], "dark");
        assert!(written["provider"]["existing"].is_object());
        assert_eq!(
            written["provider"]["new"]["npm"],
            "@ai-sdk/openai-compatible"
        );
    }

    #[test]
    fn additive_import_keeps_native_keys() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.pi_models.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.pi_models,
            r#"{providers:{anthropic:{oauth:"native"},custom:{models:[]}}}"#,
        )
        .unwrap();

        let drafts = native.import_drafts(AppType::Pi).unwrap();
        assert_eq!(
            drafts
                .iter()
                .map(|draft| draft.native_id.as_str())
                .collect::<Vec<_>>(),
            ["anthropic", "custom"]
        );
    }

    #[test]
    fn additive_import_rejects_a_partial_batch() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.pi_models.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.pi_models,
            r#"{providers:{valid:{models:[]},invalid:42}}"#,
        )
        .unwrap();

        assert!(matches!(
            native.import_drafts(AppType::Pi),
            Err(LiveError::InvalidConfig(_))
        ));
    }

    #[test]
    fn openclaw_plan_preserves_comments_outside_models() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.openclaw_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.openclaw_config,
            "{\n  // keep this comment\n  tools: { profile: 'coding' },\n  models: { mode: 'merge', providers: {} },\n}\n",
        )
        .unwrap();

        let plan = native
            .apply_plan(
                &provider(
                    AppType::OpenClaw,
                    "new",
                    json!({"models": [{"id": "model"}]}),
                ),
                None,
            )
            .unwrap();
        let contents = plan.writes[0].contents.as_deref().unwrap();

        assert!(contents.contains("// keep this comment"));
        assert!(contents.contains("tools: { profile: 'coding' }"));
        let parsed: Value = json5::from_str(contents).unwrap();
        assert_eq!(
            parsed["models"]["providers"]["new"]["models"][0]["id"],
            "model"
        );

        OperationExecutor::new(&native.paths)
            .execute(&plan)
            .expect("execute JSON5 plan");
        assert!(std::fs::read_to_string(&native.paths.openclaw_config)
            .unwrap()
            .contains("// keep this comment"));
    }

    #[test]
    fn official_imports_keep_stable_native_identity() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.gemini_settings.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.gemini_settings,
            r#"{"security":{"auth":{"selectedType":"oauth-personal"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(native.paths.grok_config.parent().unwrap()).unwrap();
        std::fs::write(&native.paths.grok_config, "# official\n").unwrap();

        let gemini = native.import_drafts(AppType::Gemini).unwrap().remove(0);
        let grok = native.import_drafts(AppType::GrokBuild).unwrap().remove(0);

        assert_eq!(gemini.native_id, "gemini-official");
        assert_eq!(gemini.category.as_deref(), Some("official"));
        assert_eq!(grok.native_id, "grokbuild-official");
        assert_eq!(grok.category.as_deref(), Some("official"));
        native
            .apply_plan(&imported_provider(gemini), None)
            .expect("reapply imported Gemini OAuth provider");
        native
            .apply_plan(&imported_provider(grok), None)
            .expect("reapply imported Grok official provider");
    }

    #[test]
    fn gemini_env_rejects_malformed_imports_and_multiline_values() {
        assert!(parse_env("GEMINI_API_KEY=ok\nbroken").is_err());

        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        let result = native.apply_plan(
            &provider(
                AppType::Gemini,
                "default",
                json!({"env": {"GEMINI_API_KEY": "ok\nEVIL=1"}, "config": {}}),
            ),
            None,
        );
        assert!(matches!(result, Err(LiveError::InvalidProvider(_))));
    }

    #[test]
    fn live_mcp_toml_is_preserved_without_accepting_provider_mcp() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(
            &path,
            "# live comment\nmodel = \"old\"\n\n[mcp_servers.echo]\ncommand = \"echo\"\n\n[mcp.servers.nested]\ncommand = \"nested\"\n",
        )
        .unwrap();

        let original = std::fs::read(&path).unwrap();
        let next = preserve_live_mcp_toml(
            Some(&original),
            "model = \"new\"\n\n[mcp_servers.stale]\ncommand = \"stale\"\n",
            "Codex",
        )
        .unwrap();

        assert!(next.contains("model = \"new\""));
        assert!(next.contains("[mcp_servers.echo]"));
        assert!(next.contains("command = \"echo\""));
        assert!(next.contains("[mcp.servers.nested]"));
        assert!(!next.contains("stale"));
    }

    #[test]
    fn codex_switch_preserves_oauth_and_projects_catalog_common_config_and_mcp() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.codex_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.codex_auth,
            r#"{"tokens":{"access_token":"oauth-login"}}"#,
        )
        .unwrap();
        std::fs::write(
            &native.paths.codex_config,
            "model = \"old\"\n[mcp_servers.keep]\ncommand = \"keep\"\n",
        )
        .unwrap();
        let mut provider = provider(
            AppType::Codex,
            "custom",
            json!({
                "auth": {"OPENAI_API_KEY": "provider-secret"},
                "config": "model = \"qwen3-coder-plus\"\nmodel_provider = \"custom\"\n[model_providers.custom]\nbase_url = \"https://example.com\"\n",
                "modelCatalog": {"models": [{"model": "qwen3-coder-plus"}]}
            }),
        );
        provider.category = Some("custom".to_owned());
        provider.metadata = json!({"commonConfigEnabled": true});

        let plan = native
            .apply_plan(&provider, Some("[features]\nkeep = true\n"))
            .expect("valid Codex plan");
        assert!(plan
            .writes
            .iter()
            .all(|write| write.target != LogicalTarget::CodexAuth));
        let config = plan
            .writes
            .iter()
            .find(|write| write.target == LogicalTarget::CodexConfig)
            .and_then(|write| write.contents.as_deref())
            .expect("Codex config write");
        assert!(config.contains("experimental_bearer_token = \"provider-secret\""));
        assert!(config.contains("[mcp_servers.keep]"));
        assert!(config.contains("[features]"));
        assert!(config.contains("model_catalog_json = \"cc-switch-model-catalog.json\""));
        assert!(plan
            .writes
            .iter()
            .any(|write| write.target == LogicalTarget::CodexModelCatalog));

        OperationExecutor::new(&native.paths)
            .execute(&plan)
            .expect("execute Codex plan");
        assert!(std::fs::read_to_string(&native.paths.codex_auth)
            .unwrap()
            .contains("oauth-login"));
        assert!(native.paths.codex_model_catalog.exists());
    }

    #[test]
    fn clearing_an_explicit_codex_catalog_removes_the_lite_managed_file() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.codex_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.codex_config,
            "model = \"old\"\nmodel_catalog_json = \"cc-switch-model-catalog.json\"\n",
        )
        .unwrap();
        std::fs::write(
            &native.paths.codex_model_catalog,
            "{\"models\":[{\"model\":\"old\"}]}\n",
        )
        .unwrap();
        let provider = provider(
            AppType::Codex,
            "custom",
            json!({
                "auth": {},
                "config": "model = \"gpt-5\"\n",
                "modelCatalog": {"models": []}
            }),
        );

        let plan = native.apply_plan(&provider, None).expect("Codex plan");
        let catalog = plan
            .writes
            .iter()
            .find(|write| write.target == LogicalTarget::CodexModelCatalog)
            .expect("managed catalog cleanup");
        assert!(catalog.contents.is_none());

        OperationExecutor::new(&native.paths)
            .execute(&plan)
            .expect("execute Codex plan");
        assert!(!native.paths.codex_model_catalog.exists());
    }

    #[test]
    fn official_codex_switch_removes_only_stale_api_key_auth() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.codex_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.codex_auth,
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"old-third-party"}"#,
        )
        .unwrap();
        let mut official = provider(
            AppType::Codex,
            "codex-official",
            json!({"auth": {}, "config": "model = \"gpt-5\"\n"}),
        );
        official.category = Some("official".to_owned());

        let plan = native.apply_plan(&official, None).expect("official plan");
        let auth = plan
            .writes
            .iter()
            .find(|write| write.target == LogicalTarget::CodexAuth)
            .expect("stale auth cleanup");
        assert!(auth.contents.is_none());
    }

    #[test]
    fn hermes_switch_updates_runtime_model_in_the_same_plan() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.hermes_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.hermes_config,
            "model:\n  provider: old\n  default: old-model\n  context_length: 32000\ncustom_providers: []\n",
        )
        .unwrap();

        let plan = native
            .apply_plan(
                &provider(
                    AppType::Hermes,
                    "new",
                    json!({"base_url": "https://example.com", "models": [{"id": "new-model"}]}),
                ),
                None,
            )
            .expect("Hermes plan");
        let contents = plan.writes[0].contents.as_deref().unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(contents).unwrap();

        assert_eq!(parsed["model"]["provider"].as_str(), Some("new"));
        assert_eq!(parsed["model"]["default"].as_str(), Some("new-model"));
        assert_eq!(parsed["model"]["context_length"].as_i64(), Some(32000));
    }

    #[test]
    fn hermes_removal_clears_a_model_reference_to_the_removed_provider() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.hermes_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.hermes_config,
            "model:\n  provider: old\n  default: old-model\n  context_length: 32000\ncustom_providers:\n  - name: old\n    base_url: https://old.example.com\n    models:\n      old-model: {}\n  - name: keep\n    base_url: https://keep.example.com\n",
        )
        .unwrap();

        let plan = native
            .remove_plan(&provider(
                AppType::Hermes,
                "old",
                json!({"base_url": "https://old.example.com"}),
            ))
            .expect("Hermes removal plan");
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(plan.writes[0].contents.as_deref().unwrap()).unwrap();

        assert!(parsed["model"].get("provider").is_none());
        assert!(parsed["model"].get("default").is_none());
        assert_eq!(parsed["model"]["context_length"].as_i64(), Some(32000));
        assert_eq!(parsed["custom_providers"].as_sequence().unwrap().len(), 1);
        assert_eq!(parsed["custom_providers"][0]["name"].as_str(), Some("keep"));
    }

    #[test]
    fn hermes_dictionary_providers_remain_visible_and_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.hermes_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.hermes_config,
            "providers:\n  anthropic:\n    base_url: https://api.anthropic.com\n    models:\n      claude-opus: {}\n",
        )
        .unwrap();

        let imported = native.import_drafts(AppType::Hermes).unwrap().remove(0);
        assert_eq!(
            imported
                .draft
                .settings
                .get(HERMES_SOURCE_FIELD)
                .and_then(Value::as_str),
            Some(HERMES_DICT_SOURCE)
        );
        let provider = imported_provider(imported);
        assert!(matches!(
            native.apply_plan(&provider, None),
            Err(LiveError::InvalidProvider(_))
        ));
        assert!(matches!(
            native.remove_plan(&provider),
            Err(LiveError::InvalidProvider(_))
        ));
    }

    #[test]
    fn hermes_custom_provider_wins_over_a_dictionary_name_collision() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.hermes_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.hermes_config,
            "custom_providers:\n  - name: shared\n    base_url: https://writable.example.com\n    models:\n      writable: {}\nproviders:\n  shadow:\n    name: shared\n    base_url: https://readonly.example.com\n    models:\n      readonly: {}\n",
        )
        .unwrap();

        let imported = native.import_drafts(AppType::Hermes).unwrap();

        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].native_id, "shared");
        assert_eq!(
            imported[0]
                .draft
                .settings
                .get(HERMES_SOURCE_FIELD)
                .and_then(Value::as_str),
            Some(HERMES_CUSTOM_SOURCE)
        );
        native
            .apply_plan(
                &imported_provider(imported.into_iter().next().unwrap()),
                None,
            )
            .expect("custom provider remains writable");
    }

    #[test]
    fn gemini_custom_provider_name_does_not_select_oauth() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        let mut custom = provider(
            AppType::Gemini,
            "google-proxy",
            json!({"env": {"GEMINI_API_KEY": "secret"}, "config": {}}),
        );
        custom.name = "Google Proxy".to_owned();
        custom.category = Some("custom".to_owned());

        let plan = native.apply_plan(&custom, None).expect("Gemini plan");
        let settings: Value = serde_json::from_str(
            plan.writes
                .iter()
                .find(|write| write.target == LogicalTarget::GeminiSettings)
                .and_then(|write| write.contents.as_deref())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings.pointer("/security/auth/selectedType"),
            Some(&json!("gemini-api-key"))
        );
    }

    #[test]
    fn desktop_non_object_json_and_omo_categories_are_rejected() {
        assert!(parse_json_object_or_empty(Some(b"[]"), "Claude Desktop").is_err());

        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        let mut omo = provider(AppType::OpenCode, "omo", json!({"npm": "special"}));
        omo.category = Some("omo".to_owned());
        assert!(matches!(
            native.apply_plan(&omo, None),
            Err(LiveError::InvalidProvider(_))
        ));
        assert!(matches!(
            native.remove_plan(&omo),
            Err(LiveError::InvalidProvider(_))
        ));
    }

    #[test]
    fn desktop_profile_routes_round_trip_into_internal_metadata() {
        let (metadata, routes) = desktop_routes_from_profile(&json!({
            "inferenceModels": [
                "claude-sonnet-4-6",
                {"name": "claude-opus-4-6", "labelOverride": "Opus", "supports1m": true}
            ]
        }))
        .unwrap();

        assert_eq!(metadata["claude-opus-4-6"]["labelOverride"], "Opus");
        assert_eq!(routes.len(), 2);
    }

    #[test]
    fn hermes_section_replacement_preserves_other_sections() {
        let raw = "# keep\nmodel:\n  default: old\ncustom_providers:\n  - name: old\nmcp_servers:\n  keep: true\n";
        let next = replace_yaml_section(
            raw,
            "custom_providers",
            &serde_yaml::from_str("- name: new\n").unwrap(),
        )
        .unwrap();

        assert!(next.contains("# keep"));
        assert!(next.contains("model:\n  default: old"));
        assert!(next.contains("mcp_servers:\n  keep: true"));
        assert!(next.contains("name: new"));
        assert!(!next.contains("name: old"));
    }

    #[test]
    fn hermes_section_replacement_recognizes_quoted_top_level_keys() {
        for raw in [
            "\"custom_providers\":\n  - name: old\nmodel: {}\n",
            "'custom_providers':\n  - name: old\nmodel: {}\n",
        ] {
            let next = replace_yaml_section(
                raw,
                "custom_providers",
                &serde_yaml::from_str("- name: new\n").unwrap(),
            )
            .unwrap();
            let parsed = parse_yaml(&next, "Hermes").unwrap();

            assert_eq!(parsed["custom_providers"][0]["name"], "new");
            assert!(!next.contains("name: old"));
        }
    }

    #[test]
    fn hermes_parser_rejects_duplicate_top_level_sections() {
        let parsed = parse_yaml(
            "custom_providers:\n  - name: old\ncustom_providers:\n  - name: new\n",
            "Hermes",
        );

        assert!(matches!(parsed, Err(LiveError::InvalidConfig(_))));
    }
}
