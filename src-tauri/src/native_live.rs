use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
use std::ffi::OsStr;

use cc_switch_core::{
    builtin_app_adapter, claude_desktop, codex, gemini, AppType, HermesProviderSource,
    LiveDocumentSet, LogicalTarget, NativeAction, NativeImportContext as CoreImportContext,
    NativeImportError, NativeImportStep, NativePlanContext, NativePlanError, NativePlanRequest,
    NativeProviderAccess, NativeProviderMode, ObservedDocument, ProviderSnapshot,
};
use serde_json::{json, Map, Value};

use crate::{
    live::{LiveError, ResolvedConfigDirs},
    operation::{read_optional, LivePaths, OperationPlan},
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
    pub fn from_home(home: &Path, dirs: &ResolvedConfigDirs) -> Result<Self, LiveError> {
        let (normal, threep, profile, meta) = claude_desktop_paths(home);
        Ok(Self {
            paths: LivePaths {
                claude_settings: dirs.claude.join("settings.json"),
                claude_desktop_normal_config: normal,
                claude_desktop_threep_config: threep,
                claude_desktop_profile: profile,
                claude_desktop_meta: meta,
                codex_auth: dirs.codex.join("auth.json"),
                codex_config: dirs.codex.join("config.toml"),
                codex_model_catalog: dirs.codex.join(codex::MODEL_CATALOG_FILENAME),
                gemini_env: dirs.gemini.join(".env"),
                gemini_settings: dirs.gemini.join("settings.json"),
                grok_config: dirs.grok.join("config.toml"),
                opencode_config: dirs.opencode.join("opencode.json"),
                openclaw_config: home.join(".openclaw").join("openclaw.json"),
                hermes_config: dirs.hermes.join("config.yaml"),
                pi_models: dirs.pi.join("models.json"),
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

    fn resolved_for_targets(&self, targets: &[LogicalTarget]) -> Result<Self, LiveError> {
        let mut paths = self.paths.clone();
        for target in targets {
            paths = paths.resolved_for_write(*target)?;
        }
        Ok(Self { paths })
    }

    pub(crate) fn target_path(&self, target: LogicalTarget) -> &Path {
        self.paths.path_for(target)
    }

    pub(crate) fn observe_target(
        &self,
        target: LogicalTarget,
    ) -> Result<(LivePaths, Option<Vec<u8>>), LiveError> {
        let paths = self.paths.resolved_for_write(target)?;
        let contents = read_optional(paths.path_for(target))?;
        Ok((paths, contents))
    }

    pub fn import_drafts(&self, app: AppType) -> Result<Vec<NativeImport>, LiveError> {
        if app == AppType::ClaudeDesktop {
            ensure_claude_desktop_supported()?;
        }
        let adapter = builtin_app_adapter(&app);
        let mut paths = self.paths.clone();
        let mut observations = adapter
            .targets()
            .iter()
            .copied()
            .map(ObservedDocument::unobserved)
            .collect::<Vec<_>>();

        loop {
            let documents = LiveDocumentSet::try_new(app.clone(), observations.clone())
                .map_err(|error| LiveError::InvalidConfig(error.to_string()))?;
            match adapter
                .project_native_import(&documents)
                .map_err(core_import_error)?
            {
                NativeImportStep::Observe { target } => {
                    let observation = observations
                        .iter_mut()
                        .find(|document| document.target() == target)
                        .ok_or_else(|| {
                            LiveError::InvalidConfig(
                                "core requested an undeclared native import target".to_owned(),
                            )
                        })?;
                    if observation.is_observed() {
                        return Err(LiveError::InvalidConfig(
                            "core requested an already observed native import target".to_owned(),
                        ));
                    }
                    paths = paths.resolved_for_write(target)?;
                    *observation = read_optional(paths.path_for(target))?.map_or_else(
                        || ObservedDocument::missing(target),
                        |contents| ObservedDocument::present(target, contents),
                    );
                }
                NativeImportStep::Ready { candidates } => {
                    return candidates
                        .into_iter()
                        .map(native_import_from_core)
                        .collect();
                }
            }
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
}

fn native_import_from_core(
    candidate: cc_switch_core::NativeImportCandidate,
) -> Result<NativeImport, LiveError> {
    let ProviderSnapshot {
        id: native_id,
        app,
        name,
        settings,
    } = candidate.provider;
    let mut settings = settings.as_object().cloned().ok_or_else(|| {
        LiveError::InvalidConfig("native provider settings must be an object".to_owned())
    })?;
    let metadata = match candidate.context {
        CoreImportContext::None => json!({}),
        CoreImportContext::ClaudeDesktopDirect { routes } => {
            let routes = routes
                .into_iter()
                .map(|route| {
                    let mut value = json!({"model": route.upstream_model});
                    if let Some(label) = route.label_override {
                        value["labelOverride"] = json!(label);
                    }
                    if route.supports_1m {
                        value["supports1m"] = json!(true);
                    }
                    (route.route_id, value)
                })
                .collect::<Map<_, _>>();
            json!({
                "claudeDesktopMode": "direct",
                "claudeDesktopModelRoutes": routes
            })
        }
        CoreImportContext::Hermes { source } => {
            settings.insert(
                HERMES_SOURCE_FIELD.to_owned(),
                json!(match source {
                    HermesProviderSource::CustomProviders => HERMES_CUSTOM_SOURCE,
                    HermesProviderSource::ProvidersDictionary => HERMES_DICT_SOURCE,
                }),
            );
            json!({})
        }
    };
    Ok(NativeImport {
        native_id,
        draft: ProviderDraft {
            app_id: app.as_str().to_owned(),
            adapter: native_adapter_reference(&app),
            name,
            settings,
        },
        name_is_explicit: candidate.name_is_explicit,
        category: candidate
            .classification
            .map(|classification| match classification {
                NativeProviderMode::Official => "official".to_owned(),
                NativeProviderMode::Custom => "custom".to_owned(),
            }),
        metadata,
    })
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
    match error {
        NativePlanError::InvalidPlan(error) => LiveError::Operation(
            crate::operation::OperationError::InvalidPlan(error.to_string()),
        ),
        error @ (NativePlanError::InvalidDocument { .. }
        | NativePlanError::WrongDocumentApp { .. }) => LiveError::InvalidConfig(error.to_string()),
        error => LiveError::InvalidProvider(error.to_string()),
    }
}

fn core_import_error(error: NativeImportError) -> LiveError {
    match error {
        NativeImportError::Missing { resource } => LiveError::Missing(resource),
        error => LiveError::InvalidConfig(error.to_string()),
    }
}

fn is_official(provider: &ProviderRecord, official_id: &str) -> bool {
    provider.id == official_id || provider.category.as_deref() == Some("official")
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

#[cfg(target_os = "windows")]
pub(crate) fn default_hermes_dir(home: &Path) -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Local"))
        .join("hermes")
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn default_hermes_dir(home: &Path) -> PathBuf {
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
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.gemini_env.parent().unwrap()).unwrap();
        std::fs::write(&native.paths.gemini_env, "GEMINI_API_KEY=ok\nbroken").unwrap();
        assert!(matches!(
            native.import_drafts(AppType::Gemini),
            Err(LiveError::InvalidConfig(_))
        ));

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
        std::fs::write(&native.paths.codex_config, "model = \"gpt-5\"\n").unwrap();
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
        native
            .import_drafts(AppType::Codex)
            .expect("Codex import does not consume the model catalog");
    }

    #[test]
    fn oversized_projected_output_keeps_the_operation_plan_error_code() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        let error = native
            .apply_plan(
                &provider(
                    AppType::Claude,
                    "large",
                    json!({"values": vec![0; 200_000]}),
                ),
                None,
            )
            .expect_err("pretty output exceeds the operation plan limit");

        assert_eq!(error.code(), "invalid_operation_plan");
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

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn desktop_profile_routes_round_trip_into_internal_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.claude_desktop_profile.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.claude_desktop_profile,
            r#"{
                "inferenceGatewayBaseUrl": "https://example.com",
                "inferenceGatewayApiKey": "secret",
                "inferenceModels": [
                    "claude-sonnet-4-6",
                    {"name": "claude-opus-4-6", "labelOverride": "Opus", "supports1m": true}
                ]
            }"#,
        )
        .unwrap();

        let imported = native
            .import_drafts(AppType::ClaudeDesktop)
            .unwrap()
            .remove(0);
        let metadata = imported.metadata;
        let routes = desktop_routes(&metadata).unwrap();

        assert_eq!(
            metadata["claudeDesktopModelRoutes"]["claude-opus-4-6"]["labelOverride"],
            "Opus"
        );
        assert_eq!(routes.len(), 2);
    }

    #[test]
    fn hermes_parser_rejects_duplicate_top_level_sections() {
        let directory = tempfile::tempdir().unwrap();
        let native = NativeLiveConfig::for_tests(
            directory.path(),
            directory.path().join(".claude"),
            directory.path().join(".codex"),
        );
        std::fs::create_dir_all(native.paths.hermes_config.parent().unwrap()).unwrap();
        std::fs::write(
            &native.paths.hermes_config,
            "custom_providers:\n  - name: old\ncustom_providers:\n  - name: new\n",
        )
        .unwrap();

        assert!(matches!(
            native.import_drafts(AppType::Hermes),
            Err(LiveError::InvalidConfig(_))
        ));
    }
}
