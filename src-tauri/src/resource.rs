use std::path::{Path, PathBuf};

use cc_switch_core::NativeResourcePath;

pub(crate) fn resolve_config_root_resource(
    root: &Path,
    resource: NativeResourcePath,
) -> Option<PathBuf> {
    let (preferred, fallbacks) = resource.config_root_relative()?;
    let preferred = root.join(preferred);
    if resource_is_present(&preferred) {
        return Some(preferred);
    }
    Some(
        fallbacks
            .iter()
            .map(|fallback| root.join(fallback))
            .find(|path| resource_is_present(path))
            .unwrap_or(preferred),
    )
}

fn resource_is_present(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|_| true)
        .unwrap_or_else(|error| error.kind() != std::io::ErrorKind::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_and_fallback_selection_is_deterministic() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let resource = NativeResourcePath::ConfigRootRelative {
            preferred: "preferred.json",
            fallbacks: &["legacy.json"],
        };

        assert_eq!(
            resolve_config_root_resource(directory.path(), resource),
            Some(directory.path().join("preferred.json"))
        );
        std::fs::write(directory.path().join("legacy.json"), "{}").expect("write fallback");
        assert_eq!(
            resolve_config_root_resource(directory.path(), resource),
            Some(directory.path().join("legacy.json"))
        );
        std::fs::write(directory.path().join("preferred.json"), "{}").expect("write preferred");
        assert_eq!(
            resolve_config_root_resource(directory.path(), resource),
            Some(directory.path().join("preferred.json"))
        );
    }
}
