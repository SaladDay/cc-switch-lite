use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::Mutex,
};

use fs4::{FileExt, TryLockError};
use thiserror::Error;

use crate::{
    native_live::NativeLiveConfig,
    operation::{LivePaths, OperationError, OperationExecutor, OperationPlan, OperationReceipt},
    provider::{NativeImport, ProviderRecord},
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
            Self::InvalidProvider(_) => "invalid_provider",
            Self::LockUnavailable => "lock_unavailable",
            Self::Io { .. } => "live_io_error",
        }
    }
}

pub struct LiveConfig {
    native: NativeLiveConfig,
    lock_path: PathBuf,
    gate: Mutex<()>,
}

pub struct LiveWriteReceipt {
    operation: OperationReceipt,
}

impl LiveConfig {
    pub fn from_home(home: &Path, lock_path: PathBuf) -> Result<Self, LiveError> {
        let claude_dir = config_root(
            std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
            &home.join(".claude"),
            "CLAUDE_CONFIG_DIR",
        )?;
        let codex_dir = config_root(
            std::env::var_os("CODEX_HOME").as_deref(),
            &home.join(".codex"),
            "CODEX_HOME",
        )?;
        Ok(Self {
            native: NativeLiveConfig::from_home(home, claude_dir, codex_dir)?,
            lock_path,
            gate: Mutex::new(()),
        })
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
            self.execute_recoverable_plan(&prepared.paths, &prepared.plan)
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
            self.execute_recoverable_plan(&prepared.paths, &prepared.plan)
        })
    }

    pub fn rollback(&self, receipt: LiveWriteReceipt) -> Result<(), LiveError> {
        self.with_lock(|| receipt.operation.rollback().map_err(LiveError::from))
    }

    fn execute_recoverable_plan(
        &self,
        paths: &LivePaths,
        plan: &OperationPlan,
    ) -> Result<LiveWriteReceipt, LiveError> {
        OperationExecutor::new(paths)
            .execute_recoverable(plan)
            .map(|operation| LiveWriteReceipt { operation })
            .map_err(Into::into)
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
