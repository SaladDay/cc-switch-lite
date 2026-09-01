use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
};

use cc_switch_core::{
    apply_skill_deployment, builtin_app_registry, inspect_skill_config_state,
    inspect_skill_deployment, inspect_skill_discovery, inspect_skill_presence, skill_name_key,
    skill_path_identity, validate_skill_directory, validate_skill_source, AppType,
    SkillConfigError, SkillConfigState, SkillConfigTarget, SkillDeploymentReceipt,
    SkillDeploymentState, SkillDiscoveryMode, SkillDiscoveryState, SkillSyncMethod,
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
        app_roots: Vec<(AppType, PathBuf)>,
    ) -> Result<Self, SkillLiveError> {
        validate_targets(&app_roots)?;
        let mut targets = app_roots
            .into_iter()
            .map(|(app, install_root)| SkillTarget {
                skills_root: install_root.join("skills"),
                app,
                install_root,
                issue: None,
            })
            .collect::<Vec<_>>();
        mark_overlapping_targets(&mut targets);
        Ok(Self {
            source_root,
            unified_discovery_root,
            sync_method,
            targets,
        })
    }

    pub(crate) fn observe(
        &self,
        skills: &[(String, String, String)],
        configs: &SkillConfigDocuments,
    ) -> Vec<SkillObservation> {
        let mut name_counts = HashMap::new();
        for (_, name, _) in skills {
            if let Ok(key) = skill_name_key(name) {
                *name_counts.entry(key).or_insert(0_usize) += 1;
            }
        }
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
                    unknown_app_states()
                } else {
                    let ambiguous_name = skill_name_key(name)
                        .ok()
                        .is_some_and(|key| name_counts.get(&key).copied().unwrap_or_default() > 1);
                    self.observe_app_overrides(
                        name,
                        directory,
                        source_issue.is_none(),
                        ambiguous_name,
                        configs,
                    )
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
        let discovery = match contract.discovery() {
            SkillDiscoveryMode::Unified => {
                inspect_skill_discovery(&self.source_root, &self.unified_discovery_root, directory)?
            }
            SkillDiscoveryMode::Managed => SkillDiscoveryState::Missing,
        };
        if discovery == SkillDiscoveryState::External {
            return Err(SkillLiveError::UnifiedConflict(
                descriptor.display_name().to_owned(),
            ));
        }
        let config_target = match (contract.discovery(), contract.config_target(), discovery) {
            (SkillDiscoveryMode::Unified, None, SkillDiscoveryState::Selected) => {
                return Err(SkillLiveError::UnifiedDiscovery(
                    descriptor.display_name().to_owned(),
                ))
            }
            (_, target, _) => target,
        };
        let deploy_native = contract.discovery() == SkillDiscoveryMode::Managed
            || discovery == SkillDiscoveryState::Missing;
        if config_target.is_some() || deploy_native {
            let target = self
                .target(app)
                .ok_or_else(|| SkillLiveError::MissingTarget(app.as_str().to_owned()))?;
            require_safe_target(target)?;
            if config_target.is_some() {
                require_app_root(descriptor.display_name(), target)?;
            }
        }
        Ok(SkillApplyRoute {
            config_target,
            deploy_native,
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
        require_safe_target(target)?;
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
        ambiguous_name: bool,
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
                        .map(|target| {
                            match require_safe_target(target)
                                .and_then(|()| require_app_root(descriptor.display_name(), target))
                            {
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
                    unified_discovery.clone()
                } else {
                    Ok(SkillDiscoveryState::Missing)
                };
                let observation = match (contract.discovery(), contract.config_target(), discovery)
                {
                    (SkillDiscoveryMode::Unified, _, Ok(SkillDiscoveryState::External)) => {
                        SkillAppState {
                            enabled: None,
                            issue: Some(
                                SkillLiveError::UnifiedConflict(
                                    descriptor.display_name().to_owned(),
                                )
                                .to_string(),
                            ),
                        }
                    }
                    (SkillDiscoveryMode::Unified, None, Ok(SkillDiscoveryState::Selected)) => {
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
                    (_, Some(target), Ok(discovery)) => observe_configured_state(
                        native_observation(),
                        target,
                        name,
                        discovery == SkillDiscoveryState::Selected,
                        configs,
                    ),
                    (SkillDiscoveryMode::Unified, _, Err(issue)) => SkillAppState {
                        enabled: None,
                        issue: Some(issue.clone()),
                    },
                    _ => native_observation(),
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
            if !source_valid {
                return SkillAppState {
                    enabled: None,
                    issue: None,
                };
            }
            match inspect_skill_deployment(source_root, destination_root, directory) {
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
        Ok(SkillConfigState::ExternallyDisabled) => append_issue(
            SkillAppState {
                enabled: None,
                issue: native.issue,
            },
            "This Skill is disabled by a platform-specific native setting".to_owned(),
        ),
        Ok(configured @ (SkillConfigState::Enabled | SkillConfigState::Disabled)) => {
            let configured = configured == SkillConfigState::Enabled;
            SkillAppState {
                enabled: if external || !configured {
                    Some(configured)
                } else {
                    native.enabled
                },
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

fn mark_overlapping_targets(targets: &mut [SkillTarget]) {
    let identities = targets
        .iter()
        .map(|target| skill_path_identity(&target.skills_root).map_err(|error| error.to_string()))
        .collect::<Vec<_>>();
    for (target, identity) in targets.iter_mut().zip(&identities) {
        if let Err(issue) = identity {
            append_target_issue(target, issue.clone());
        }
    }
    for left in 0..targets.len() {
        for right in (left + 1)..targets.len() {
            let (Ok(left_path), Ok(right_path)) = (&identities[left], &identities[right]) else {
                continue;
            };
            if left_path == right_path
                || left_path.starts_with(right_path)
                || right_path.starts_with(left_path)
            {
                let right_app = targets[right].app.as_str().to_owned();
                let left_app = targets[left].app.as_str().to_owned();
                let (before_right, from_right) = targets.split_at_mut(right);
                append_target_issue(
                    &mut before_right[left],
                    format!("native Skill directory overlaps application '{right_app}'"),
                );
                append_target_issue(
                    &mut from_right[0],
                    format!("native Skill directory overlaps application '{left_app}'"),
                );
            }
        }
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

    fn skill_request() -> Vec<(String, String, String)> {
        vec![("skill".to_owned(), "Docs".to_owned(), "docs".to_owned())]
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
        let native = SkillAppState {
            enabled: None,
            issue: Some("native state unavailable".to_owned()),
        };
        let enabled = observe_configured_state(
            native,
            SkillConfigTarget::GeminiSettings,
            "Docs",
            false,
            &enabled_configs(),
        );
        assert_eq!(enabled.enabled, None);

        let mut disabled_configs = enabled_configs();
        disabled_configs.insert(
            SkillConfigTarget::GeminiSettings,
            Ok(Some(br#"{"skills":{"disabled":["Docs"]}}"#.to_vec())),
        );
        let disabled = observe_configured_state(
            SkillAppState {
                enabled: None,
                issue: None,
            },
            SkillConfigTarget::GeminiSettings,
            "Docs",
            false,
            &disabled_configs,
        );
        assert_eq!(disabled.enabled, Some(false));
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
        apply_skill_deployment(&source, &unified, "docs", true, SkillSyncMethod::Copy)
            .unwrap()
            .commit()
            .unwrap();
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
        let roots = app_roots(directory.path());
        for (_, root) in &roots {
            fs::create_dir_all(root).unwrap();
        }
        let hermes_root = roots
            .iter()
            .find(|(app, _)| *app == AppType::Hermes)
            .unwrap()
            .1
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
        let roots = app_roots(directory.path());
        for (_, root) in &roots {
            fs::create_dir_all(root).unwrap();
        }
        let live = SkillLiveConfig::new(
            source,
            directory.path().join(".agents/skills"),
            SkillSyncMethod::Copy,
            roots,
        )
        .unwrap();
        let skills = vec![
            ("one".to_owned(), "Docs".to_owned(), "docs".to_owned()),
            ("two".to_owned(), "Ｄocs".to_owned(), "other".to_owned()),
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
        let mut roots = app_roots(directory.path());
        roots
            .iter_mut()
            .find(|(app, _)| app == &AppType::Claude)
            .unwrap()
            .1 = claude;
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
    fn invalid_directory_never_falls_back_to_requested_catalog_state() {
        let directory = tempdir().unwrap();
        let live = SkillLiveConfig::new(
            directory.path().join("source"),
            directory.path().join("unified"),
            SkillSyncMethod::Copy,
            app_roots(directory.path()),
        )
        .unwrap();
        let skills = vec![("skill".to_owned(), "Docs".to_owned(), "../docs".to_owned())];

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
        let mut roots = app_roots(directory.path());
        for (app, root) in &mut roots {
            if matches!(app, AppType::Claude | AppType::Codex) {
                *root = shared_app_root.clone();
            } else {
                fs::create_dir_all(root).unwrap();
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
            live.route("docs", &AppType::Claude),
            Err(SkillLiveError::InvalidTargets(_))
        ));
        assert!(observations[0].app_overrides["hermes"].issue.is_none());
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
                .is_some_and(|contract| contract.discovery() == SkillDiscoveryMode::Unified)
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
