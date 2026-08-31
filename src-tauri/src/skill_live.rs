use std::{collections::BTreeMap, fs, path::PathBuf};

use cc_switch_core::{
    apply_skill_deployment, builtin_app_registry, inspect_skill_deployment, inspect_skill_presence,
    validate_skill_directory, validate_skill_source, AppType, SkillActivationSource,
    SkillConfigError, SkillDeploymentReceipt, SkillDeploymentState, SkillSyncMethod,
};
use thiserror::Error;

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
        "{0} discovers the unified Skill store directly; its per-application switch is read-only"
    )]
    UnifiedDiscovery(String),
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
    pub(crate) app_overrides: BTreeMap<String, SkillAppObservation>,
}

pub(crate) struct SkillAppObservation {
    pub(crate) enabled: Option<bool>,
    pub(crate) issue: Option<String>,
}

struct SkillTarget {
    app: AppType,
    install_root: PathBuf,
    skills_root: PathBuf,
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
        app_roots: Vec<(AppType, PathBuf)>,
    ) -> Result<Self, SkillLiveError> {
        validate_targets(&app_roots)?;
        Ok(Self {
            source_root,
            unified_discovery_root,
            sync_method,
            targets: app_roots
                .into_iter()
                .map(|(app, install_root)| SkillTarget {
                    skills_root: install_root.join("skills"),
                    app,
                    install_root,
                })
                .collect(),
        })
    }

    pub(crate) fn observe(&self, skills: &[(String, String)]) -> Vec<SkillObservation> {
        skills
            .iter()
            .map(|(id, directory)| {
                let directory_issue = validate_skill_directory(directory)
                    .err()
                    .map(|error| error.to_string());
                let source_issue = directory_issue.clone().or_else(|| {
                    validate_skill_source(&self.source_root.join(directory))
                        .err()
                        .map(|error| error.to_string())
                });
                let app_overrides = if directory_issue.is_some() {
                    BTreeMap::new()
                } else {
                    self.observe_app_overrides(directory, source_issue.is_none())
                };
                SkillObservation {
                    id: id.clone(),
                    source_issue,
                    app_overrides,
                }
            })
            .collect()
    }

    pub(crate) fn apply(
        &self,
        directory: &str,
        app: &AppType,
        enabled: bool,
    ) -> Result<SkillDeploymentReceipt, SkillLiveError> {
        let descriptor = builtin_app_registry().for_app(app);
        let Some(contract) = descriptor.skill_contract() else {
            return Err(SkillLiveError::Unsupported(app.as_str().to_owned()));
        };
        if contract.discovers_unified_store()
            && inspect_skill_presence(&self.unified_discovery_root, directory)?
        {
            return Err(SkillLiveError::UnifiedDiscovery(
                descriptor.display_name().to_owned(),
            ));
        }
        let target = self
            .target(app)
            .ok_or_else(|| SkillLiveError::MissingTarget(app.as_str().to_owned()))?;
        if enabled {
            match fs::metadata(&target.install_root) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => {
                    return Err(SkillLiveError::InvalidAppRoot(
                        descriptor.display_name().to_owned(),
                        target.install_root.clone(),
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SkillLiveError::AppUnavailable(
                        descriptor.display_name().to_owned(),
                    ))
                }
                Err(source) => {
                    return Err(SkillLiveError::Io {
                        path: target.install_root.clone(),
                        source,
                    })
                }
            }
        }
        apply_skill_deployment(
            &self.source_root,
            &target.skills_root,
            directory,
            enabled,
            self.sync_method,
        )
        .map_err(Into::into)
    }

    fn observe_app_overrides(
        &self,
        directory: &str,
        source_valid: bool,
    ) -> BTreeMap<String, SkillAppObservation> {
        builtin_app_registry()
            .descriptors()
            .filter_map(|descriptor| {
                let contract = descriptor.skill_contract()?;
                (contract.activation_source() == SkillActivationSource::NativePresence
                    || contract.discovers_unified_store())
                .then_some((descriptor, contract))
            })
            .filter_map(|(descriptor, contract)| {
                let native_observation = || {
                    self.target(descriptor.app())
                        .map(|target| {
                            observe_managed_presence(
                                &self.source_root,
                                &target.skills_root,
                                directory,
                                source_valid,
                            )
                        })
                        .unwrap_or_else(|| SkillAppObservation {
                            enabled: Some(false),
                            issue: Some(
                                SkillLiveError::MissingTarget(descriptor.id().to_owned())
                                    .to_string(),
                            ),
                        })
                };
                let observation = if contract.discovers_unified_store() {
                    match inspect_skill_presence(&self.unified_discovery_root, directory) {
                        Ok(true) => SkillAppObservation {
                            enabled: Some(true),
                            issue: Some(
                                SkillLiveError::UnifiedDiscovery(
                                    descriptor.display_name().to_owned(),
                                )
                                .to_string(),
                            ),
                        },
                        Ok(false)
                            if contract.activation_source()
                                == SkillActivationSource::NativePresence =>
                        {
                            native_observation()
                        }
                        Ok(false) => return None,
                        Err(error) => {
                            let issue = error.to_string();
                            if contract.activation_source() == SkillActivationSource::NativePresence
                            {
                                append_issue(native_observation(), issue)
                            } else {
                                SkillAppObservation {
                                    enabled: None,
                                    issue: Some(issue),
                                }
                            }
                        }
                    }
                } else {
                    native_observation()
                };
                Some((descriptor.id().to_owned(), observation))
            })
            .collect()
    }

    fn target(&self, app: &AppType) -> Option<&SkillTarget> {
        self.targets.iter().find(|target| &target.app == app)
    }
}

fn observe_managed_presence(
    source_root: &std::path::Path,
    destination_root: &std::path::Path,
    directory: &str,
    source_valid: bool,
) -> SkillAppObservation {
    match inspect_skill_presence(destination_root, directory) {
        Ok(false) => SkillAppObservation {
            enabled: Some(false),
            issue: None,
        },
        Ok(true) => {
            let issue = if source_valid {
                match inspect_skill_deployment(source_root, destination_root, directory) {
                    Ok(SkillDeploymentState::Missing) => {
                        Some("native Skill changed while it was inspected".to_owned())
                    }
                    Ok(SkillDeploymentState::Linked | SkillDeploymentState::Copied) => None,
                    Err(error) => Some(error.to_string()),
                }
            } else {
                None
            };
            SkillAppObservation {
                enabled: Some(true),
                issue,
            }
        }
        Err(error) => SkillAppObservation {
            enabled: Some(false),
            issue: Some(error.to_string()),
        },
    }
}

fn append_issue(mut observation: SkillAppObservation, issue: String) -> SkillAppObservation {
    observation.issue = Some(match observation.issue {
        Some(existing) => format!("{issue}; {existing}"),
        None => issue,
    });
    observation
}

fn validate_targets(app_roots: &[(AppType, PathBuf)]) -> Result<(), SkillLiveError> {
    for descriptor in builtin_app_registry().descriptors() {
        let expected = usize::from(descriptor.skill_contract().is_some());
        let actual = app_roots
            .iter()
            .filter(|(app, _)| app == descriptor.app())
            .count();
        if actual != expected {
            return Err(SkillLiveError::InvalidTargets(format!(
                "application '{}' requires {expected} target(s), found {actual}",
                descriptor.id()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn app_roots(base: &std::path::Path) -> Vec<(AppType, PathBuf)> {
        builtin_app_registry()
            .descriptors()
            .filter(|descriptor| descriptor.skill_contract().is_some())
            .map(|descriptor| (descriptor.app().clone(), base.join(descriptor.id())))
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
            app_roots(directory.path()),
        )
        .unwrap();

        assert!(matches!(
            live.apply("docs", &AppType::Claude, true),
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
        let mut roots = app_roots(directory.path());
        roots
            .iter_mut()
            .find(|(app, _)| app == &AppType::Pi)
            .unwrap()
            .1 = pi.clone();
        let live = SkillLiveConfig::new(
            source,
            directory.path().join("unified"),
            SkillSyncMethod::Copy,
            roots,
        )
        .unwrap();
        live.apply("docs", &AppType::Pi, true)
            .unwrap()
            .commit()
            .unwrap();

        let observations = live.observe(&[("skill".to_owned(), "docs".to_owned())]);
        assert_eq!(observations[0].app_overrides["pi"].enabled, Some(true));
        assert_eq!(observations[0].app_overrides["pi"].issue, None);
        assert!(!observations[0].app_overrides.contains_key("codex"));

        fs::write(pi.join("skills/docs/extra"), "external change").unwrap();
        let observations = live.observe(&[("skill".to_owned(), "docs".to_owned())]);
        assert_eq!(observations[0].app_overrides["pi"].enabled, Some(true));
        assert!(observations[0].app_overrides["pi"].issue.is_some());
    }

    #[test]
    fn external_unified_discovery_is_active_and_read_only_in_default_mode() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".cc-switch/skills");
        let unified = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(unified.join("docs")).unwrap();
        fs::write(unified.join("docs/SKILL.md"), "# External Docs").unwrap();
        let live = SkillLiveConfig::new(
            source,
            unified,
            SkillSyncMethod::Copy,
            app_roots(directory.path()),
        )
        .unwrap();

        let observations = live.observe(&[("skill".to_owned(), "docs".to_owned())]);
        for descriptor in builtin_app_registry().descriptors().filter(|descriptor| {
            descriptor
                .skill_contract()
                .is_some_and(|contract| contract.discovers_unified_store())
        }) {
            let state = &observations[0].app_overrides[descriptor.id()];
            assert_eq!(state.enabled, Some(true), "{}", descriptor.id());
            assert!(
                state.issue.as_deref().unwrap().contains("read-only"),
                "{}",
                descriptor.id()
            );
            assert!(matches!(
                live.apply("docs", descriptor.app(), false),
                Err(SkillLiveError::UnifiedDiscovery(_))
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
