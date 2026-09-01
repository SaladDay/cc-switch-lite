use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

#[cfg(test)]
use cc_switch_core::apply_skill_deployment;
use cc_switch_core::{
    apply_skill_deployment_with_policy, builtin_app_registry, inspect_skill_config_state,
    inspect_skill_deployment_with_policy, inspect_skill_discovery_with_policy,
    inspect_skill_presence, resolve_skill_effective_state, resolve_skill_route, skill_name_key,
    skill_path_identity, validate_skill_directory, validate_skill_source, AppType,
    SkillConfigError, SkillConfigState, SkillConfigTarget, SkillControlReason, SkillCopyPolicy,
    SkillDeploymentReceipt, SkillDeploymentState, SkillDiscoveryMode, SkillDiscoveryState,
    SkillRoute, SkillSyncMethod,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::skill::SkillAppState;

#[derive(Debug, Error)]
pub enum SkillLiveError {
    #[error("application '{0}' does not support Skills")]
    Unsupported(String),
    #[error("{0} is not installed or its configuration directory is unavailable")]
    AppUnavailable(String),
    #[error("{0} configuration path is not a directory: {1:?}")]
    InvalidAppRoot(String, PathBuf),
    #[error("Skill application path is unavailable for '{0}'")]
    MissingTarget(String),
    #[error("Skill application targets are invalid: {0}")]
    InvalidTargets(String),
    #[error(
        "{0} discovers the unified Skill store directly; its effective state is managed outside Lite"
    )]
    UnifiedDiscovery(String),
    #[error("{0} finds an unrelated same-directory Skill in the unified store")]
    UnifiedConflict(String),
    #[error(transparent)]
    Config(#[from] SkillConfigError),
    #[error("Skill filesystem I/O failed at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub(crate) struct SkillObservation {
    pub(crate) id: String,
    pub(crate) source_issue: Option<String>,
    pub(crate) app_overrides: BTreeMap<String, SkillAppState>,
}

pub(crate) struct SkillObservationRequest {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) directory: String,
    pub(crate) selected_apps: HashMap<AppType, bool>,
}

pub(crate) type SkillConfigDocuments = HashMap<SkillConfigTarget, Result<Option<Vec<u8>>, String>>;

pub(crate) struct SkillAppResources {
    app: AppType,
    install_root: PathBuf,
    config_path: Option<PathBuf>,
}

impl SkillAppResources {
    pub(crate) fn new(app: AppType, install_root: PathBuf, config_path: Option<PathBuf>) -> Self {
        Self {
            app,
            install_root,
            config_path,
        }
    }
}

struct SkillTarget {
    app: AppType,
    install_root: PathBuf,
    skills_root: PathBuf,
    config_path: Option<PathBuf>,
    issue: Option<String>,
}

pub(crate) struct SkillLiveConfig {
    source_root: PathBuf,
    unified_discovery_root: PathBuf,
    sync_method: SkillSyncMethod,
    targets: Vec<SkillTarget>,
}

impl SkillLiveConfig {
    pub(crate) fn new(
        source_root: PathBuf,
        unified_discovery_root: PathBuf,
        sync_method: SkillSyncMethod,
        resources: Vec<SkillAppResources>,
    ) -> Result<Self, SkillLiveError> {
        validate_targets(&resources)?;
        let mut targets = resources
            .into_iter()
            .map(|resources| SkillTarget {
                skills_root: resources.install_root.join("skills"),
                app: resources.app,
                install_root: resources.install_root,
                config_path: resources.config_path,
                issue: None,
            })
            .collect::<Vec<_>>();
        mark_overlapping_targets(&mut targets, &source_root, &unified_discovery_root);
        Ok(Self {
            source_root,
            unified_discovery_root,
            sync_method,
            targets,
        })
    }

    pub(crate) fn observe(
        &self,
        skills: &[SkillObservationRequest],
        configs: &SkillConfigDocuments,
    ) -> Vec<SkillObservation> {
        let mut name_counts = HashMap::new();
        for skill in skills {
            if let Ok(key) = skill_name_key(&skill.name) {
                *name_counts.entry(key).or_insert(0_usize) += 1;
            }
        }
        skills
            .iter()
            .map(|skill| {
                let directory_issue = validate_skill_directory(&skill.directory)
                    .err()
                    .map(|error| error.to_string());
                let source_issue = directory_issue.clone().or_else(|| {
                    validate_skill_source(&self.source_root.join(&skill.directory))
                        .err()
                        .map(|error| error.to_string())
                });
                let app_overrides = if directory_issue.is_some() {
                    unknown_app_states()
                } else {
                    let ambiguous_name = skill_name_key(&skill.name)
                        .ok()
                        .is_some_and(|key| name_counts.get(&key).copied().unwrap_or_default() > 1);
                    self.observe_app_overrides(
                        &skill.name,
                        &skill.directory,
                        source_issue.is_none(),
                        ambiguous_name,
                        &skill.selected_apps,
                        configs,
                    )
                };
                SkillObservation {
                    id: skill.id.clone(),
                    source_issue,
                    app_overrides,
                }
            })
            .collect()
    }

    pub(crate) fn route(
        &self,
        directory: &str,
        app: &AppType,
        copy_policy: SkillCopyPolicy,
    ) -> Result<SkillRoute, SkillLiveError> {
        let descriptor = builtin_app_registry().for_app(app);
        let Some(contract) = descriptor.skill_contract() else {
            return Err(SkillLiveError::Unsupported(app.as_str().to_owned()));
        };
        let discovery = match contract.discovery() {
            SkillDiscoveryMode::Unified => inspect_skill_discovery_with_policy(
                &self.source_root,
                &self.unified_discovery_root,
                directory,
                copy_policy,
            )?,
            SkillDiscoveryMode::Managed => SkillDiscoveryState::Missing,
        };
        let route = resolve_skill_route(*contract, discovery)
            .map_err(|reason| control_reason_error(descriptor.display_name(), reason))?;
        let config_target = route.config_target();
        let deploy_native = route.deploy_native();
        if config_target.is_some() || deploy_native {
            let target = self
                .target(app)
                .ok_or_else(|| SkillLiveError::MissingTarget(app.as_str().to_owned()))?;
            require_safe_target(target)?;
            if config_target.is_some() {
                require_app_root(descriptor.display_name(), target)?;
            }
        }
        Ok(route)
    }

    pub(crate) fn apply_deployment(
        &self,
        directory: &str,
        app: &AppType,
        enabled: bool,
        copy_policy: SkillCopyPolicy,
    ) -> Result<SkillDeploymentReceipt, SkillLiveError> {
        let descriptor = builtin_app_registry().for_app(app);
        if descriptor.skill_contract().is_none() {
            return Err(SkillLiveError::Unsupported(app.as_str().to_owned()));
        }
        let target = self
            .target(app)
            .ok_or_else(|| SkillLiveError::MissingTarget(app.as_str().to_owned()))?;
        require_safe_target(target)?;
        if enabled {
            require_app_root(descriptor.display_name(), target)?;
        }
        apply_skill_deployment_with_policy(
            &self.source_root,
            &target.skills_root,
            directory,
            enabled,
            self.sync_method,
            copy_policy,
        )
        .map_err(Into::into)
    }

    pub(crate) fn runtime_fingerprint(&self, app: &AppType) -> Result<String, SkillLiveError> {
        let target = self
            .target(app)
            .ok_or_else(|| SkillLiveError::MissingTarget(app.as_str().to_owned()))?;
        require_safe_target(target)?;
        let contract = builtin_app_registry()
            .for_app(app)
            .skill_contract()
            .ok_or_else(|| SkillLiveError::Unsupported(app.as_str().to_owned()))?;
        let mut hasher = Sha256::new();
        hash_fingerprint_field(&mut hasher, b"cc-switch-lite-skill-runtime-v1");
        hash_fingerprint_field(&mut hasher, app.as_str().as_bytes());
        for path in [
            &self.source_root,
            &self.unified_discovery_root,
            &target.install_root,
            &target.skills_root,
        ] {
            hash_fingerprint_path(&mut hasher, &skill_path_identity(path)?);
        }
        hash_fingerprint_field(
            &mut hasher,
            match self.sync_method {
                SkillSyncMethod::Auto => b"auto",
                SkillSyncMethod::Symlink => b"symlink",
                SkillSyncMethod::Copy => b"copy",
            },
        );
        if let Some(config_target) = contract.config_target() {
            let label = match config_target {
                SkillConfigTarget::GeminiSettings => b"gemini-settings".as_slice(),
                SkillConfigTarget::GrokConfig => b"grok-config".as_slice(),
                SkillConfigTarget::HermesConfig => b"hermes-config".as_slice(),
            };
            let config_path = target.config_path.as_ref().ok_or_else(|| {
                SkillLiveError::MissingTarget(format!("{} configuration", app.as_str()))
            })?;
            hash_fingerprint_field(&mut hasher, label);
            hash_fingerprint_path(&mut hasher, &skill_path_identity(config_path)?);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    fn observe_app_overrides(
        &self,
        name: &str,
        directory: &str,
        source_valid: bool,
        ambiguous_name: bool,
        selected_apps: &HashMap<AppType, bool>,
        configs: &SkillConfigDocuments,
    ) -> BTreeMap<String, SkillAppState> {
        builtin_app_registry()
            .descriptors()
            .filter_map(|descriptor| {
                let contract = descriptor.skill_contract()?;
                Some((descriptor, contract))
            })
            .map(|(descriptor, contract)| {
                let copy_policy = selected_apps
                    .get(descriptor.app())
                    .copied()
                    .filter(|selected| *selected)
                    .map_or(SkillCopyPolicy::ManagedOnly, |_| {
                        SkillCopyPolicy::AllowMatching
                    });
                let native_observation = || {
                    self.target(descriptor.app())
                        .map(|target| {
                            match require_safe_target(target)
                                .and_then(|()| require_app_root(descriptor.display_name(), target))
                            {
                                Ok(()) => observe_managed_presence(
                                    &self.source_root,
                                    &target.skills_root,
                                    directory,
                                    source_valid,
                                    copy_policy,
                                ),
                                Err(error) => SkillAppState {
                                    enabled: None,
                                    issue: Some(error.to_string()),
                                },
                            }
                        })
                        .unwrap_or_else(|| SkillAppState {
                            enabled: None,
                            issue: Some(
                                SkillLiveError::MissingTarget(descriptor.id().to_owned())
                                    .to_string(),
                            ),
                        })
                };
                let discovery = if contract.discovery() == SkillDiscoveryMode::Unified {
                    inspect_skill_discovery_with_policy(
                        &self.source_root,
                        &self.unified_discovery_root,
                        directory,
                        copy_policy,
                    )
                    .map_err(|error| error.to_string())
                } else {
                    Ok(SkillDiscoveryState::Missing)
                };
                let observation = match discovery {
                    Err(issue) => SkillAppState {
                        enabled: None,
                        issue: Some(issue),
                    },
                    Ok(discovery) => {
                        let direct = contract.discovery() == SkillDiscoveryMode::Unified
                            && contract.config_target().is_none()
                            && discovery != SkillDiscoveryState::Missing;
                        let native = if direct {
                            SkillAppState {
                                enabled: None,
                                issue: None,
                            }
                        } else {
                            native_observation()
                        };
                        let config_state = contract
                            .config_target()
                            .map(|target| observe_config_state(target, name, configs));
                        match config_state.transpose() {
                            Ok(config_state) => apply_effective_state(
                                descriptor.display_name(),
                                *contract,
                                discovery,
                                native,
                                config_state,
                            ),
                            Err(issue) => append_issue(
                                SkillAppState {
                                    enabled: None,
                                    issue: native.issue,
                                },
                                issue,
                            ),
                        }
                    }
                };
                let observation = if ambiguous_name && contract.config_target().is_some() {
                    append_issue(
                        observation,
                        "Multiple installed Skills use this native name; switching is read-only"
                            .to_owned(),
                    )
                } else {
                    observation
                };
                (descriptor.id().to_owned(), observation)
            })
            .collect()
    }

    fn target(&self, app: &AppType) -> Option<&SkillTarget> {
        self.targets.iter().find(|target| &target.app == app)
    }
}

fn require_app_root(display_name: &str, target: &SkillTarget) -> Result<(), SkillLiveError> {
    match fs::metadata(&target.install_root) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(SkillLiveError::InvalidAppRoot(
            display_name.to_owned(),
            target.install_root.clone(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(SkillLiveError::AppUnavailable(display_name.to_owned()))
        }
        Err(source) => Err(SkillLiveError::Io {
            path: target.install_root.clone(),
            source,
        }),
    }
}

fn require_safe_target(target: &SkillTarget) -> Result<(), SkillLiveError> {
    match &target.issue {
        Some(issue) => Err(SkillLiveError::InvalidTargets(issue.clone())),
        None => Ok(()),
    }
}

fn observe_managed_presence(
    source_root: &Path,
    destination_root: &Path,
    directory: &str,
    source_valid: bool,
    copy_policy: SkillCopyPolicy,
) -> SkillAppState {
    match inspect_skill_presence(destination_root, directory) {
        Ok(false) => SkillAppState {
            enabled: Some(false),
            issue: None,
        },
        Ok(true) => {
            if !source_valid {
                return SkillAppState {
                    enabled: None,
                    issue: None,
                };
            }
            match inspect_skill_deployment_with_policy(
                source_root,
                destination_root,
                directory,
                copy_policy,
            ) {
                Ok(SkillDeploymentState::Linked | SkillDeploymentState::Copied) => SkillAppState {
                    enabled: Some(true),
                    issue: None,
                },
                Ok(SkillDeploymentState::Missing) => SkillAppState {
                    enabled: None,
                    issue: Some("native Skill changed while it was inspected".to_owned()),
                },
                Err(error) => SkillAppState {
                    enabled: None,
                    issue: Some(error.to_string()),
                },
            }
        }
        Err(error) => SkillAppState {
            enabled: None,
            issue: Some(error.to_string()),
        },
    }
}

fn observe_config_state(
    target: SkillConfigTarget,
    name: &str,
    configs: &SkillConfigDocuments,
) -> Result<SkillConfigState, String> {
    configs
        .get(&target)
        .ok_or_else(|| "native Skill settings were not observed".to_owned())
        .and_then(|contents| {
            contents
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|contents| {
                    inspect_skill_config_state(target, contents.as_deref(), name)
                        .map_err(|error| error.to_string())
                })
        })
}

fn apply_effective_state(
    display_name: &str,
    contract: cc_switch_core::SkillAppContract,
    discovery: SkillDiscoveryState,
    native: SkillAppState,
    config_state: Option<SkillConfigState>,
) -> SkillAppState {
    let effective =
        resolve_skill_effective_state(contract, discovery, native.enabled, config_state);
    let observation = SkillAppState {
        enabled: effective.enabled(),
        issue: native.issue,
    };
    match effective.reason() {
        Some(reason) => append_issue(observation, control_reason_message(display_name, reason)),
        None => observation,
    }
}

fn control_reason_error(display_name: &str, reason: SkillControlReason) -> SkillLiveError {
    match reason {
        SkillControlReason::ExternalDiscovery => {
            SkillLiveError::UnifiedConflict(display_name.to_owned())
        }
        SkillControlReason::DirectUnifiedDiscovery => {
            SkillLiveError::UnifiedDiscovery(display_name.to_owned())
        }
        _ => SkillLiveError::InvalidTargets(control_reason_message(display_name, reason)),
    }
}

fn control_reason_message(display_name: &str, reason: SkillControlReason) -> String {
    match reason {
        SkillControlReason::ExternalDiscovery => {
            SkillLiveError::UnifiedConflict(display_name.to_owned()).to_string()
        }
        SkillControlReason::DirectUnifiedDiscovery => {
            SkillLiveError::UnifiedDiscovery(display_name.to_owned()).to_string()
        }
        SkillControlReason::GloballyDisabled => {
            "Skills are disabled globally in the application's native settings".to_owned()
        }
        SkillControlReason::ExternallyDisabled => {
            "This Skill is disabled by a platform-specific native setting".to_owned()
        }
        SkillControlReason::NativeControlUnavailable => {
            "The application's native Skill control is unavailable".to_owned()
        }
    }
}

fn append_issue(mut observation: SkillAppState, issue: String) -> SkillAppState {
    observation.issue = Some(match observation.issue {
        Some(existing) => format!("{issue}; {existing}"),
        None => issue,
    });
    observation
}

fn unknown_app_states() -> BTreeMap<String, SkillAppState> {
    builtin_app_registry()
        .descriptors()
        .filter(|descriptor| descriptor.skill_contract().is_some())
        .map(|descriptor| {
            (
                descriptor.id().to_owned(),
                SkillAppState {
                    enabled: None,
                    issue: None,
                },
            )
        })
        .collect()
}

fn validate_targets(resources: &[SkillAppResources]) -> Result<(), SkillLiveError> {
    for descriptor in builtin_app_registry().descriptors() {
        let expected = usize::from(descriptor.skill_contract().is_some());
        let matching = resources
            .iter()
            .filter(|resources| &resources.app == descriptor.app())
            .collect::<Vec<_>>();
        if matching.len() != expected {
            return Err(SkillLiveError::InvalidTargets(format!(
                "application '{}' requires {expected} target(s), found {}",
                descriptor.id(),
                matching.len()
            )));
        }
        if let (Some(contract), Some(resources)) = (descriptor.skill_contract(), matching.first()) {
            if contract.config_target().is_some() != resources.config_path.is_some() {
                return Err(SkillLiveError::InvalidTargets(format!(
                    "application '{}' has an invalid Skill configuration resource",
                    descriptor.id()
                )));
            }
        }
    }
    Ok(())
}

fn mark_overlapping_targets(
    targets: &mut [SkillTarget],
    source_root: &Path,
    unified_discovery_root: &Path,
) {
    let mut resources = Vec::new();
    let source = collect_shared_resource(
        targets,
        &mut resources,
        SkillResourceKind::Source,
        source_root,
        "shared Skill source",
    );
    let unified = collect_shared_resource(
        targets,
        &mut resources,
        SkillResourceKind::Unified,
        unified_discovery_root,
        "unified Skill directory",
    );
    if source
        .as_ref()
        .zip(unified.as_ref())
        .is_some_and(|(source, unified)| source == unified)
    {
        resources.retain(|resource| resource.kind != SkillResourceKind::Unified);
    }

    for index in 0..targets.len() {
        let skills_root = targets[index].skills_root.clone();
        let config_path = targets[index].config_path.clone();
        collect_target_resource(
            targets,
            &mut resources,
            SkillResourceKind::Native(index),
            index,
            skills_root,
            "native Skill directory",
        );
        if let Some(path) = config_path {
            collect_target_resource(
                targets,
                &mut resources,
                SkillResourceKind::Config(index),
                index,
                path,
                "native Skill configuration",
            );
        }
    }

    for left in 0..resources.len() {
        for right in (left + 1)..resources.len() {
            let left = &resources[left];
            let right = &resources[right];
            if !paths_overlap(&left.path, &right.path) {
                continue;
            }
            if same_target_resources(left.kind, right.kind) {
                let index = left.kind.owner().expect("target resource has an owner");
                append_target_issue(
                    &mut targets[index],
                    "native Skill configuration overlaps its Skill directory".to_owned(),
                );
                continue;
            }
            for (current, other) in [(left.kind, right.kind), (right.kind, left.kind)] {
                if let Some((index, issue)) = resource_overlap_issue(current, other, targets) {
                    append_target_issue(&mut targets[index], issue);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillResourceKind {
    Source,
    Unified,
    Native(usize),
    Config(usize),
}

impl SkillResourceKind {
    const fn owner(self) -> Option<usize> {
        match self {
            Self::Native(index) | Self::Config(index) => Some(index),
            Self::Source | Self::Unified => None,
        }
    }
}

struct SkillResource {
    kind: SkillResourceKind,
    path: PathBuf,
}

fn collect_shared_resource(
    targets: &mut [SkillTarget],
    resources: &mut Vec<SkillResource>,
    kind: SkillResourceKind,
    path: &Path,
    label: &str,
) -> Option<PathBuf> {
    match skill_path_identity(path) {
        Ok(path) => {
            resources.push(SkillResource {
                kind,
                path: path.clone(),
            });
            Some(path)
        }
        Err(error) => {
            for target in targets {
                append_target_issue(target, format!("{label} is unavailable: {error}"));
            }
            None
        }
    }
}

fn collect_target_resource(
    targets: &mut [SkillTarget],
    resources: &mut Vec<SkillResource>,
    kind: SkillResourceKind,
    index: usize,
    path: PathBuf,
    label: &str,
) {
    match skill_path_identity(&path) {
        Ok(path) => resources.push(SkillResource { kind, path }),
        Err(error) => append_target_issue(
            &mut targets[index],
            format!("{label} is unavailable: {error}"),
        ),
    }
}

fn same_target_resources(left: SkillResourceKind, right: SkillResourceKind) -> bool {
    matches!(
        (left, right),
        (SkillResourceKind::Native(left), SkillResourceKind::Config(right))
            | (SkillResourceKind::Config(left), SkillResourceKind::Native(right))
            if left == right
    )
}

fn resource_overlap_issue(
    current: SkillResourceKind,
    other: SkillResourceKind,
    targets: &[SkillTarget],
) -> Option<(usize, String)> {
    let index = current.owner()?;
    let current_label = match current {
        SkillResourceKind::Native(_) => "native Skill directory",
        SkillResourceKind::Config(_) => "native Skill configuration",
        SkillResourceKind::Source | SkillResourceKind::Unified => unreachable!(),
    };
    let other_label = match other {
        SkillResourceKind::Source => "the shared Skill source".to_owned(),
        SkillResourceKind::Unified => "the unified Skill directory".to_owned(),
        SkillResourceKind::Native(other) => {
            format!("application '{}'", targets[other].app.as_str())
        }
        SkillResourceKind::Config(other) => {
            format!(
                "application '{}' configuration",
                targets[other].app.as_str()
            )
        }
    };
    Some((index, format!("{current_label} overlaps {other_label}")))
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn hash_fingerprint_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

fn hash_fingerprint_path(hasher: &mut Sha256, path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hash_fingerprint_field(hasher, path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let bytes = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        hash_fingerprint_field(hasher, &bytes);
    }
}

fn append_target_issue(target: &mut SkillTarget, issue: String) {
    target.issue = Some(match target.issue.take() {
        Some(existing) => format!("{existing}; {issue}"),
        None => issue,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn skill_request() -> Vec<SkillObservationRequest> {
        vec![SkillObservationRequest {
            id: "skill".to_owned(),
            name: "Docs".to_owned(),
            directory: "docs".to_owned(),
            selected_apps: HashMap::new(),
        }]
    }

    fn enabled_configs() -> SkillConfigDocuments {
        HashMap::from([
            (SkillConfigTarget::GeminiSettings, Ok(None)),
            (SkillConfigTarget::GrokConfig, Ok(None)),
            (SkillConfigTarget::HermesConfig, Ok(None)),
        ])
    }

    #[test]
    fn native_uncertainty_is_not_reported_as_disabled() {
        let contract = *builtin_app_registry()
            .for_app(&AppType::Gemini)
            .skill_contract()
            .unwrap();
        let native = SkillAppState {
            enabled: None,
            issue: Some("native state unavailable".to_owned()),
        };
        let enabled = apply_effective_state(
            "Gemini",
            contract,
            SkillDiscoveryState::Missing,
            native,
            Some(
                observe_config_state(
                    SkillConfigTarget::GeminiSettings,
                    "Docs",
                    &enabled_configs(),
                )
                .unwrap(),
            ),
        );
        assert_eq!(enabled.enabled, None);

        let mut disabled_configs = enabled_configs();
        disabled_configs.insert(
            SkillConfigTarget::GeminiSettings,
            Ok(Some(br#"{"skills":{"disabled":["Docs"]}}"#.to_vec())),
        );
        let disabled = apply_effective_state(
            "Gemini",
            contract,
            SkillDiscoveryState::Missing,
            SkillAppState {
                enabled: None,
                issue: None,
            },
            Some(
                observe_config_state(SkillConfigTarget::GeminiSettings, "Docs", &disabled_configs)
                    .unwrap(),
            ),
        );
        assert_eq!(disabled.enabled, Some(false));
    }

    fn app_resources(base: &Path) -> Vec<SkillAppResources> {
        builtin_app_registry()
            .descriptors()
            .filter_map(|descriptor| {
                let contract = descriptor.skill_contract()?;
                let install_root = base.join(descriptor.id());
                let config_path = contract.config_target().map(|target| {
                    install_root.join(match target {
                        SkillConfigTarget::GeminiSettings => "settings.json",
                        SkillConfigTarget::GrokConfig => "config.toml",
                        SkillConfigTarget::HermesConfig => "config.yaml",
                    })
                });
                Some(SkillAppResources::new(
                    descriptor.app().clone(),
                    install_root,
                    config_path,
                ))
            })
            .collect()
    }

    #[test]
    fn enabling_requires_an_existing_application_root() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        let live = SkillLiveConfig::new(
            source,
            directory.path().join("unified"),
            SkillSyncMethod::Copy,
            app_resources(directory.path()),
        )
        .unwrap();

        assert!(matches!(
            live.apply_deployment("docs", &AppType::Claude, true, SkillCopyPolicy::ManagedOnly),
            Err(SkillLiveError::AppUnavailable(_))
        ));
        assert!(!directory.path().join("claude").exists());
    }

    #[test]
    fn native_presence_is_observed_from_the_core_contract() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let pi = directory.path().join("pi");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(&pi).unwrap();
        let mut roots = app_resources(directory.path());
        roots
            .iter_mut()
            .find(|resources| resources.app == AppType::Pi)
            .unwrap()
            .install_root = pi.clone();
        let live = SkillLiveConfig::new(
            source,
            directory.path().join("unified"),
            SkillSyncMethod::Copy,
            roots,
        )
        .unwrap();
        live.apply_deployment("docs", &AppType::Pi, true, SkillCopyPolicy::ManagedOnly)
            .unwrap()
            .commit()
            .unwrap();

        let observations = live.observe(&skill_request(), &enabled_configs());
        assert_eq!(observations[0].app_overrides["pi"].enabled, Some(true));
        assert_eq!(observations[0].app_overrides["pi"].issue, None);
        assert_eq!(observations[0].app_overrides["codex"].enabled, None);
        assert!(observations[0].app_overrides["codex"].issue.is_some());

        fs::write(pi.join("skills/docs/extra"), "external change").unwrap();
        let observations = live.observe(&skill_request(), &enabled_configs());
        assert_eq!(observations[0].app_overrides["pi"].enabled, None);
        assert!(observations[0].app_overrides["pi"].issue.is_some());
    }

    #[test]
    fn external_discovery_uses_declared_control_without_guessing_state() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".cc-switch/skills");
        let unified = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        apply_skill_deployment(&source, &unified, "docs", true, SkillSyncMethod::Copy)
            .unwrap()
            .commit()
            .unwrap();
        let roots = app_resources(directory.path());
        for resources in &roots {
            fs::create_dir_all(&resources.install_root).unwrap();
        }
        let live = SkillLiveConfig::new(source, unified, SkillSyncMethod::Copy, roots).unwrap();

        let configs = HashMap::from([
            (
                SkillConfigTarget::GeminiSettings,
                Ok(Some(br#"{"skills":{"disabled":["Docs"]}}"#.to_vec())),
            ),
            (SkillConfigTarget::GrokConfig, Ok(None)),
            (SkillConfigTarget::HermesConfig, Ok(None)),
        ]);
        let observations = live.observe(&skill_request(), &configs);
        for descriptor in builtin_app_registry().descriptors().filter(|descriptor| {
            descriptor.skill_contract().is_some_and(|contract| {
                contract.discovery() == SkillDiscoveryMode::Unified
                    && contract.config_target().is_none()
            })
        }) {
            let state = &observations[0].app_overrides[descriptor.id()];
            assert_eq!(state.enabled, None, "{}", descriptor.id());
            assert!(
                state
                    .issue
                    .as_deref()
                    .unwrap()
                    .contains("managed outside Lite"),
                "{}",
                descriptor.id()
            );
            assert!(matches!(
                live.route("docs", descriptor.app(), SkillCopyPolicy::ManagedOnly),
                Err(SkillLiveError::UnifiedDiscovery(_))
            ));
        }
        assert_eq!(observations[0].app_overrides["gemini"].enabled, Some(false));
        assert_eq!(
            observations[0].app_overrides["grokbuild"].enabled,
            Some(true)
        );
        for app in [AppType::Gemini, AppType::GrokBuild] {
            let route = live
                .route("docs", &app, SkillCopyPolicy::ManagedOnly)
                .unwrap();
            assert!(!route.deploy_native());
            assert!(route.config_target().is_some());
        }
    }

    #[test]
    fn gemini_global_disable_is_reported_as_read_only() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".cc-switch/skills");
        let unified = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        apply_skill_deployment(&source, &unified, "docs", true, SkillSyncMethod::Copy)
            .unwrap()
            .commit()
            .unwrap();
        let roots = app_resources(directory.path());
        for resources in &roots {
            fs::create_dir_all(&resources.install_root).unwrap();
        }
        let live = SkillLiveConfig::new(source, unified, SkillSyncMethod::Copy, roots).unwrap();
        let configs = HashMap::from([
            (
                SkillConfigTarget::GeminiSettings,
                Ok(Some(br#"{"skills":{"enabled":false}}"#.to_vec())),
            ),
            (SkillConfigTarget::GrokConfig, Ok(None)),
            (SkillConfigTarget::HermesConfig, Ok(None)),
        ]);

        let observations = live.observe(&skill_request(), &configs);
        let state = &observations[0].app_overrides["gemini"];
        assert_eq!(state.enabled, Some(false));
        assert!(state
            .issue
            .as_deref()
            .is_some_and(|issue| issue.contains("disabled globally")));
    }

    #[test]
    fn hermes_native_disabled_state_is_observed_and_platform_controls_are_read_only() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".cc-switch/skills");
        let unified = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        let roots = app_resources(directory.path());
        for resources in &roots {
            fs::create_dir_all(&resources.install_root).unwrap();
        }
        let hermes_root = roots
            .iter()
            .find(|resources| resources.app == AppType::Hermes)
            .unwrap()
            .install_root
            .clone();
        apply_skill_deployment(
            &source,
            &hermes_root.join("skills"),
            "docs",
            true,
            SkillSyncMethod::Copy,
        )
        .unwrap()
        .commit()
        .unwrap();
        let live = SkillLiveConfig::new(source, unified, SkillSyncMethod::Copy, roots).unwrap();
        let mut configs = enabled_configs();
        configs.insert(
            SkillConfigTarget::HermesConfig,
            Ok(Some(b"skills:\n  disabled: [Docs]\n".to_vec())),
        );
        let observations = live.observe(&skill_request(), &configs);
        assert_eq!(observations[0].app_overrides["hermes"].enabled, Some(false));
        assert!(observations[0].app_overrides["hermes"].issue.is_none());

        configs.insert(
            SkillConfigTarget::HermesConfig,
            Ok(Some(
                b"skills:\n  platform_disabled:\n    telegram: [Docs]\n".to_vec(),
            )),
        );
        let observations = live.observe(&skill_request(), &configs);
        let state = &observations[0].app_overrides["hermes"];
        assert_eq!(state.enabled, None);
        assert!(state
            .issue
            .as_deref()
            .is_some_and(|issue| issue.contains("platform-specific")));
    }

    #[test]
    fn duplicate_native_names_are_reported_per_application() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".cc-switch/skills");
        for name in ["docs", "other"] {
            fs::create_dir_all(source.join(name)).unwrap();
            fs::write(source.join(name).join("SKILL.md"), "# Skill").unwrap();
        }
        let roots = app_resources(directory.path());
        for resources in &roots {
            fs::create_dir_all(&resources.install_root).unwrap();
        }
        let live = SkillLiveConfig::new(
            source,
            directory.path().join(".agents/skills"),
            SkillSyncMethod::Copy,
            roots,
        )
        .unwrap();
        let skills = vec![
            SkillObservationRequest {
                id: "one".to_owned(),
                name: "Docs".to_owned(),
                directory: "docs".to_owned(),
                selected_apps: HashMap::new(),
            },
            SkillObservationRequest {
                id: "two".to_owned(),
                name: "Ｄocs".to_owned(),
                directory: "other".to_owned(),
                selected_apps: HashMap::new(),
            },
        ];

        let observations = live.observe(&skills, &enabled_configs());
        for observation in observations {
            for app in ["gemini", "grokbuild", "hermes"] {
                assert!(observation.app_overrides[app]
                    .issue
                    .as_deref()
                    .is_some_and(|issue| issue.contains("native name")));
            }
            assert!(observation.app_overrides["claude"].issue.is_none());
        }
    }

    #[test]
    fn unmanaged_same_directory_skill_is_unknown_and_read_only() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let claude = directory.path().join("claude");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(claude.join("skills/docs")).unwrap();
        fs::write(claude.join("skills/docs/SKILL.md"), "# Docs").unwrap();
        let mut roots = app_resources(directory.path());
        roots
            .iter_mut()
            .find(|resources| resources.app == AppType::Claude)
            .unwrap()
            .install_root = claude;
        let live = SkillLiveConfig::new(
            source,
            directory.path().join("unified"),
            SkillSyncMethod::Copy,
            roots,
        )
        .unwrap();

        let observations = live.observe(&skill_request(), &enabled_configs());
        let state = &observations[0].app_overrides["claude"];
        assert_eq!(state.enabled, None);
        assert!(state.issue.is_some());
    }

    #[test]
    fn selected_legacy_copy_can_be_observed_and_disabled() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let claude = directory.path().join("claude");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(claude.join("skills/docs")).unwrap();
        fs::write(claude.join("skills/docs/SKILL.md"), "# Docs").unwrap();
        let mut resources = app_resources(directory.path());
        resources
            .iter_mut()
            .find(|resources| resources.app == AppType::Claude)
            .unwrap()
            .install_root = claude.clone();
        let live = SkillLiveConfig::new(
            source.clone(),
            directory.path().join("unified"),
            SkillSyncMethod::Copy,
            resources,
        )
        .unwrap();
        let mut request = skill_request();
        request[0].selected_apps.insert(AppType::Claude, true);

        let observations = live.observe(&request, &enabled_configs());
        assert_eq!(observations[0].app_overrides["claude"].enabled, Some(true));
        live.apply_deployment(
            "docs",
            &AppType::Claude,
            false,
            SkillCopyPolicy::AllowMatching,
        )
        .unwrap()
        .commit()
        .unwrap();
        assert!(!claude.join("skills/docs").exists());
        assert!(source.join("docs/SKILL.md").is_file());
    }

    #[test]
    fn invalid_directory_never_falls_back_to_requested_catalog_state() {
        let directory = tempdir().unwrap();
        let live = SkillLiveConfig::new(
            directory.path().join("source"),
            directory.path().join("unified"),
            SkillSyncMethod::Copy,
            app_resources(directory.path()),
        )
        .unwrap();
        let skills = vec![SkillObservationRequest {
            id: "skill".to_owned(),
            name: "Docs".to_owned(),
            directory: "../docs".to_owned(),
            selected_apps: HashMap::new(),
        }];

        let observations = live.observe(&skills, &enabled_configs());
        assert!(observations[0].source_issue.is_some());
        assert!(observations[0]
            .app_overrides
            .values()
            .all(|state| state.enabled.is_none()));
    }

    #[test]
    fn overlapping_application_roots_are_isolated_to_the_affected_apps() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        let shared_app_root = directory.path().join("shared-app");
        fs::create_dir_all(&shared_app_root).unwrap();
        let mut roots = app_resources(directory.path());
        for resources in &mut roots {
            if matches!(resources.app, AppType::Claude | AppType::Codex) {
                resources.install_root = shared_app_root.clone();
            } else {
                fs::create_dir_all(&resources.install_root).unwrap();
            }
        }
        let live = SkillLiveConfig::new(
            source,
            directory.path().join("unified"),
            SkillSyncMethod::Copy,
            roots,
        )
        .unwrap();

        let observations = live.observe(&skill_request(), &enabled_configs());
        for app in ["claude", "codex"] {
            let state = &observations[0].app_overrides[app];
            assert_eq!(state.enabled, None);
            assert!(state
                .issue
                .as_deref()
                .is_some_and(|issue| issue.contains("overlaps")));
        }
        assert!(matches!(
            live.route("docs", &AppType::Claude, SkillCopyPolicy::ManagedOnly),
            Err(SkillLiveError::InvalidTargets(_))
        ));
        assert!(observations[0].app_overrides["hermes"].issue.is_none());
    }

    #[test]
    fn application_root_overlapping_unified_discovery_is_read_only() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let unified = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(&unified).unwrap();
        let mut roots = app_resources(directory.path());
        roots
            .iter_mut()
            .find(|resources| resources.app == AppType::Codex)
            .unwrap()
            .install_root = directory.path().join(".agents");
        for resources in &roots {
            fs::create_dir_all(&resources.install_root).unwrap();
        }
        let live = SkillLiveConfig::new(source, unified, SkillSyncMethod::Copy, roots).unwrap();

        let observations = live.observe(&skill_request(), &enabled_configs());
        let codex = &observations[0].app_overrides["codex"];
        assert_eq!(codex.enabled, None);
        assert!(codex
            .issue
            .as_deref()
            .is_some_and(|issue| issue.contains("unified Skill directory")));
        assert!(matches!(
            live.route("docs", &AppType::Codex, SkillCopyPolicy::ManagedOnly),
            Err(SkillLiveError::InvalidTargets(_))
        ));
    }

    #[test]
    fn native_configuration_overlapping_skill_source_is_read_only() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        let mut resources = app_resources(directory.path());
        let gemini = resources
            .iter_mut()
            .find(|resources| resources.app == AppType::Gemini)
            .unwrap();
        gemini.install_root = source.clone();
        gemini.config_path = Some(source.join("settings.json"));
        for resources in &resources {
            fs::create_dir_all(&resources.install_root).unwrap();
        }
        let live = SkillLiveConfig::new(
            source,
            directory.path().join("unified"),
            SkillSyncMethod::Copy,
            resources,
        )
        .unwrap();

        let observations = live.observe(&skill_request(), &enabled_configs());
        let gemini = &observations[0].app_overrides["gemini"];
        assert_eq!(gemini.enabled, None);
        assert!(gemini
            .issue
            .as_deref()
            .is_some_and(|issue| issue.contains("shared Skill source")));
        assert!(matches!(
            live.route("docs", &AppType::Gemini, SkillCopyPolicy::ManagedOnly),
            Err(SkillLiveError::InvalidTargets(_))
        ));
    }

    #[test]
    fn unrelated_unified_skill_is_never_toggled() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".cc-switch/skills");
        let unified = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(unified.join("docs")).unwrap();
        fs::write(unified.join("docs/SKILL.md"), "# Other").unwrap();
        let roots = app_resources(directory.path());
        for resources in &roots {
            fs::create_dir_all(&resources.install_root).unwrap();
        }
        let live = SkillLiveConfig::new(source, unified, SkillSyncMethod::Copy, roots).unwrap();

        let observations = live.observe(&skill_request(), &enabled_configs());
        for descriptor in builtin_app_registry().descriptors().filter(|descriptor| {
            descriptor
                .skill_contract()
                .is_some_and(|contract| contract.discovery() == SkillDiscoveryMode::Unified)
        }) {
            let state = &observations[0].app_overrides[descriptor.id()];
            assert_eq!(state.enabled, None, "{}", descriptor.id());
            assert!(state.issue.is_some(), "{}", descriptor.id());
            assert!(matches!(
                live.route("docs", descriptor.app(), SkillCopyPolicy::ManagedOnly),
                Err(SkillLiveError::UnifiedConflict(_))
            ));
        }
    }

    #[test]
    fn every_core_skill_application_requires_exactly_one_target() {
        assert!(matches!(
            SkillLiveConfig::new(
                PathBuf::from("/source"),
                PathBuf::from("/unified"),
                SkillSyncMethod::Copy,
                Vec::new()
            ),
            Err(SkillLiveError::InvalidTargets(_))
        ));
    }
}
