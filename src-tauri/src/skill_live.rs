use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
};

use cc_switch_core::{
    apply_skill_deployment, builtin_app_registry, inspect_skill_config_state,
    inspect_skill_deployment, inspect_skill_discovery, inspect_skill_presence,
    validate_skill_directory, validate_skill_source, AppType, SkillConfigError, SkillConfigState,
    SkillConfigTarget, SkillDeploymentReceipt, SkillDeploymentState, SkillDiscoveryState,
    SkillSyncMethod, UnifiedSkillControl,
};
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

pub(crate) type SkillConfigDocuments = HashMap<SkillConfigTarget, Result<Option<Vec<u8>>, String>>;

pub(crate) struct SkillApplyRoute {
    pub(crate) config_target: Option<SkillConfigTarget>,
    pub(crate) deploy_native: bool,
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

    pub(crate) fn observe(
        &self,
        skills: &[(String, String, String)],
        configs: &SkillConfigDocuments,
    ) -> Vec<SkillObservation> {
        skills
            .iter()
            .map(|(id, name, directory)| {
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
                    self.observe_app_overrides(name, directory, source_issue.is_none(), configs)
                };
                SkillObservation {
                    id: id.clone(),
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
    ) -> Result<SkillApplyRoute, SkillLiveError> {
        let descriptor = builtin_app_registry().for_app(app);
        let Some(contract) = descriptor.skill_contract() else {
            return Err(SkillLiveError::Unsupported(app.as_str().to_owned()));
        };
        let discovery = match contract.unified_control() {
            Some(_) => {
                inspect_skill_discovery(&self.source_root, &self.unified_discovery_root, directory)?
            }
            None => SkillDiscoveryState::Missing,
        };
        if discovery == SkillDiscoveryState::External {
            return Err(SkillLiveError::UnifiedConflict(
                descriptor.display_name().to_owned(),
            ));
        }
        let config_target = match (contract.unified_control(), discovery) {
            (Some(UnifiedSkillControl::ReadOnly), SkillDiscoveryState::Selected) => {
                return Err(SkillLiveError::UnifiedDiscovery(
                    descriptor.display_name().to_owned(),
                ))
            }
            (Some(UnifiedSkillControl::DisabledNameList(target)), _) => Some(target),
            (Some(UnifiedSkillControl::ReadOnly) | None, _) => None,
        };
        if config_target.is_some() {
            let target = self
                .target(app)
                .ok_or_else(|| SkillLiveError::MissingTarget(app.as_str().to_owned()))?;
            require_app_root(descriptor.display_name(), target)?;
        }
        Ok(SkillApplyRoute {
            config_target,
            deploy_native: discovery == SkillDiscoveryState::Missing,
        })
    }

    pub(crate) fn apply_deployment(
        &self,
        directory: &str,
        app: &AppType,
        enabled: bool,
    ) -> Result<SkillDeploymentReceipt, SkillLiveError> {
        let descriptor = builtin_app_registry().for_app(app);
        if descriptor.skill_contract().is_none() {
            return Err(SkillLiveError::Unsupported(app.as_str().to_owned()));
        }
        let target = self
            .target(app)
            .ok_or_else(|| SkillLiveError::MissingTarget(app.as_str().to_owned()))?;
        if enabled {
            require_app_root(descriptor.display_name(), target)?;
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
        name: &str,
        directory: &str,
        source_valid: bool,
        configs: &SkillConfigDocuments,
    ) -> BTreeMap<String, SkillAppState> {
        let unified_discovery =
            inspect_skill_discovery(&self.source_root, &self.unified_discovery_root, directory)
                .map_err(|error| error.to_string());
        builtin_app_registry()
            .descriptors()
            .filter_map(|descriptor| {
                let contract = descriptor.skill_contract()?;
                Some((descriptor, contract))
            })
            .map(|(descriptor, contract)| {
                let native_observation = || {
                    self.target(descriptor.app())
                        .map(
                            |target| match require_app_root(descriptor.display_name(), target) {
                                Ok(()) => observe_managed_presence(
                                    &self.source_root,
                                    &target.skills_root,
                                    directory,
                                    source_valid,
                                ),
                                Err(error) => SkillAppState {
                                    enabled: None,
                                    issue: Some(error.to_string()),
                                },
                            },
                        )
                        .unwrap_or_else(|| SkillAppState {
                            enabled: Some(false),
                            issue: Some(
                                SkillLiveError::MissingTarget(descriptor.id().to_owned())
                                    .to_string(),
                            ),
                        })
                };
                let observation = match (contract.unified_control(), &unified_discovery) {
                    (Some(_), Ok(SkillDiscoveryState::External)) => SkillAppState {
                        enabled: None,
                        issue: Some(
                            SkillLiveError::UnifiedConflict(descriptor.display_name().to_owned())
                                .to_string(),
                        ),
                    },
                    (Some(UnifiedSkillControl::ReadOnly), Ok(SkillDiscoveryState::Selected)) => {
                        SkillAppState {
                            enabled: None,
                            issue: Some(
                                SkillLiveError::UnifiedDiscovery(
                                    descriptor.display_name().to_owned(),
                                )
                                .to_string(),
                            ),
                        }
                    }
                    (Some(UnifiedSkillControl::DisabledNameList(target)), Ok(discovery)) => {
                        observe_configured_state(
                            native_observation(),
                            target,
                            name,
                            *discovery == SkillDiscoveryState::Selected,
                            configs,
                        )
                    }
                    (Some(_), Err(issue)) => SkillAppState {
                        enabled: None,
                        issue: Some(issue.clone()),
                    },
                    _ => native_observation(),
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

fn observe_managed_presence(
    source_root: &std::path::Path,
    destination_root: &std::path::Path,
    directory: &str,
    source_valid: bool,
) -> SkillAppState {
    match inspect_skill_presence(destination_root, directory) {
        Ok(false) => SkillAppState {
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
            SkillAppState {
                enabled: Some(true),
                issue,
            }
        }
        Err(error) => SkillAppState {
            enabled: Some(false),
            issue: Some(error.to_string()),
        },
    }
}

fn observe_configured_state(
    native: SkillAppState,
    target: SkillConfigTarget,
    name: &str,
    external: bool,
    configs: &SkillConfigDocuments,
) -> SkillAppState {
    let configured = configs
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
        });
    match configured {
        Ok(SkillConfigState::GloballyDisabled) => append_issue(
            SkillAppState {
                enabled: Some(false),
                issue: native.issue,
            },
            "Skills are disabled globally in the application's native settings".to_owned(),
        ),
        Ok(configured @ (SkillConfigState::Enabled | SkillConfigState::Disabled)) => {
            let configured = configured == SkillConfigState::Enabled;
            SkillAppState {
                enabled: Some(if external {
                    configured
                } else {
                    native.enabled.unwrap_or(false) && configured
                }),
                issue: native.issue,
            }
        }
        Err(issue) => append_issue(
            SkillAppState {
                enabled: None,
                issue: native.issue,
            },
            issue,
        ),
    }
}

fn append_issue(mut observation: SkillAppState, issue: String) -> SkillAppState {
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

    fn skill_request() -> Vec<(String, String, String)> {
        vec![("skill".to_owned(), "Docs".to_owned(), "docs".to_owned())]
    }

    fn enabled_configs() -> SkillConfigDocuments {
        HashMap::from([
            (SkillConfigTarget::GeminiSettings, Ok(None)),
            (SkillConfigTarget::GrokConfig, Ok(None)),
        ])
    }

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
            live.apply_deployment("docs", &AppType::Claude, true),
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
        live.apply_deployment("docs", &AppType::Pi, true)
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
        assert_eq!(observations[0].app_overrides["pi"].enabled, Some(true));
        assert!(observations[0].app_overrides["pi"].issue.is_some());
    }

    #[test]
    fn external_discovery_uses_declared_control_without_guessing_state() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".cc-switch/skills");
        let unified = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(unified.join("docs")).unwrap();
        fs::write(unified.join("docs/SKILL.md"), "# Docs").unwrap();
        let roots = app_roots(directory.path());
        for (_, root) in &roots {
            fs::create_dir_all(root).unwrap();
        }
        let live = SkillLiveConfig::new(source, unified, SkillSyncMethod::Copy, roots).unwrap();

        let configs = HashMap::from([
            (
                SkillConfigTarget::GeminiSettings,
                Ok(Some(br#"{"skills":{"disabled":["Docs"]}}"#.to_vec())),
            ),
            (SkillConfigTarget::GrokConfig, Ok(None)),
        ]);
        let observations = live.observe(&skill_request(), &configs);
        for descriptor in builtin_app_registry().descriptors().filter(|descriptor| {
            descriptor.skill_contract().is_some_and(|contract| {
                contract.unified_control() == Some(UnifiedSkillControl::ReadOnly)
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
                live.route("docs", descriptor.app()),
                Err(SkillLiveError::UnifiedDiscovery(_))
            ));
        }
        assert_eq!(observations[0].app_overrides["gemini"].enabled, Some(false));
        assert_eq!(
            observations[0].app_overrides["grokbuild"].enabled,
            Some(true)
        );
        for app in [AppType::Gemini, AppType::GrokBuild] {
            let route = live.route("docs", &app).unwrap();
            assert!(!route.deploy_native);
            assert!(route.config_target.is_some());
        }
    }

    #[test]
    fn gemini_global_disable_is_reported_as_read_only() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".cc-switch/skills");
        let unified = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(unified.join("docs")).unwrap();
        fs::write(unified.join("docs/SKILL.md"), "# Docs").unwrap();
        let roots = app_roots(directory.path());
        for (_, root) in &roots {
            fs::create_dir_all(root).unwrap();
        }
        let live = SkillLiveConfig::new(source, unified, SkillSyncMethod::Copy, roots).unwrap();
        let configs = HashMap::from([
            (
                SkillConfigTarget::GeminiSettings,
                Ok(Some(br#"{"skills":{"enabled":false}}"#.to_vec())),
            ),
            (SkillConfigTarget::GrokConfig, Ok(None)),
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
    fn unrelated_unified_skill_is_never_toggled() {
        let directory = tempdir().unwrap();
        let source = directory.path().join(".cc-switch/skills");
        let unified = directory.path().join(".agents/skills");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::write(source.join("docs/SKILL.md"), "# Docs").unwrap();
        fs::create_dir_all(unified.join("docs")).unwrap();
        fs::write(unified.join("docs/SKILL.md"), "# Other").unwrap();
        let roots = app_roots(directory.path());
        for (_, root) in &roots {
            fs::create_dir_all(root).unwrap();
        }
        let live = SkillLiveConfig::new(source, unified, SkillSyncMethod::Copy, roots).unwrap();

        let observations = live.observe(&skill_request(), &enabled_configs());
        for descriptor in builtin_app_registry().descriptors().filter(|descriptor| {
            descriptor
                .skill_contract()
                .is_some_and(|contract| contract.unified_control().is_some())
        }) {
            let state = &observations[0].app_overrides[descriptor.id()];
            assert_eq!(state.enabled, None, "{}", descriptor.id());
            assert!(state.issue.is_some(), "{}", descriptor.id());
            assert!(matches!(
                live.route("docs", descriptor.app()),
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
