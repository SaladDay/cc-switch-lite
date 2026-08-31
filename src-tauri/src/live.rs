use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use cc_switch_core::{
    fs::shared_live_config_lock_path, AppType, SkillDeploymentReceipt, SkillSyncMethod,
    MAX_OPERATION_CONTENT_BYTES,
};
use fs4::{FileExt, TryLockError};
use serde::Deserialize;
use thiserror::Error;

use crate::{
    mcp::{McpImportsByApp, McpLiveChange},
    mcp_live::{McpImportSnapshot, McpLiveConfig, McpLiveReceipt},
    native_live::NativeLiveConfig,
    operation::{LivePaths, OperationError, OperationExecutor, OperationPlan, OperationReceipt},
    provider::{NativeImport, ProviderRecord},
    skill::RecoverableSkillChange,
    skill_live::{SkillLiveConfig, SkillLiveError, SkillObservation},
};

#[derive(Debug, Error)]
pub enum LiveError {
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error("live configuration is missing for {0}")]
    Missing(String),
    #[error("live configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("provider cannot be switched: {0}")]
    InvalidProvider(String),
    #[error(transparent)]
    Skill(#[from] SkillLiveError),
    #[error("live configuration lock is unavailable")]
    LockUnavailable,
    #[error("live configuration I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("live recovery was incomplete: {0}")]
    Recovery(String),
}

impl LiveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Operation(error) => error.code(),
            Self::Missing(_) => "live_missing",
            Self::InvalidConfig(_) => "invalid_live_config",
            Self::InvalidProvider(_) => "invalid_provider",
            Self::Skill(_) => "skill_live_error",
            Self::LockUnavailable => "lock_unavailable",
            Self::Io { .. } => "live_io_error",
            Self::Recovery(_) => "rollback_failed",
        }
    }
}

pub struct LiveConfig {
    native: NativeLiveConfig,
    mcp: McpLiveConfig,
    skill: SkillLiveConfig,
    lock_path: PathBuf,
    gate: Mutex<()>,
}

pub(crate) struct LockedLiveReceipt<'a, T> {
    value: T,
    gate: MutexGuard<'a, ()>,
    file_lock: File,
}

pub(crate) type LiveWriteReceipt<'a> = LockedLiveReceipt<'a, OperationReceipt>;
pub(crate) type McpWriteReceipt<'a> = LockedLiveReceipt<'a, McpLiveReceipt>;
pub(crate) type McpImportObservation<'a> = LockedLiveReceipt<'a, McpImportSnapshot>;
pub(crate) type SkillWriteReceipt<'a> = LockedLiveReceipt<'a, SkillDeploymentReceipt>;

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SharedPathSettings {
    claude_config_dir: Option<String>,
    codex_config_dir: Option<String>,
    gemini_config_dir: Option<String>,
    grok_config_dir: Option<String>,
    opencode_config_dir: Option<String>,
    hermes_config_dir: Option<String>,
    skill_sync_method: Option<String>,
    skill_storage_location: Option<String>,
}

pub(crate) struct ResolvedConfigDirs {
    pub(crate) claude: PathBuf,
    pub(crate) codex: PathBuf,
    pub(crate) gemini: PathBuf,
    pub(crate) grok: PathBuf,
    pub(crate) opencode: PathBuf,
    pub(crate) hermes: PathBuf,
    pub(crate) pi: PathBuf,
}

impl LiveConfig {
    pub fn from_home(home: &Path) -> Result<Self, LiveError> {
        let settings = load_shared_path_settings(home);
        let dirs = resolve_config_dirs(home, &settings)?;
        let claude_mcp = claude_mcp_path(home, &dirs.claude)?;
        let unified_store = settings.skill_storage_location.as_deref() == Some("unified");
        let skill_roots = vec![
            (AppType::Claude, dirs.claude.clone()),
            (AppType::Codex, dirs.codex.clone()),
            (AppType::Gemini, dirs.gemini.clone()),
            (AppType::GrokBuild, dirs.grok.clone()),
            (AppType::OpenCode, dirs.opencode.clone()),
            (AppType::Hermes, dirs.hermes.clone()),
            (AppType::Pi, dirs.pi.clone()),
        ]
        .into_iter()
        .map(|(app, root)| absolute_skill_root(&root).map(|root| (app, root)))
        .collect::<Result<Vec<_>, _>>()?;
        let skill = SkillLiveConfig::new(
            skill_source_root(home, settings.skill_storage_location.as_deref()),
            skill_sync_method(settings.skill_sync_method.as_deref()),
            unified_store,
            skill_roots,
        )?;
        Ok(Self {
            native: NativeLiveConfig::from_home(home, &dirs)?,
            mcp: McpLiveConfig::new(
                (claude_mcp, dirs.claude),
                (dirs.codex.join("config.toml"), dirs.codex),
                (dirs.gemini.join("settings.json"), dirs.gemini),
                (dirs.grok.join("config.toml"), dirs.grok),
                (dirs.opencode.join("opencode.json"), dirs.opencode),
                (dirs.hermes.join("config.yaml"), dirs.hermes),
            ),
            skill,
            lock_path: shared_live_config_lock_path(home),
            gate: Mutex::new(()),
        })
    }

    pub fn import_native_drafts(&self, app_id: &str) -> Result<Vec<NativeImport>, LiveError> {
        let app = app_id.parse::<cc_switch_core::AppType>().map_err(|_| {
            LiveError::InvalidProvider("application is not available in Lite".to_owned())
        })?;
        self.with_lock(|| self.native.import_drafts(app))
    }

    pub fn apply_mcp_recoverable(
        &self,
        changes: &mut [McpLiveChange],
    ) -> Result<McpWriteReceipt<'_>, LiveError> {
        self.lock_result(|| self.mcp.apply(changes))
    }

    pub(crate) fn apply_skill_recoverable(
        &self,
        directory: &str,
        app: &AppType,
        enabled: bool,
    ) -> Result<SkillWriteReceipt<'_>, LiveError> {
        self.lock_result(|| {
            self.skill
                .apply(directory, app, enabled)
                .map_err(Into::into)
        })
    }

    pub(crate) fn observe_skills(
        &self,
        skills: &[(String, String)],
    ) -> Result<Vec<SkillObservation>, LiveError> {
        self.with_lock(|| Ok(self.skill.observe(skills)))
    }

    pub fn rollback_mcp(&self, receipt: McpWriteReceipt<'_>) -> Result<(), LiveError> {
        let LockedLiveReceipt {
            value,
            gate,
            file_lock,
        } = receipt;
        let result = self.mcp.rollback(value);
        drop(file_lock);
        drop(gate);
        result
    }

    pub fn observe_mcp(&self) -> Result<(McpImportsByApp, McpImportObservation<'_>), LiveError> {
        let observation = self.lock_result(|| Ok(self.mcp.import_snapshot()))?;
        let imports = observation.value.imports.clone();
        Ok((imports, observation))
    }

    pub fn mcp_observation_is_current(&self, observation: &McpImportObservation<'_>) -> bool {
        self.mcp.snapshot_is_current(&observation.value)
    }

    pub fn switch_native_recoverable(
        &self,
        provider: &ProviderRecord,
        common_snippet: Option<&str>,
    ) -> Result<LiveWriteReceipt<'_>, LiveError> {
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
        self.lock_result(|| {
            let prepared = self.native.prepare_apply_plan(provider, common_snippet)?;
            self.execute_recoverable_plan(&prepared.paths, &prepared.plan)
        })
    }

    pub fn remove_native_recoverable(
        &self,
        provider: &ProviderRecord,
    ) -> Result<LiveWriteReceipt<'_>, LiveError> {
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
        self.lock_result(|| {
            let prepared = self.native.prepare_remove_plan(provider)?;
            self.execute_recoverable_plan(&prepared.paths, &prepared.plan)
        })
    }

    pub fn rollback(&self, receipt: LiveWriteReceipt<'_>) -> Result<(), LiveError> {
        let LockedLiveReceipt {
            value,
            gate,
            file_lock,
        } = receipt;
        let result = value.rollback().map_err(LiveError::from);
        drop(file_lock);
        drop(gate);
        result
    }

    fn execute_recoverable_plan(
        &self,
        paths: &LivePaths,
        plan: &OperationPlan,
    ) -> Result<OperationReceipt, LiveError> {
        OperationExecutor::new(paths)
            .execute_recoverable(plan)
            .map_err(Into::into)
    }

    fn with_lock<T>(&self, action: impl FnOnce() -> Result<T, LiveError>) -> Result<T, LiveError> {
        let (_guard, _file_lock) = self.acquire_lock()?;
        action()
    }

    fn lock_result<T>(
        &self,
        action: impl FnOnce() -> Result<T, LiveError>,
    ) -> Result<LockedLiveReceipt<'_, T>, LiveError> {
        let (gate, file_lock) = self.acquire_lock()?;
        let value = action()?;
        Ok(LockedLiveReceipt {
            value,
            gate,
            file_lock,
        })
    }

    fn acquire_lock(&self) -> Result<(MutexGuard<'_, ()>, File), LiveError> {
        let guard = self
            .gate
            .try_lock()
            .map_err(|_| LiveError::LockUnavailable)?;
        let file_lock = self.lock_file()?;
        Ok((guard, file_lock))
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

impl RecoverableSkillChange for SkillWriteReceipt<'_> {
    type Error = LiveError;

    fn verify(&self) -> Result<(), Self::Error> {
        self.value
            .verify()
            .map_err(|error| LiveError::Skill(error.into()))
    }

    fn commit(self) -> Result<(), Self::Error> {
        let LockedLiveReceipt {
            value,
            gate,
            file_lock,
        } = self;
        let result = value
            .commit()
            .map_err(|error| LiveError::Skill(error.into()));
        drop(file_lock);
        drop(gate);
        result
    }

    fn rollback(self) -> Result<(), Self::Error> {
        let LockedLiveReceipt {
            value,
            gate,
            file_lock,
        } = self;
        let result = value
            .rollback()
            .map_err(|error| LiveError::Skill(error.into()));
        drop(file_lock);
        drop(gate);
        result
    }
}

fn resolve_config_dirs(
    home: &Path,
    settings: &SharedPathSettings,
) -> Result<ResolvedConfigDirs, LiveError> {
    let claude_env = std::env::var_os("CLAUDE_CONFIG_DIR");
    let codex_env = std::env::var_os("CODEX_HOME");
    let hermes_env = std::env::var_os("HERMES_HOME");
    let pi_env = std::env::var_os("PI_CODING_AGENT_DIR");
    Ok(ResolvedConfigDirs {
        claude: configured_root(
            home,
            settings.claude_config_dir.as_deref(),
            claude_env.as_deref(),
            &home.join(".claude"),
            "Claude config directory",
        )?,
        codex: configured_root(
            home,
            settings.codex_config_dir.as_deref(),
            codex_env.as_deref(),
            &home.join(".codex"),
            "Codex config directory",
        )?,
        gemini: configured_root(
            home,
            settings.gemini_config_dir.as_deref(),
            None,
            &home.join(".gemini"),
            "Gemini config directory",
        )?,
        grok: configured_root(
            home,
            settings.grok_config_dir.as_deref(),
            None,
            &home.join(".grok"),
            "Grok config directory",
        )?,
        opencode: configured_root(
            home,
            settings.opencode_config_dir.as_deref(),
            None,
            &home.join(".config/opencode"),
            "OpenCode config directory",
        )?,
        hermes: hermes_root(
            home,
            settings.hermes_config_dir.as_deref(),
            hermes_env.as_deref(),
            &crate::native_live::default_hermes_dir(home),
        ),
        pi: configured_root(
            home,
            None,
            pi_env.as_deref(),
            &home.join(".pi/agent"),
            "Pi config directory",
        )?,
    })
}

fn skill_source_root(home: &Path, location: Option<&str>) -> PathBuf {
    match location {
        Some("unified") => home.join(".agents/skills"),
        _ => shared_database_dir(home).join("skills"),
    }
}

fn skill_sync_method(method: Option<&str>) -> SkillSyncMethod {
    match method {
        Some("symlink") => SkillSyncMethod::Symlink,
        Some("copy") => SkillSyncMethod::Copy,
        _ => SkillSyncMethod::Auto,
    }
}

fn load_shared_path_settings(home: &Path) -> SharedPathSettings {
    let path = device_settings_path(home);
    let Ok(contents) = fs::read(path) else {
        return SharedPathSettings::default();
    };
    if contents.len() > MAX_OPERATION_CONTENT_BYTES {
        return SharedPathSettings::default();
    }
    serde_json::from_slice(&contents).unwrap_or_default()
}

fn device_settings_path(home: &Path) -> PathBuf {
    home.join(".cc-switch/settings.json")
}

fn shared_database_dir(home: &Path) -> PathBuf {
    crate::store::database_path(home)
        .parent()
        .map(Path::to_owned)
        .unwrap_or_else(|| home.join(".cc-switch"))
}

fn absolute_skill_root(path: &Path) -> Result<PathBuf, LiveError> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    std::env::current_dir()
        .map(|current| current.join(path))
        .map_err(|source| LiveError::Io {
            path: PathBuf::from("."),
            source,
        })
}

fn configured_root(
    home: &Path,
    setting: Option<&str>,
    environment: Option<&OsStr>,
    default: &Path,
    label: &str,
) -> Result<PathBuf, LiveError> {
    let setting = setting
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| resolve_shared_path(home, value));
    let environment = environment
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let configured = setting.or(environment);
    config_root(configured.as_deref().map(Path::as_os_str), default, label)
}

fn hermes_root(
    home: &Path,
    setting: Option<&str>,
    environment: Option<&OsStr>,
    default: &Path,
) -> PathBuf {
    if let Some(setting) = setting {
        return resolve_shared_path(home, setting);
    }
    environment
        .map(|value| value.to_string_lossy())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default.to_owned())
}

fn resolve_shared_path(home: &Path, raw: &str) -> PathBuf {
    if raw == "~" {
        home.to_owned()
    } else if let Some(path) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        home.join(path)
    } else {
        PathBuf::from(raw)
    }
}

fn claude_mcp_path(home: &Path, config_dir: &Path) -> Result<PathBuf, LiveError> {
    let default_dir = config_root(None, &home.join(".claude"), "Claude config directory")?;
    if path_eq_lexical(config_dir, &default_dir) {
        return Ok(home.join(".claude.json"));
    }
    #[cfg(windows)]
    if let Some(path) = derive_wsl_default_mcp_path(config_dir) {
        return Ok(path);
    }
    Ok(config_dir.join(".claude.json"))
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn path_eq_lexical(left: &Path, right: &Path) -> bool {
    comparable_path_key(left) == comparable_path_key(right)
}

fn comparable_path_key(path: &Path) -> String {
    let mut key = normalize_path_lexically(path).to_string_lossy().to_string();
    #[cfg(windows)]
    {
        key = key.replace('\\', "/");
    }
    while key.len() > 1 && key.ends_with('/') {
        key.pop();
    }
    #[cfg(windows)]
    key.make_ascii_lowercase();
    key
}

#[cfg(windows)]
fn derive_wsl_default_mcp_path(dir: &Path) -> Option<PathBuf> {
    use std::path::Prefix;

    let normalized = normalize_path_lexically(dir);
    let mut components = normalized.components();
    let prefix = match components.next()? {
        Component::Prefix(prefix) => prefix,
        _ => return None,
    };
    let server = match prefix.kind() {
        Prefix::UNC(server, _) | Prefix::VerbatimUNC(server, _) => server.to_string_lossy(),
        _ => return None,
    };
    if !server.eq_ignore_ascii_case("wsl$") && !server.eq_ignore_ascii_case("wsl.localhost") {
        return None;
    }
    let mut parts = Vec::new();
    for component in components {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    let home_default =
        parts.len() == 3 && parts[0] == "home" && !parts[1].is_empty() && parts[2] == ".claude";
    let root_default = parts.len() == 2 && parts[0] == "root" && parts[1] == ".claude";
    (home_default || root_default)
        .then(|| {
            normalized
                .parent()
                .map(|parent| parent.join(".claude.json"))
        })
        .flatten()
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_roots_require_absolute_paths_and_allow_missing_directories() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let default = directory.path().join("default");

        assert!(matches!(
            config_root(Some(OsStr::new("relative")), &default, "CODEX_HOME"),
            Err(LiveError::InvalidConfig(_))
        ));
        assert_eq!(
            config_root(None, &default, "CODEX_HOME").unwrap(),
            fs::canonicalize(directory.path()).unwrap().join("default")
        );
    }

    #[test]
    fn claude_default_directory_always_uses_the_home_mcp_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let default = directory.path().join(".claude");
        for configured in [
            configured_root(
                directory.path(),
                None,
                Some(OsStr::new("")),
                &default,
                "Claude config directory",
            )
            .unwrap(),
            configured_root(
                directory.path(),
                default.to_str(),
                None,
                &default,
                "Claude config directory",
            )
            .unwrap(),
        ] {
            assert_eq!(
                claude_mcp_path(directory.path(), &configured).unwrap(),
                directory.path().join(".claude.json")
            );
        }
    }

    #[test]
    fn reads_only_shared_directory_settings_and_expands_home() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::create_dir(directory.path().join(".cc-switch")).unwrap();
        fs::write(
            directory.path().join(".cc-switch/settings.json"),
            r#"{"claudeConfigDir":"~/profiles/claude","webdavSync":{"password":"ignored"}}"#,
        )
        .unwrap();

        let settings = load_shared_path_settings(directory.path());
        let resolved = configured_root(
            directory.path(),
            settings.claude_config_dir.as_deref(),
            None,
            &directory.path().join(".claude"),
            "Claude config directory",
        )
        .unwrap();

        assert_eq!(
            resolved,
            fs::canonicalize(directory.path())
                .unwrap()
                .join("profiles/claude")
        );
    }

    #[test]
    fn hermes_home_matches_the_full_application_resolution() {
        let home = Path::new("/home/tester");
        let default = home.join(".hermes");

        assert_eq!(
            hermes_root(
                home,
                None,
                Some(OsStr::new("  relative/hermes  ")),
                &default
            ),
            PathBuf::from("relative/hermes")
        );
        assert_eq!(
            hermes_root(home, None, Some(OsStr::new("   ")), &default),
            default
        );
        assert_eq!(
            hermes_root(
                home,
                Some("~/custom-hermes"),
                Some(OsStr::new("ignored")),
                &home.join(".hermes"),
            ),
            home.join("custom-hermes")
        );
        assert_eq!(
            absolute_skill_root(Path::new("relative/hermes")).unwrap(),
            std::env::current_dir().unwrap().join("relative/hermes")
        );
    }

    #[test]
    fn device_settings_stay_under_the_system_home() {
        let home = Path::new("/system/home");
        assert_eq!(
            device_settings_path(home),
            home.join(".cc-switch/settings.json")
        );
    }

    #[test]
    fn mcp_receipt_holds_the_shared_lock_until_the_database_outcome() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::create_dir(directory.path().join(".claude")).unwrap();
        fs::write(directory.path().join(".claude.json"), "{}").unwrap();
        let live = LiveConfig::from_home(directory.path()).unwrap();
        let mut changes = [McpLiveChange::Upsert {
            app: cc_switch_core::AppType::Claude,
            id: "server".to_owned(),
            previous: None,
            server: json!({"command":"npx"}),
            native_snapshot: None,
            link_state: crate::mcp::McpNativeLinkState::Unowned,
        }];
        let receipt = live.apply_mcp_recoverable(&mut changes).unwrap();
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(shared_live_config_lock_path(directory.path()))
            .unwrap();

        assert!(matches!(
            FileExt::try_lock(&contender),
            Err(TryLockError::WouldBlock)
        ));
        drop(receipt);
        FileExt::try_lock(&contender).unwrap();
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
}
