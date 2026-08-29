use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use cc_switch_core::{
    builtin_app_adapter, claude, claude_desktop, codex, gemini, grokbuild, hermes, openclaw,
    opencode, pi, AppType, LiveDocumentSet, LogicalTarget, NativeAction, NativePlanContext,
    NativePlanError, NativePlanRequest, NativeProviderAccess, NativeProviderMode, ObservedDocument,
    ProviderSnapshot,
};
use serde_json::{json, Map, Value};

use crate::{
    live::LiveError,
    operation::{duplicate_yaml_top_level_key, read_optional, LivePaths, OperationPlan},
    provider::{
        is_lite_writable, native_adapter_reference, NativeImport, ProviderDraft, ProviderRecord,
    },
};

const CLAUDE_DESKTOP_PROFILE_ID: &str = "00000000-0000-4000-8000-000000157210";
const CLAUDE_DESKTOP_OFFICIAL_ID: &str = "claude-desktop-official";
const GEMINI_OFFICIAL_PARTNER_KEY: &str = "google-official";
const HERMES_SOURCE_FIELD: &str = "_cc_source";
const HERMES_CUSTOM_SOURCE: &str = "custom_providers";
const HERMES_DICT_SOURCE: &str = "providers_dict";

pub struct NativeLiveConfig {
    paths: LivePaths,
}

pub(crate) struct PreparedNativePlan {
    pub(crate) paths: LivePaths,
    pub(crate) plan: OperationPlan,
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
        self.resolved_for_targets(builtin_app_adapter(&app).targets())
    }

    fn resolved_for_targets(&self, targets: &[LogicalTarget]) -> Result<Self, LiveError> {
        let mut paths = self.paths.clone();
        for target in targets {
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

    #[cfg(test)]
    pub fn apply_plan(
        &self,
        provider: &ProviderRecord,
        common_snippet: Option<&str>,
    ) -> Result<OperationPlan, LiveError> {
        self.prepare_apply_plan(provider, common_snippet)
            .map(|prepared| prepared.plan)
    }

    pub(crate) fn prepare_apply_plan(
        &self,
        provider: &ProviderRecord,
        common_snippet: Option<&str>,
    ) -> Result<PreparedNativePlan, LiveError> {
        let app = provider
            .app_id
            .parse::<AppType>()
            .map_err(|_| LiveError::InvalidProvider("application is not supported".to_owned()))?;
        let access = writable_native_access(provider)?;
        if app == AppType::ClaudeDesktop {
            ensure_claude_desktop_supported()?;
        }
        let snapshot = ProviderSnapshot::new(
            &provider.id,
            app.clone(),
            &provider.name,
            Value::Object(provider.settings.clone()),
        );
        let mode = native_provider_mode(&app, provider);
        let common_config = (provider
            .metadata
            .get("commonConfigEnabled")
            .and_then(Value::as_bool)
            == Some(true))
        .then_some(common_snippet)
        .flatten();
        let routes = if app == AppType::ClaudeDesktop {
            desktop_routes(&provider.metadata)?
        } else {
            Vec::new()
        };
        let context = if app == AppType::ClaudeDesktop {
            NativePlanContext::ClaudeDesktop { routes: &routes }
        } else {
            NativePlanContext::Standard { common_config }
        };
        let adapter = builtin_app_adapter(&app);
        let targets = adapter
            .required_native_targets(NativeAction::Apply, &snapshot, mode)
            .map_err(core_plan_error)?;
        let native = self.resolved_for_targets(&targets)?;
        let documents = native.observed_documents(&app, &targets)?;
        let plan = adapter
            .plan_native(&NativePlanRequest {
                action: NativeAction::Apply,
                provider: &snapshot,
                documents: &documents,
                mode,
                access,
                context,
            })
            .map_err(core_plan_error)?;
        Ok(PreparedNativePlan {
            paths: native.paths,
            plan,
        })
    }

    #[cfg(test)]
    pub fn remove_plan(&self, provider: &ProviderRecord) -> Result<OperationPlan, LiveError> {
        self.prepare_remove_plan(provider)
            .map(|prepared| prepared.plan)
    }

    pub(crate) fn prepare_remove_plan(
        &self,
        provider: &ProviderRecord,
    ) -> Result<PreparedNativePlan, LiveError> {
        let app = provider
            .app_id
            .parse::<AppType>()
            .map_err(|_| LiveError::InvalidProvider("application is not supported".to_owned()))?;
        let access = writable_native_access(provider)?;
        let snapshot = ProviderSnapshot::new(
            &provider.id,
            app.clone(),
            &provider.name,
            Value::Object(provider.settings.clone()),
        );
        let adapter = builtin_app_adapter(&app);
        let mode = native_provider_mode(&app, provider);
        let targets = adapter
            .required_native_targets(NativeAction::Remove, &snapshot, mode)
            .map_err(core_plan_error)?;
        let native = self.resolved_for_targets(&targets)?;
        let documents = native.observed_documents(&app, &targets)?;
        let plan = adapter
            .plan_native(&NativePlanRequest {
                action: NativeAction::Remove,
                provider: &snapshot,
                documents: &documents,
                mode,
                access,
                context: NativePlanContext::Standard {
                    common_config: None,
                },
            })
            .map_err(core_plan_error)?;
        Ok(PreparedNativePlan {
            paths: native.paths,
            plan,
        })
    }

    fn observed_documents(
        &self,
        app: &AppType,
        required: &[LogicalTarget],
    ) -> Result<LiveDocumentSet, LiveError> {
        let observations = builtin_app_adapter(app)
            .targets()
            .iter()
            .copied()
            .map(|target| {
                if !required.contains(&target) {
                    return Ok(ObservedDocument::unobserved(target));
                }
                read_optional(self.paths.path_for(target)).map(|contents| match contents {
                    Some(contents) => ObservedDocument::present(target, contents),
                    None => ObservedDocument::missing(target),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        LiveDocumentSet::try_new(app.clone(), observations)
            .map_err(|error| LiveError::InvalidConfig(error.to_string()))
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

fn read_text_optional(path: &Path, label: &str) -> Result<Option<String>, LiveError> {
    read_optional(path)?
        .map(|contents| {
            String::from_utf8(contents)
                .map_err(|_| LiveError::InvalidConfig(format!("{label} config is not UTF-8")))
        })
        .transpose()
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

fn native_provider_mode(app: &AppType, provider: &ProviderRecord) -> NativeProviderMode {
    let official = match app {
        AppType::Codex => is_official(provider, "codex-official"),
        AppType::Gemini => gemini_mode(provider) == gemini::AuthMode::OAuthPersonal,
        AppType::GrokBuild => is_official(provider, "grokbuild-official"),
        AppType::ClaudeDesktop => is_official(provider, CLAUDE_DESKTOP_OFFICIAL_ID),
        _ => false,
    };
    if official {
        NativeProviderMode::Official
    } else {
        NativeProviderMode::Custom
    }
}

fn writable_native_access(provider: &ProviderRecord) -> Result<NativeProviderAccess, LiveError> {
    if is_lite_writable(provider) {
        Ok(NativeProviderAccess::Writable)
    } else {
        Err(core_plan_error(NativePlanError::ReadOnlyProvider {
            provider_id: provider.id.clone(),
        }))
    }
}

fn core_plan_error(error: NativePlanError) -> LiveError {
    if matches!(
        &error,
        NativePlanError::InvalidDocument { .. }
            | NativePlanError::WrongDocumentApp { .. }
            | NativePlanError::InvalidPlan(_)
    ) {
        LiveError::InvalidConfig(error.to_string())
    } else {
        LiveError::InvalidProvider(error.to_string())
    }
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
    fn unmanaged_codex_auth_and_catalog_are_not_observed_or_rewritten() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.codex_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.codex_auth,
            vec![b'x'; cc_switch_core::MAX_OPERATION_CONTENT_BYTES + 1],
        )
        .unwrap();
        std::fs::write(
            &native.paths.codex_model_catalog,
            vec![b'x'; cc_switch_core::MAX_OPERATION_CONTENT_BYTES + 1],
        )
        .unwrap();
        let plan = native
            .apply_plan(
                &provider(
                    AppType::Codex,
                    "custom",
                    json!({"auth": {}, "config": "model = \"gpt-5\"\n"}),
                ),
                None,
            )
            .expect("unmanaged catalog does not block Codex");

        assert!(plan.writes.iter().all(|write| !matches!(
            write.target,
            LogicalTarget::CodexAuth | LogicalTarget::CodexModelCatalog
        )));
    }

    #[cfg(unix)]
    #[test]
    fn unmanaged_codex_catalog_symlink_does_not_block_path_preflight() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.codex_config.parent().unwrap()).unwrap();
        let unrelated = directory.path().join("unrelated.json");
        std::fs::write(&unrelated, "{}").unwrap();
        symlink(&unrelated, &native.paths.codex_model_catalog).unwrap();

        native
            .apply_plan(
                &provider(
                    AppType::Codex,
                    "custom",
                    json!({"auth": {}, "config": "model = \"gpt-5\"\n"}),
                ),
                None,
            )
            .expect("unmanaged symlink is outside the operation target set");
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
    fn additive_removal_ignores_unused_oversized_provider_settings() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.pi_models.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.pi_models,
            r#"{"providers":{"remove":{"models":[]}}}"#,
        )
        .unwrap();

        let plan = native
            .remove_plan(&provider(
                AppType::Pi,
                "remove",
                json!({
                    "unused": "x".repeat(cc_switch_core::MAX_OPERATION_CONTENT_BYTES + 1)
                }),
            ))
            .expect("removal only consumes the native provider id");
        let parsed: Value =
            serde_json::from_str(plan.writes[0].contents.as_deref().unwrap()).expect("Pi JSON");
        assert!(parsed["providers"].get("remove").is_none());
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

        let mut desktop_proxy = provider(
            AppType::ClaudeDesktop,
            "proxy",
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://example.com",
                    "ANTHROPIC_AUTH_TOKEN": "secret"
                }
            }),
        );
        desktop_proxy.metadata = json!({
            "claudeDesktopMode": "proxy",
            "claudeDesktopModelRoutes": {"broken": {}}
        });
        assert!(matches!(
            native.apply_plan(&desktop_proxy, None),
            Err(LiveError::InvalidProvider(message)) if message.contains("read-only")
        ));
    }

    #[test]
    fn claude_desktop_apply_respects_platform_support() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        let result = native.apply_plan(
            &provider(
                AppType::ClaudeDesktop,
                "desktop-direct",
                json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": "https://example.com",
                        "ANTHROPIC_AUTH_TOKEN": "secret"
                    }
                }),
            ),
            None,
        );

        if cfg!(any(target_os = "macos", target_os = "windows")) {
            assert!(result.is_ok());
        } else {
            assert!(matches!(
                result,
                Err(LiveError::InvalidProvider(message))
                    if message.contains("supported on macOS and Windows")
            ));
        }
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
    fn hermes_parser_rejects_duplicate_top_level_sections() {
        let parsed = parse_yaml(
            "custom_providers:\n  - name: old\ncustom_providers:\n  - name: new\n",
            "Hermes",
        );

        assert!(matches!(parsed, Err(LiveError::InvalidConfig(_))));
    }
}
