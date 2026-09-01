use std::path::Path;

use cc_switch_core::{builtin_app_registry, AppType, SkillDiscoveryMode, SkillSelectionMode};

use crate::live::ResolvedConfigDirs;

type RootResolver = fn(&ResolvedConfigDirs) -> &Path;

pub(crate) struct SkillHostAdapter {
    app: AppType,
    catalog_column: Option<&'static str>,
    install_root: RootResolver,
    unified_discovery_root: Option<RootResolver>,
}

impl SkillHostAdapter {
    const fn new(
        app: AppType,
        catalog_column: Option<&'static str>,
        install_root: RootResolver,
    ) -> Self {
        Self {
            app,
            catalog_column,
            install_root,
            unified_discovery_root: None,
        }
    }

    const fn with_unified_discovery_root(mut self, resolver: RootResolver) -> Self {
        self.unified_discovery_root = Some(resolver);
        self
    }

    pub(crate) fn app(&self) -> &AppType {
        &self.app
    }

    pub(crate) const fn catalog_column(&self) -> Option<&'static str> {
        self.catalog_column
    }

    pub(crate) fn install_root<'a>(&self, dirs: &'a ResolvedConfigDirs) -> &'a Path {
        (self.install_root)(dirs)
    }

    pub(crate) fn unified_discovery_root<'a>(
        &self,
        dirs: &'a ResolvedConfigDirs,
    ) -> Option<&'a Path> {
        self.unified_discovery_root.map(|resolver| resolver(dirs))
    }
}

fn claude_root(dirs: &ResolvedConfigDirs) -> &Path {
    &dirs.claude
}

fn codex_root(dirs: &ResolvedConfigDirs) -> &Path {
    &dirs.codex
}

fn gemini_root(dirs: &ResolvedConfigDirs) -> &Path {
    &dirs.gemini
}

fn gemini_unified_root(dirs: &ResolvedConfigDirs) -> &Path {
    &dirs.gemini_unified_skills
}

fn grok_root(dirs: &ResolvedConfigDirs) -> &Path {
    &dirs.grok
}

fn opencode_root(dirs: &ResolvedConfigDirs) -> &Path {
    &dirs.opencode
}

fn hermes_root(dirs: &ResolvedConfigDirs) -> &Path {
    &dirs.hermes
}

fn pi_root(dirs: &ResolvedConfigDirs) -> &Path {
    &dirs.pi
}

static SKILL_HOST_ADAPTERS: [SkillHostAdapter; 7] = [
    SkillHostAdapter::new(AppType::Claude, Some("enabled_claude"), claude_root),
    SkillHostAdapter::new(AppType::Codex, Some("enabled_codex"), codex_root),
    SkillHostAdapter::new(AppType::Gemini, Some("enabled_gemini"), gemini_root)
        .with_unified_discovery_root(gemini_unified_root),
    SkillHostAdapter::new(AppType::GrokBuild, Some("enabled_grokbuild"), grok_root),
    SkillHostAdapter::new(AppType::OpenCode, Some("enabled_opencode"), opencode_root),
    SkillHostAdapter::new(AppType::Hermes, Some("enabled_hermes"), hermes_root),
    SkillHostAdapter::new(AppType::Pi, None, pi_root),
];

pub(crate) fn skill_host_adapters() -> &'static [SkillHostAdapter] {
    &SKILL_HOST_ADAPTERS
}

pub(crate) fn skill_host_adapter(app: &AppType) -> Option<&'static SkillHostAdapter> {
    SKILL_HOST_ADAPTERS
        .iter()
        .find(|adapter| adapter.app() == app)
}

pub(crate) fn validate_skill_host_adapters() -> Result<(), String> {
    for descriptor in builtin_app_registry().descriptors() {
        let matching = SKILL_HOST_ADAPTERS
            .iter()
            .filter(|adapter| adapter.app() == descriptor.app())
            .collect::<Vec<_>>();
        let expected = usize::from(descriptor.skill_contract().is_some());
        if matching.len() != expected {
            return Err(format!(
                "application '{}' requires {expected} Skill host adapter(s), found {}",
                descriptor.id(),
                matching.len()
            ));
        }
        let (Some(contract), Some(adapter)) = (descriptor.skill_contract(), matching.first())
        else {
            continue;
        };
        if (contract.selection() == SkillSelectionMode::HostManaged)
            != adapter.catalog_column().is_some()
        {
            return Err(format!(
                "application '{}' has an invalid Skill catalog binding",
                descriptor.id()
            ));
        }
        if contract.discovery() == SkillDiscoveryMode::Managed
            && adapter.unified_discovery_root.is_some()
        {
            return Err(format!(
                "application '{}' has an unexpected unified Skill root",
                descriptor.id()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_adapters_cover_the_core_skill_registry_once() {
        validate_skill_host_adapters().unwrap();
    }
}
