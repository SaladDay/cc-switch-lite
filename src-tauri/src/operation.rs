use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use cc_switch_core::fs::{atomic_write, FileError};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use toml_edit::DocumentMut;

pub const OPERATION_CONTRACT_MAJOR: u32 = 1;
const MAX_OPERATIONS: usize = 4;
const MAX_CONTENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogicalTarget {
    ClaudeSettings,
    ClaudeDesktopNormalConfig,
    ClaudeDesktopThreepConfig,
    ClaudeDesktopProfile,
    ClaudeDesktopMeta,
    CodexAuth,
    CodexConfig,
    CodexModelCatalog,
    GeminiEnv,
    GeminiSettings,
    GrokConfig,
    OpenCodeConfig,
    OpenClawConfig,
    HermesConfig,
    PiModels,
}

impl LogicalTarget {
    pub(crate) fn app_id(self) -> &'static str {
        match self {
            Self::ClaudeSettings => "claude",
            Self::ClaudeDesktopNormalConfig
            | Self::ClaudeDesktopThreepConfig
            | Self::ClaudeDesktopProfile
            | Self::ClaudeDesktopMeta => "claude-desktop",
            Self::CodexAuth | Self::CodexConfig | Self::CodexModelCatalog => "codex",
            Self::GeminiEnv | Self::GeminiSettings => "gemini",
            Self::GrokConfig => "grokbuild",
            Self::OpenCodeConfig => "opencode",
            Self::OpenClawConfig => "openclaw",
            Self::HermesConfig => "hermes",
            Self::PiModels => "pi",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ContentExpectation {
    Missing,
    Sha256 { digest: String },
}

impl<'de> Deserialize<'de> for ContentExpectation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "state", rename_all = "camelCase")]
        enum Wire {
            Missing {
                #[serde(flatten)]
                extra: std::collections::BTreeMap<String, serde_json::Value>,
            },
            Sha256 {
                digest: String,
                #[serde(flatten)]
                extra: std::collections::BTreeMap<String, serde_json::Value>,
            },
        }

        match Wire::deserialize(deserializer)? {
            Wire::Missing { extra } if extra.is_empty() => Ok(Self::Missing),
            Wire::Sha256 { digest, extra } if extra.is_empty() => Ok(Self::Sha256 { digest }),
            Wire::Missing { .. } | Wire::Sha256 { .. } => Err(serde::de::Error::custom(
                "unknown content expectation field",
            )),
        }
    }
}

impl ContentExpectation {
    pub fn for_contents(contents: Option<&[u8]>) -> Self {
        match contents {
            Some(contents) => Self::Sha256 {
                digest: sha256(contents),
            },
            None => Self::Missing,
        }
    }

    fn matches(&self, contents: Option<&[u8]>) -> bool {
        match (self, contents) {
            (Self::Missing, None) => true,
            (Self::Sha256 { digest }, Some(contents)) => *digest == sha256(contents),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlannedWrite {
    pub target: LogicalTarget,
    pub expected: ContentExpectation,
    pub contents: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationPlan {
    pub contract_major: u32,
    pub app_id: String,
    pub writes: Vec<PlannedWrite>,
}

#[derive(Debug, Clone)]
pub struct LivePaths {
    pub claude_settings: PathBuf,
    pub claude_desktop_normal_config: PathBuf,
    pub claude_desktop_threep_config: PathBuf,
    pub claude_desktop_profile: PathBuf,
    pub claude_desktop_meta: PathBuf,
    pub codex_auth: PathBuf,
    pub codex_config: PathBuf,
    pub codex_model_catalog: PathBuf,
    pub gemini_env: PathBuf,
    pub gemini_settings: PathBuf,
    pub grok_config: PathBuf,
    pub opencode_config: PathBuf,
    pub openclaw_config: PathBuf,
    pub hermes_config: PathBuf,
    pub pi_models: PathBuf,
}

impl LivePaths {
    pub(crate) fn path_for(&self, target: LogicalTarget) -> &Path {
        match target {
            LogicalTarget::ClaudeSettings => &self.claude_settings,
            LogicalTarget::ClaudeDesktopNormalConfig => &self.claude_desktop_normal_config,
            LogicalTarget::ClaudeDesktopThreepConfig => &self.claude_desktop_threep_config,
            LogicalTarget::ClaudeDesktopProfile => &self.claude_desktop_profile,
            LogicalTarget::ClaudeDesktopMeta => &self.claude_desktop_meta,
            LogicalTarget::CodexAuth => &self.codex_auth,
            LogicalTarget::CodexConfig => &self.codex_config,
            LogicalTarget::CodexModelCatalog => &self.codex_model_catalog,
            LogicalTarget::GeminiEnv => &self.gemini_env,
            LogicalTarget::GeminiSettings => &self.gemini_settings,
            LogicalTarget::GrokConfig => &self.grok_config,
            LogicalTarget::OpenCodeConfig => &self.opencode_config,
            LogicalTarget::OpenClawConfig => &self.openclaw_config,
            LogicalTarget::HermesConfig => &self.hermes_config,
            LogicalTarget::PiModels => &self.pi_models,
        }
    }

    pub fn resolved_for_write(&self, target: LogicalTarget) -> Result<Self, OperationError> {
        let mut resolved = self.clone();
        let path = resolve_write_path(self.path_for(target))?;
        match target {
            LogicalTarget::ClaudeSettings => resolved.claude_settings = path,
            LogicalTarget::ClaudeDesktopNormalConfig => {
                resolved.claude_desktop_normal_config = path
            }
            LogicalTarget::ClaudeDesktopThreepConfig => {
                resolved.claude_desktop_threep_config = path
            }
            LogicalTarget::ClaudeDesktopProfile => resolved.claude_desktop_profile = path,
            LogicalTarget::ClaudeDesktopMeta => resolved.claude_desktop_meta = path,
            LogicalTarget::CodexAuth => resolved.codex_auth = path,
            LogicalTarget::CodexConfig => resolved.codex_config = path,
            LogicalTarget::CodexModelCatalog => resolved.codex_model_catalog = path,
            LogicalTarget::GeminiEnv => resolved.gemini_env = path,
            LogicalTarget::GeminiSettings => resolved.gemini_settings = path,
            LogicalTarget::GrokConfig => resolved.grok_config = path,
            LogicalTarget::OpenCodeConfig => resolved.opencode_config = path,
            LogicalTarget::OpenClawConfig => resolved.openclaw_config = path,
            LogicalTarget::HermesConfig => resolved.hermes_config = path,
            LogicalTarget::PiModels => resolved.pi_models = path,
        }
        Ok(resolved)
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

struct PreparedWrite<'a> {
    write: &'a PlannedWrite,
    path: PathBuf,
    original: Option<Vec<u8>>,
}

struct AppliedWrite {
    target: LogicalTarget,
    path: PathBuf,
    original: Option<Vec<u8>>,
    written: Option<Vec<u8>>,
}

pub(crate) struct OperationReceipt {
    applied: Vec<AppliedWrite>,
}

impl OperationReceipt {
    pub(crate) fn rollback(self) -> Result<(), OperationError> {
        rollback_applied(&self.applied)
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

        let mut paths = HashSet::new();
        let mut prepared = Vec::with_capacity(plan.writes.len());
        for write in &plan.writes {
            let path = resolve_write_path(self.paths.path_for(write.target))?;
            if !paths.insert(path.clone()) {
                return Err(OperationError::InvalidPlan(
                    "logical targets resolve to the same file".to_owned(),
                ));
            }
            let original = read_optional(&path)?;
            if !write.expected.matches(original.as_deref()) {
                return Err(OperationError::Conflict);
            }
            prepared.push(PreparedWrite {
                write,
                path,
                original,
            });
        }

        let mut applied = Vec::with_capacity(prepared.len());
        for prepared_write in prepared {
            let current = match read_optional(&prepared_write.path) {
                Ok(current) => current,
                Err(error) => return Err(self.failure_after_rollback(error, &applied)),
            };
            if current != prepared_write.original
                || !prepared_write.write.expected.matches(current.as_deref())
            {
                return Err(self.failure_after_rollback(OperationError::Conflict, &applied));
            }

            let written = match &prepared_write.write.contents {
                Some(contents) => {
                    if let Err(error) = atomic_write(&prepared_write.path, contents.as_bytes()) {
                        return Err(
                            self.failure_after_rollback(OperationError::File(error), &applied)
                        );
                    }
                    Some(contents.as_bytes().to_vec())
                }
                None => match fs::remove_file(&prepared_write.path) {
                    Ok(()) => None,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(source) => {
                        return Err(self.failure_after_rollback(
                            OperationError::Io {
                                path: prepared_write.path.clone(),
                                source,
                            },
                            &applied,
                        ));
                    }
                },
            };
            applied.push(AppliedWrite {
                target: prepared_write.write.target,
                path: prepared_write.path,
                original: prepared_write.original,
                written,
            });
        }
        Ok(OperationReceipt { applied })
    }

    fn validate(&self, plan: &OperationPlan) -> Result<(), OperationError> {
        if plan.contract_major != OPERATION_CONTRACT_MAJOR {
            return Err(OperationError::InvalidPlan(format!(
                "unsupported contract major {}",
                plan.contract_major
            )));
        }
        if !cc_switch_core::AppType::all().any(|app| app.as_str() == plan.app_id) {
            return Err(OperationError::InvalidPlan(
                "application is not available in Lite".to_owned(),
            ));
        }
        if plan.writes.is_empty() || plan.writes.len() > MAX_OPERATIONS {
            return Err(OperationError::InvalidPlan(format!(
                "a plan must contain between 1 and {MAX_OPERATIONS} writes"
            )));
        }

        let mut targets = HashSet::new();
        for write in &plan.writes {
            if write.target.app_id() != plan.app_id {
                return Err(OperationError::InvalidPlan(
                    "a write targets a different application".to_owned(),
                ));
            }
            if !targets.insert(write.target) {
                return Err(OperationError::InvalidPlan(
                    "a logical target appears more than once".to_owned(),
                ));
            }
            if write
                .contents
                .as_ref()
                .is_some_and(|contents| contents.len() > MAX_CONTENT_BYTES)
            {
                return Err(OperationError::InvalidPlan(format!(
                    "a write exceeds the {MAX_CONTENT_BYTES} byte limit"
                )));
            }
            if let ContentExpectation::Sha256 { digest } = &write.expected {
                if digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                {
                    return Err(OperationError::InvalidPlan(
                        "a SHA-256 precondition is malformed".to_owned(),
                    ));
                }
            }
            validate_contents(write.target, write.contents.as_deref())?;
        }
        Ok(())
    }

    fn failure_after_rollback(
        &self,
        error: OperationError,
        applied: &[AppliedWrite],
    ) -> OperationError {
        match rollback_applied(applied) {
            Ok(()) => error,
            Err(rollback_error) => OperationError::Rollback(format!(
                "operation error: {error}; rollback error: {rollback_error}"
            )),
        }
    }
}

fn rollback_applied(applied: &[AppliedWrite]) -> Result<(), OperationError> {
    let mut failures = Vec::new();
    for applied_write in applied.iter().rev() {
        let current = match read_optional_no_follow(&applied_write.path, MAX_CONTENT_BYTES) {
            Ok(current) => current,
            Err(error) => {
                failures.push(error.to_string());
                continue;
            }
        };
        if current.as_deref() != applied_write.written.as_deref() {
            failures.push(format!(
                "{:?} changed after Lite wrote it; external contents were preserved",
                applied_write.target
            ));
            continue;
        }

        let result = match &applied_write.original {
            Some(contents) => {
                atomic_write(&applied_write.path, contents).map_err(OperationError::File)
            }
            None => match fs::remove_file(&applied_write.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(OperationError::Io {
                    path: applied_write.path.clone(),
                    source,
                }),
            },
        };
        if let Err(error) = result {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(OperationError::Rollback(failures.join("; ")))
    }
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

fn resolve_write_path(path: &Path) -> Result<PathBuf, OperationError> {
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
        return if matches!(
            target,
            LogicalTarget::ClaudeDesktopProfile
                | LogicalTarget::CodexAuth
                | LogicalTarget::CodexModelCatalog
        ) {
            Ok(())
        } else {
            Err(OperationError::InvalidPlan(
                "only Lite-managed authentication, profile, or catalog files may be removed"
                    .to_owned(),
            ))
        };
    };
    match target {
        LogicalTarget::ClaudeSettings
        | LogicalTarget::ClaudeDesktopNormalConfig
        | LogicalTarget::ClaudeDesktopThreepConfig
        | LogicalTarget::ClaudeDesktopProfile
        | LogicalTarget::ClaudeDesktopMeta
        | LogicalTarget::CodexAuth
        | LogicalTarget::CodexModelCatalog
        | LogicalTarget::GeminiSettings
        | LogicalTarget::OpenCodeConfig
        | LogicalTarget::PiModels => {
            let value: serde_json::Value = serde_json::from_str(contents).map_err(|error| {
                OperationError::InvalidPlan(format!("JSON write is invalid: {error}"))
            })?;
            if !value.is_object() {
                return Err(OperationError::InvalidPlan(
                    "JSON write must contain an object".to_owned(),
                ));
            }
        }
        LogicalTarget::CodexConfig | LogicalTarget::GrokConfig => {
            contents
                .parse::<DocumentMut>()
                .map_err(|_| OperationError::InvalidPlan("TOML write is invalid".to_owned()))?;
        }
        LogicalTarget::OpenClawConfig => {
            let value: serde_json::Value = json5::from_str(contents)
                .map_err(|_| OperationError::InvalidPlan("JSON5 write is invalid".to_owned()))?;
            if !value.is_object() {
                return Err(OperationError::InvalidPlan(
                    "JSON5 write must contain an object".to_owned(),
                ));
            }
        }
        LogicalTarget::GeminiEnv => validate_env_contents(contents)?,
        LogicalTarget::HermesConfig => {
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

pub(crate) fn sha256(contents: &[u8]) -> String {
    let digest = Sha256::digest(contents);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(directory: &Path) -> LivePaths {
        LivePaths {
            claude_settings: directory.join(".claude/settings.json"),
            claude_desktop_normal_config: directory.join("Claude/claude_desktop_config.json"),
            claude_desktop_threep_config: directory.join("Claude-3p/claude_desktop_config.json"),
            claude_desktop_profile: directory.join("Claude-3p/configLibrary/profile.json"),
            claude_desktop_meta: directory.join("Claude-3p/configLibrary/_meta.json"),
            codex_auth: directory.join(".codex/auth.json"),
            codex_config: directory.join(".codex/config.toml"),
            codex_model_catalog: directory.join(".codex/cc-switch-model-catalog.json"),
            gemini_env: directory.join(".gemini/.env"),
            gemini_settings: directory.join(".gemini/settings.json"),
            grok_config: directory.join(".grok/config.toml"),
            opencode_config: directory.join(".config/opencode/opencode.json"),
            openclaw_config: directory.join(".openclaw/openclaw.json"),
            hermes_config: directory.join(".hermes/config.yaml"),
            pi_models: directory.join(".pi/agent/models.json"),
        }
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
        assert!(!paths.codex_config.exists());
    }

    #[test]
    fn executor_checks_content_preconditions_before_writing() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(&paths.claude_settings, "{}\n").unwrap();
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
        assert_eq!(fs::read_to_string(paths.claude_settings).unwrap(), "{}\n");
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
            fs::read_to_string(&paths.claude_settings).unwrap(),
            contents
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(paths.claude_settings)
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
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(&paths.claude_settings, b"{}\n").unwrap();
        fs::set_permissions(&paths.claude_settings, fs::Permissions::from_mode(0o640)).unwrap();
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
            fs::metadata(paths.claude_settings)
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
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(&paths.claude_settings, b"external").unwrap();
        let applied = AppliedWrite {
            target: LogicalTarget::ClaudeSettings,
            path: paths.claude_settings.clone(),
            original: Some(b"original".to_vec()),
            written: Some(b"lite".to_vec()),
        };

        let result = rollback_applied(&[applied]);

        assert!(matches!(result, Err(OperationError::Rollback(_))));
        assert_eq!(fs::read(paths.claude_settings).unwrap(), b"external");
    }

    #[test]
    fn managed_profile_deletion_is_recoverable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        fs::create_dir_all(paths.claude_desktop_profile.parent().unwrap()).unwrap();
        fs::write(&paths.claude_desktop_profile, b"{}\n").unwrap();
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
        assert!(!paths.claude_desktop_profile.exists());

        receipt.rollback().expect("restore profile");
        assert_eq!(fs::read(&paths.claude_desktop_profile).unwrap(), b"{}\n");
    }

    #[test]
    fn managed_codex_catalog_deletion_is_recoverable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let paths = paths(directory.path());
        fs::create_dir_all(paths.codex_model_catalog.parent().unwrap()).unwrap();
        fs::write(&paths.codex_model_catalog, b"{\"models\":[]}\n").unwrap();
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
        assert!(!paths.codex_model_catalog.exists());

        receipt.rollback().expect("restore model catalog");
        assert_eq!(
            fs::read(&paths.codex_model_catalog).unwrap(),
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
        fs::create_dir_all(paths.claude_settings.parent().unwrap()).unwrap();
        fs::write(&target, b"{}\n").unwrap();
        symlink(&target, &paths.claude_settings).unwrap();
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
        assert!(fs::symlink_metadata(&paths.claude_settings)
            .unwrap()
            .file_type()
            .is_symlink());
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
        symlink(&managed, paths.claude_settings.parent().unwrap()).unwrap();

        let resolved = paths
            .resolved_for_write(LogicalTarget::ClaudeSettings)
            .expect("resolve target");

        assert_eq!(
            resolved.claude_settings,
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
