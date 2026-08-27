use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use cc_switch_core::fs::{atomic_write, FileError};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use toml_edit::DocumentMut;

pub const OPERATION_CONTRACT_MAJOR: u32 = 1;
const MAX_OPERATIONS: usize = 3;
const MAX_CONTENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogicalTarget {
    ClaudeSettings,
    CodexConfig,
}

impl LogicalTarget {
    fn app_id(self) -> &'static str {
        match self {
            Self::ClaudeSettings => "claude",
            Self::CodexConfig => "codex",
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
    pub contents: String,
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
    pub codex_auth: PathBuf,
    pub codex_config: PathBuf,
}

impl LivePaths {
    fn path_for(&self, target: LogicalTarget) -> &Path {
        match target {
            LogicalTarget::ClaudeSettings => &self.claude_settings,
            LogicalTarget::CodexConfig => &self.codex_config,
        }
    }

    pub fn resolved_for_write(&self, target: LogicalTarget) -> Result<Self, OperationError> {
        let mut resolved = self.clone();
        let path = resolve_write_path(self.path_for(target))?;
        match target {
            LogicalTarget::ClaudeSettings => resolved.claude_settings = path,
            LogicalTarget::CodexConfig => resolved.codex_config = path,
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
    #[error("live configuration exceeds the {MAX_CONTENT_BYTES} byte limit")]
    TooLarge,
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
            Self::TooLarge => "live_too_large",
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
    written: Vec<u8>,
}

impl<'a> OperationExecutor<'a> {
    pub fn new(paths: &'a LivePaths) -> Self {
        Self { paths }
    }

    pub fn execute(&self, plan: &OperationPlan) -> Result<(), OperationError> {
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

            if let Err(error) = atomic_write(
                &prepared_write.path,
                prepared_write.write.contents.as_bytes(),
            ) {
                return Err(self.failure_after_rollback(OperationError::File(error), &applied));
            }
            applied.push(AppliedWrite {
                target: prepared_write.write.target,
                path: prepared_write.path,
                original: prepared_write.original,
                written: prepared_write.write.contents.as_bytes().to_vec(),
            });
        }
        Ok(())
    }

    fn validate(&self, plan: &OperationPlan) -> Result<(), OperationError> {
        if plan.contract_major != OPERATION_CONTRACT_MAJOR {
            return Err(OperationError::InvalidPlan(format!(
                "unsupported contract major {}",
                plan.contract_major
            )));
        }
        if !matches!(plan.app_id.as_str(), "claude" | "codex") {
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
            if write.contents.len() > MAX_CONTENT_BYTES {
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
            validate_contents(write.target, &write.contents)?;
        }
        Ok(())
    }

    fn failure_after_rollback(
        &self,
        error: OperationError,
        applied: &[AppliedWrite],
    ) -> OperationError {
        match self.rollback(applied) {
            Ok(()) => error,
            Err(rollback_error) => OperationError::Rollback(format!(
                "operation error: {error}; rollback error: {rollback_error}"
            )),
        }
    }

    fn rollback(&self, applied: &[AppliedWrite]) -> Result<(), OperationError> {
        let mut failures = Vec::new();
        for applied_write in applied.iter().rev() {
            let current = match read_optional(&applied_write.path) {
                Ok(current) => current,
                Err(error) => {
                    failures.push(error.to_string());
                    continue;
                }
            };
            if current.as_deref() != Some(applied_write.written.as_slice()) {
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
}

pub fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, OperationError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(OperationError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
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
    if metadata.len() > MAX_CONTENT_BYTES as u64 {
        return Err(OperationError::TooLarge);
    }

    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_CONTENT_BYTES + 1) as u64)
        .read_to_end(&mut contents)
        .map_err(|source| OperationError::Io {
            path: path.to_owned(),
            source,
        })?;
    if contents.len() > MAX_CONTENT_BYTES {
        return Err(OperationError::TooLarge);
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

fn validate_contents(target: LogicalTarget, contents: &str) -> Result<(), OperationError> {
    match target {
        LogicalTarget::ClaudeSettings => {
            let value: serde_json::Value = serde_json::from_str(contents).map_err(|error| {
                OperationError::InvalidPlan(format!("JSON write is invalid: {error}"))
            })?;
            if !value.is_object() {
                return Err(OperationError::InvalidPlan(
                    "JSON write must contain an object".to_owned(),
                ));
            }
        }
        LogicalTarget::CodexConfig => {
            contents
                .parse::<DocumentMut>()
                .map_err(|_| OperationError::InvalidPlan("TOML write is invalid".to_owned()))?;
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
            codex_auth: directory.join(".codex/auth.json"),
            codex_config: directory.join(".codex/config.toml"),
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
                contents: "model = \"gpt-5\"\n".to_owned(),
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
                contents: "{\"env\":{}}\n".to_owned(),
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
                contents: contents.to_owned(),
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
                contents: "{\"env\":{}}\n".to_owned(),
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
            Err(OperationError::TooLarge)
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
            written: b"lite".to_vec(),
        };

        let result = OperationExecutor::new(&paths).rollback(&[applied]);

        assert!(matches!(result, Err(OperationError::Rollback(_))));
        assert_eq!(fs::read(paths.claude_settings).unwrap(), b"external");
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
                contents: "{\"env\":{}}\n".to_owned(),
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
