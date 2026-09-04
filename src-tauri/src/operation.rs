use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use cc_switch_core::{
    execute_operation_plan,
    fs::{atomic_write, FileError},
    CompareExchangeOutcome, ConfigFormat, OperationExecutionError, OperationFailure, OperationHost,
    OperationRead, OperationReceipt as CoreOperationReceipt, OperationRollbackError,
    MAX_OPERATION_CONTENT_BYTES,
};
#[cfg(test)]
use cc_switch_core::{ContentExpectation, PlannedWrite, OPERATION_CONTRACT_MAJOR};
pub use cc_switch_core::{LogicalTarget, OperationPlan};
use thiserror::Error;
use toml_edit::DocumentMut;

const MAX_CONTENT_BYTES: usize = MAX_OPERATION_CONTENT_BYTES;

#[derive(Debug, Clone)]
pub struct LivePaths {
    paths: Vec<(LogicalTarget, PathBuf)>,
}

impl LivePaths {
    pub(crate) fn try_new(
        paths: impl IntoIterator<Item = (LogicalTarget, PathBuf)>,
    ) -> Result<Self, OperationError> {
        let paths = paths.into_iter().collect::<Vec<_>>();
        let targets = paths
            .iter()
            .map(|(target, _)| *target)
            .collect::<HashSet<_>>();
        if paths.len() != LogicalTarget::ALL.len()
            || targets.len() != LogicalTarget::ALL.len()
            || !LogicalTarget::ALL
                .iter()
                .all(|target| targets.contains(target))
        {
            return Err(OperationError::InvalidTarget(
                "live paths must cover every logical target exactly once".to_owned(),
            ));
        }
        Ok(Self { paths })
    }

    pub(crate) fn path_for(&self, target: LogicalTarget) -> &Path {
        self.paths
            .iter()
            .find_map(|(candidate, path)| (*candidate == target).then_some(path.as_path()))
            .expect("LivePaths construction validates complete target coverage")
    }

    pub fn resolved_for_write(&self, target: LogicalTarget) -> Result<Self, OperationError> {
        let mut resolved = self.clone();
        let path = resolve_write_path(self.path_for(target))?;
        resolved.replace(target, path);
        Ok(resolved)
    }

    pub(crate) fn replace(&mut self, target: LogicalTarget, path: PathBuf) {
        let (_, current) = self
            .paths
            .iter_mut()
            .find(|(candidate, _)| *candidate == target)
            .expect("LivePaths construction validates complete target coverage");
        *current = path;
    }
}

#[derive(Debug, Error)]
pub enum OperationError {
    #[error("operation plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("live configuration changed while the switch was being prepared; try again")]
    Conflict,
    #[error("live configuration target is invalid: {0}")]
    InvalidTarget(String),
    #[error("live configuration exceeds the {limit} byte limit")]
    TooLarge { limit: usize },
    #[error(transparent)]
    File(#[from] FileError),
    #[error("live configuration I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("switch failed and rollback was incomplete: {0}")]
    Rollback(String),
}

impl OperationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPlan(_) => "invalid_operation_plan",
            Self::Conflict => "live_conflict",
            Self::InvalidTarget(_) => "invalid_live_target",
            Self::TooLarge { .. } => "live_too_large",
            Self::File(_) | Self::Io { .. } => "live_io_error",
            Self::Rollback(_) => "rollback_failed",
        }
    }
}

pub struct OperationExecutor<'a> {
    paths: &'a LivePaths,
}

pub(crate) struct OperationReceipt {
    inner: CoreOperationReceipt<PathBuf>,
    paths: LivePaths,
}

impl OperationReceipt {
    pub(crate) fn rollback(self) -> Result<(), OperationError> {
        let Self { inner, paths } = self;
        let mut host = FileOperationHost::new(&paths);
        inner.rollback(&mut host).map_err(map_rollback_error)
    }
}

pub(crate) struct FileOperationHost<'a> {
    paths: &'a LivePaths,
}

impl<'a> FileOperationHost<'a> {
    pub(crate) fn new(paths: &'a LivePaths) -> Self {
        Self { paths }
    }
}

impl OperationHost for FileOperationHost<'_> {
    type Resource = PathBuf;
    type Error = OperationError;

    fn resolve(&mut self, target: LogicalTarget) -> Result<Self::Resource, Self::Error> {
        resolve_write_path(self.paths.path_for(target))
    }

    fn read(
        &mut self,
        resource: &Self::Resource,
        maximum: usize,
    ) -> Result<OperationRead, Self::Error> {
        match read_optional_no_follow(resource, maximum) {
            Ok(Some(contents)) => Ok(OperationRead::Contents(contents)),
            Ok(None) => Ok(OperationRead::Missing),
            Err(OperationError::TooLarge { .. }) => Ok(OperationRead::TooLarge),
            Err(error) => Err(error),
        }
    }

    fn compare_exchange(
        &mut self,
        resource: &Self::Resource,
        expected: Option<&[u8]>,
        replacement: Option<&[u8]>,
    ) -> Result<CompareExchangeOutcome, Self::Error> {
        if !resource_matches(resource, expected)? {
            return Ok(CompareExchangeOutcome::Conflict);
        }

        match replacement {
            Some(contents) => atomic_write(resource, contents)?,
            None => match fs::remove_file(resource) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(CompareExchangeOutcome::Conflict);
                }
                Err(source) => {
                    return Err(OperationError::Io {
                        path: resource.clone(),
                        source,
                    });
                }
            },
        }
        Ok(CompareExchangeOutcome::Applied)
    }
}

impl<'a> OperationExecutor<'a> {
    pub fn new(paths: &'a LivePaths) -> Self {
        Self { paths }
    }

    #[cfg(test)]
    pub fn execute(&self, plan: &OperationPlan) -> Result<(), OperationError> {
        self.execute_recoverable(plan).map(drop)
    }

    pub(crate) fn execute_recoverable(
        &self,
        plan: &OperationPlan,
    ) -> Result<OperationReceipt, OperationError> {
        self.validate(plan)?;
        let mut host = FileOperationHost::new(self.paths);
        let inner = execute_operation_plan(plan, &mut host).map_err(map_execution_error)?;
        Ok(OperationReceipt {
            inner,
            paths: self.paths.clone(),
        })
    }

    fn validate(&self, plan: &OperationPlan) -> Result<(), OperationError> {
        plan.validate()
            .map_err(|error| OperationError::InvalidPlan(error.to_string()))?;
        for write in &plan.writes {
            validate_contents(write.target, write.contents.as_deref())?;
        }
        Ok(())
    }
}

fn resource_matches(path: &Path, expected: Option<&[u8]>) -> Result<bool, OperationError> {
    let maximum = expected.map_or(0, <[u8]>::len);
    match read_optional_no_follow(path, maximum) {
        Ok(contents) => Ok(contents.as_deref() == expected),
        Err(OperationError::TooLarge { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

fn map_execution_error(error: OperationExecutionError<OperationError>) -> OperationError {
    let (failure, rollback_failures) = error.into_parts();
    if !rollback_failures.is_empty() {
        let rollback = rollback_failures
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        return OperationError::Rollback(format!(
            "operation error: {failure}; rollback error: {rollback}"
        ));
    }

    match failure {
        OperationFailure::InvalidPlan(error) => OperationError::InvalidPlan(error.to_string()),
        OperationFailure::Resolve { source, .. }
        | OperationFailure::Read { source, .. }
        | OperationFailure::Write { source, .. } => source,
        OperationFailure::AliasedTargets { .. } => {
            OperationError::InvalidPlan("logical targets resolve to the same file".to_owned())
        }
        OperationFailure::ObservedContentTooLarge { limit, .. } => {
            OperationError::TooLarge { limit }
        }
        OperationFailure::Conflict { .. } => OperationError::Conflict,
        other => OperationError::InvalidPlan(other.to_string()),
    }
}

fn map_rollback_error(error: OperationRollbackError<OperationError>) -> OperationError {
    OperationError::Rollback(
        error
            .into_failures()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
    )
}

pub fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, OperationError> {
    read_optional_no_follow(path, MAX_CONTENT_BYTES)
}

pub fn read_optional_no_follow(
    path: &Path,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, OperationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(OperationError::InvalidTarget(format!(
                "{} must not be a symbolic link",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(OperationError::InvalidTarget(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(OperationError::Io {
                path: path.to_owned(),
                source,
            });
        }
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|source| OperationError::Io {
        path: path.to_owned(),
        source,
    })?;
    read_opened(path, file, max_bytes)
}

fn read_opened(
    path: &Path,
    file: File,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, OperationError> {
    let metadata = file.metadata().map_err(|source| OperationError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(OperationError::InvalidTarget(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(OperationError::TooLarge { limit: max_bytes });
    }

    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut contents)
        .map_err(|source| OperationError::Io {
            path: path.to_owned(),
            source,
        })?;
    if contents.len() > max_bytes {
        return Err(OperationError::TooLarge { limit: max_bytes });
    }
    Ok(Some(contents))
}

pub(crate) fn resolve_write_path(path: &Path) -> Result<PathBuf, OperationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(OperationError::InvalidTarget(
            format!("{} must not be a symbolic link", path.display()),
        )),
        Ok(metadata) if metadata.is_file() => {
            fs::canonicalize(path).map_err(|source| OperationError::Io {
                path: path.to_owned(),
                source,
            })
        }
        Ok(_) => Err(OperationError::InvalidTarget(format!(
            "{} is not a regular file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            canonicalize_missing_path(path)
        }
        Err(source) => Err(OperationError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn canonicalize_missing_path(path: &Path) -> Result<PathBuf, OperationError> {
    let mut ancestor = path.to_owned();
    let mut missing = Vec::new();
    loop {
        match fs::metadata(&ancestor) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(OperationError::InvalidTarget(format!(
                        "{} is not below a directory",
                        path.display()
                    )));
                }
                let mut resolved =
                    fs::canonicalize(&ancestor).map_err(|source| OperationError::Io {
                        path: ancestor.clone(),
                        source,
                    })?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    OperationError::InvalidTarget(format!(
                        "{} has no existing parent directory",
                        path.display()
                    ))
                })?;
                missing.push(name.to_owned());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| {
                        OperationError::InvalidTarget(format!(
                            "{} has no existing parent directory",
                            path.display()
                        ))
                    })?
                    .to_owned();
            }
            Err(source) => {
                return Err(OperationError::Io {
                    path: ancestor,
                    source,
                });
            }
        }
    }
}

fn validate_contents(target: LogicalTarget, contents: Option<&str>) -> Result<(), OperationError> {
    let Some(contents) = contents else {
        return Ok(());
    };
    match target.format() {
        ConfigFormat::Json => {
            let value: serde_json::Value = serde_json::from_str(contents).map_err(|error| {
                OperationError::InvalidPlan(format!("JSON write is invalid: {error}"))
            })?;
            if !value.is_object() {
                return Err(OperationError::InvalidPlan(
                    "JSON write must contain an object".to_owned(),
                ));
            }
        }
        ConfigFormat::Toml => {
            contents
                .parse::<DocumentMut>()
                .map_err(|_| OperationError::InvalidPlan("TOML write is invalid".to_owned()))?;
        }
        ConfigFormat::Json5 => {
            let value: serde_json::Value = json5::from_str(contents)
                .map_err(|_| OperationError::InvalidPlan("JSON5 write is invalid".to_owned()))?;
            if !value.is_object() {
                return Err(OperationError::InvalidPlan(
                    "JSON5 write must contain an object".to_owned(),
                ));
            }
        }
        ConfigFormat::Env => validate_env_contents(contents)?,
        ConfigFormat::Yaml => {
            if duplicate_yaml_top_level_key(contents).is_some() {
                return Err(OperationError::InvalidPlan(
                    "YAML write contains a duplicate top-level key".to_owned(),
                ));
            }
            serde_yaml::from_str::<serde_yaml::Value>(contents)
                .map_err(|_| OperationError::InvalidPlan("YAML write is invalid".to_owned()))?;
        }
    }
    Ok(())
}

pub(crate) fn duplicate_yaml_top_level_key(raw: &str) -> Option<String> {
    let mut seen = HashSet::new();
    for line in raw.split('\n') {
        if yaml_top_level_key_line(line) {
            if let Some(colon) = line.find(':') {
                let key = line[..colon].trim();
                if !seen.insert(key) {
                    return Some(key.to_owned());
                }
            }
        }
    }
    None
}

fn yaml_top_level_key_line(line: &str) -> bool {
    if line.is_empty() || line.starts_with([' ', '\t', '#', '-']) {
        return false;
    }
    line.find(':').is_some_and(|colon| {
        let suffix = &line[colon + 1..];
        suffix.is_empty() || suffix.starts_with([' ', '\t', '\r'])
    })
}

fn validate_env_contents(contents: &str) -> Result<(), OperationError> {
    for line in contents.lines() {
        if line.is_empty() {
            return Err(OperationError::InvalidPlan(
                "environment write contains an empty line".to_owned(),
            ));
        }
        let (key, value) = line.split_once('=').ok_or_else(|| {
            OperationError::InvalidPlan("environment write is malformed".to_owned())
        })?;
        if key.is_empty()
            || !key
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            || value.contains(['\r', '\n', '\0'])
        {
            return Err(OperationError::InvalidPlan(
                "environment write contains an unsafe key or value".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(directory: &Path) -> LivePaths {
        LivePaths::try_new([
            (
                LogicalTarget::ClaudeSettings,
                directory.join(".claude/settings.json"),
            ),
            (
                LogicalTarget::ClaudeDesktopNormalConfig,
                directory.join("Claude/claude_desktop_config.json"),
            ),
            (
                LogicalTarget::ClaudeDesktopThreepConfig,
                directory.join("Claude-3p/claude_desktop_config.json"),
            ),
            (
                LogicalTarget::ClaudeDesktopProfile,
                directory.join("Claude-3p/configLibrary/profile.json"),
            ),
            (
                LogicalTarget::ClaudeDesktopMeta,
                directory.join("Claude-3p/configLibrary/_meta.json"),
            ),
            (LogicalTarget::CodexAuth, directory.join(".codex/auth.json")),
            (
                LogicalTarget::CodexConfig,
                directory.join(".codex/config.toml"),
            ),
            (
                LogicalTarget::CodexModelCatalog,
                directory.join(".codex/cc-switch-model-catalog.json"),
            ),
            (LogicalTarget::GeminiEnv, directory.join(".gemini/.env")),
            (
                LogicalTarget::GeminiSettings,
                directory.join(".gemini/settings.json"),
            ),
            (
                LogicalTarget::GrokConfig,
                directory.join(".grok/config.toml"),
            ),
            (
                LogicalTarget::OpenCodeConfig,
                directory.join(".config/opencode/opencode.json"),
            ),
            (
                LogicalTarget::OpenClawConfig,
                directory.join(".openclaw/openclaw.json"),
            ),
            (
                LogicalTarget::HermesConfig,
                directory.join(".hermes/config.yaml"),
            ),
            (
                LogicalTarget::PiModels,
                directory.join(".pi/agent/models.json"),
            ),
        ])
        .expect("complete test paths")
    }

    #[test]
    fn executor_rejects_raw_cross_application_targets() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "claude".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::CodexConfig,
                expected: ContentExpectation::Missing,
                contents: Some("model = \"gpt-5\"\n".to_owned()),
            }],
        };

        let result = OperationExecutor::new(&paths).execute(&plan);

        assert!(matches!(result, Err(OperationError::InvalidPlan(_))));
        assert!(!paths.path_for(LogicalTarget::CodexConfig).exists());
    }

    #[test]
    fn executor_checks_content_preconditions_before_writing() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        fs::create_dir_all(
            paths
                .path_for(LogicalTarget::ClaudeSettings)
                .parent()
                .unwrap(),
        )
        .unwrap();
        fs::write(paths.path_for(LogicalTarget::ClaudeSettings), "{}\n").unwrap();
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "claude".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::ClaudeSettings,
                expected: ContentExpectation::Missing,
                contents: Some("{\"env\":{}}\n".to_owned()),
            }],
        };

        let result = OperationExecutor::new(&paths).execute(&plan);

        assert!(matches!(result, Err(OperationError::Conflict)));
        assert_eq!(
            fs::read_to_string(paths.path_for(LogicalTarget::ClaudeSettings)).unwrap(),
            "{}\n"
        );
    }

    #[test]
    fn executor_writes_private_validated_content() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        let contents = "{\n  \"env\": {\"ANTHROPIC_AUTH_TOKEN\": \"secret\"}\n}\n";
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "claude".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::ClaudeSettings,
                expected: ContentExpectation::Missing,
                contents: Some(contents.to_owned()),
            }],
        };

        OperationExecutor::new(&paths)
            .execute(&plan)
            .expect("execute plan");

        assert_eq!(
            fs::read_to_string(paths.path_for(LogicalTarget::ClaudeSettings)).unwrap(),
            contents
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(paths.path_for(LogicalTarget::ClaudeSettings))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn executor_preserves_an_existing_unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        fs::create_dir_all(
            paths
                .path_for(LogicalTarget::ClaudeSettings)
                .parent()
                .unwrap(),
        )
        .unwrap();
        fs::write(paths.path_for(LogicalTarget::ClaudeSettings), b"{}\n").unwrap();
        fs::set_permissions(
            paths.path_for(LogicalTarget::ClaudeSettings),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "claude".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::ClaudeSettings,
                expected: ContentExpectation::for_contents(Some(b"{}\n")),
                contents: Some("{\"env\":{}}\n".to_owned()),
            }],
        };

        OperationExecutor::new(&paths)
            .execute(&plan)
            .expect("execute plan");

        assert_eq!(
            fs::metadata(paths.path_for(LogicalTarget::ClaudeSettings))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }

    #[test]
    fn capped_reader_rejects_oversized_live_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("large.json");
        let file = File::create(&path).expect("create sparse file");
        file.set_len((MAX_CONTENT_BYTES + 1) as u64)
            .expect("extend sparse file");

        assert!(matches!(
            read_optional(&path),
            Err(OperationError::TooLarge {
                limit: MAX_CONTENT_BYTES
            })
        ));
    }

    #[test]
    fn rollback_preserves_external_changes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        fs::create_dir_all(
            paths
                .path_for(LogicalTarget::ClaudeSettings)
                .parent()
                .unwrap(),
        )
        .unwrap();
        fs::write(paths.path_for(LogicalTarget::ClaudeSettings), b"original").unwrap();
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "claude".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::ClaudeSettings,
                expected: ContentExpectation::for_contents(Some(b"original")),
                contents: Some("{\"managed\":true}\n".to_owned()),
            }],
        };
        let receipt = OperationExecutor::new(&paths)
            .execute_recoverable(&plan)
            .expect("execute plan");
        fs::write(paths.path_for(LogicalTarget::ClaudeSettings), b"external").unwrap();

        let result = receipt.rollback();

        assert!(matches!(result, Err(OperationError::Rollback(_))));
        assert_eq!(
            fs::read(paths.path_for(LogicalTarget::ClaudeSettings)).unwrap(),
            b"external"
        );
    }

    #[test]
    fn managed_profile_deletion_is_recoverable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        fs::create_dir_all(
            paths
                .path_for(LogicalTarget::ClaudeDesktopProfile)
                .parent()
                .unwrap(),
        )
        .unwrap();
        fs::write(paths.path_for(LogicalTarget::ClaudeDesktopProfile), b"{}\n").unwrap();
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "claude-desktop".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::ClaudeDesktopProfile,
                expected: ContentExpectation::for_contents(Some(b"{}\n")),
                contents: None,
            }],
        };

        let receipt = OperationExecutor::new(&paths)
            .execute_recoverable(&plan)
            .expect("delete profile");
        assert!(!paths.path_for(LogicalTarget::ClaudeDesktopProfile).exists());

        receipt.rollback().expect("restore profile");
        assert_eq!(
            fs::read(paths.path_for(LogicalTarget::ClaudeDesktopProfile)).unwrap(),
            b"{}\n"
        );
    }

    #[test]
    fn managed_codex_catalog_deletion_is_recoverable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        fs::create_dir_all(
            paths
                .path_for(LogicalTarget::CodexModelCatalog)
                .parent()
                .unwrap(),
        )
        .unwrap();
        fs::write(
            paths.path_for(LogicalTarget::CodexModelCatalog),
            b"{\"models\":[]}\n",
        )
        .unwrap();
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "codex".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::CodexModelCatalog,
                expected: ContentExpectation::for_contents(Some(b"{\"models\":[]}\n")),
                contents: None,
            }],
        };

        let receipt = OperationExecutor::new(&paths)
            .execute_recoverable(&plan)
            .expect("delete model catalog");
        assert!(!paths.path_for(LogicalTarget::CodexModelCatalog).exists());

        receipt.rollback().expect("restore model catalog");
        assert_eq!(
            fs::read(paths.path_for(LogicalTarget::CodexModelCatalog)).unwrap(),
            b"{\"models\":[]}\n"
        );
    }

    #[test]
    fn hermes_write_rejects_duplicate_top_level_sections() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "hermes".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::HermesConfig,
                expected: ContentExpectation::Missing,
                contents: Some(
                    "custom_providers: []\ncustom_providers:\n  - name: duplicate\n".to_owned(),
                ),
            }],
        };

        assert!(matches!(
            OperationExecutor::new(&paths).execute(&plan),
            Err(OperationError::InvalidPlan(_))
        ));
    }

    #[test]
    fn arbitrary_targets_cannot_be_deleted() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "pi".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::PiModels,
                expected: ContentExpectation::Missing,
                contents: None,
            }],
        };

        assert!(matches!(
            OperationExecutor::new(&paths).execute(&plan),
            Err(OperationError::InvalidPlan(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn executor_rejects_a_final_symbolic_link() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        let target = directory.path().join("managed/settings.json");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::create_dir_all(
            paths
                .path_for(LogicalTarget::ClaudeSettings)
                .parent()
                .unwrap(),
        )
        .unwrap();
        fs::write(&target, b"{}\n").unwrap();
        symlink(&target, paths.path_for(LogicalTarget::ClaudeSettings)).unwrap();
        let plan = OperationPlan {
            contract_major: OPERATION_CONTRACT_MAJOR,
            app_id: "claude".to_owned(),
            writes: vec![PlannedWrite {
                target: LogicalTarget::ClaudeSettings,
                expected: ContentExpectation::for_contents(Some(b"{}\n")),
                contents: Some("{\"env\":{}}\n".to_owned()),
            }],
        };

        let result = OperationExecutor::new(&paths).execute(&plan);

        assert!(matches!(result, Err(OperationError::InvalidTarget(_))));
        assert!(
            fs::symlink_metadata(paths.path_for(LogicalTarget::ClaudeSettings))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "{}\n");
    }

    #[cfg(unix)]
    #[test]
    fn resolved_paths_bind_a_symbolic_link_parent_before_planning() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        let managed = directory.path().join("managed");
        fs::create_dir_all(&managed).unwrap();
        symlink(
            &managed,
            paths
                .path_for(LogicalTarget::ClaudeSettings)
                .parent()
                .unwrap(),
        )
        .unwrap();

        let resolved = paths
            .resolved_for_write(LogicalTarget::ClaudeSettings)
            .expect("resolve target");

        assert_eq!(
            resolved.path_for(LogicalTarget::ClaudeSettings),
            fs::canonicalize(managed).unwrap().join("settings.json")
        );
    }

    #[test]
    fn content_expectations_reject_unknown_fields() {
        let extra_missing = r#"{"state":"missing","digest":"ignored"}"#;
        let extra_digest = r#"{"state":"sha256","digest":"abc","future":true}"#;

        assert!(serde_json::from_str::<ContentExpectation>(extra_missing).is_err());
        assert!(serde_json::from_str::<ContentExpectation>(extra_digest).is_err());
    }
}
