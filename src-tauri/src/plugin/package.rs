use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use super::{
    io_error, state::create_private_dir, validate_dotted_id, validate_sha256, PluginError,
    PluginManifest, RegistryPackage, RegistrySource, SignatureAlgorithm, TrustedPublisherKey,
    COMPONENT_PATH,
};

pub const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_EXPANDED_BYTES: usize = 32 * 1024 * 1024;
const MAX_COMPONENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_SIGNATURE_BYTES: usize = 1024;
const MAX_ARCHIVE_ENTRIES: usize = 130;

pub struct PreparedPackage {
    pub manifest: PluginManifest,
    pub staging_path: PathBuf,
}

impl Drop for PreparedPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.staging_path);
    }
}

pub fn verify_registry_entry(
    registry: &RegistrySource,
    package: &RegistryPackage,
) -> Result<Vec<u8>, PluginError> {
    package
        .manifest
        .validate()
        .map_err(PluginError::Verification)?;
    validate_sha256(&package.manifest_sha256, "manifest digest")
        .map_err(PluginError::Verification)?;
    validate_sha256(&package.package_sha256, "package digest")
        .map_err(PluginError::Verification)?;
    let canonical = package
        .manifest
        .canonical_bytes()
        .map_err(PluginError::Verification)?;
    if sha256(&canonical) != package.manifest_sha256 {
        return Err(PluginError::Verification(
            "registry manifest digest does not match canonical contents".to_owned(),
        ));
    }
    verify_signature(registry, &package.manifest, &canonical, &package.signature)?;
    Ok(canonical)
}

pub fn prepare_package(
    registry: &RegistrySource,
    package: &RegistryPackage,
    archive: &[u8],
    staging_path: PathBuf,
) -> Result<PreparedPackage, PluginError> {
    if archive.len() > MAX_PACKAGE_BYTES {
        return Err(PluginError::Verification(
            "plugin archive exceeds the download limit".to_owned(),
        ));
    }
    if sha256(archive) != package.package_sha256 {
        return Err(PluginError::Verification(
            "plugin archive digest does not match the registry".to_owned(),
        ));
    }
    let canonical = verify_registry_entry(registry, package)?;
    let signature = package.signature.as_bytes();
    if signature.len() > MAX_SIGNATURE_BYTES {
        return Err(PluginError::Verification(
            "plugin signature encoding exceeds the limit".to_owned(),
        ));
    }

    create_private_dir(&staging_path)?;
    let result = (|| {
        let mut zip = ZipArchive::new(Cursor::new(archive)).map_err(|_| {
            PluginError::Verification("plugin archive is not a valid ZIP file".to_owned())
        })?;
        if zip.is_empty() || zip.len() > MAX_ARCHIVE_ENTRIES {
            return Err(PluginError::Verification(
                "plugin archive entry count exceeds the limit".to_owned(),
            ));
        }

        let declared = package
            .manifest
            .files
            .keys()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        let mut expanded = 0usize;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|_| {
                PluginError::Verification("plugin archive entry cannot be read".to_owned())
            })?;
            if entry.is_dir() || entry.encrypted() {
                return Err(PluginError::Verification(
                    "plugin archives cannot contain directories or encrypted entries".to_owned(),
                ));
            }
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
            {
                return Err(PluginError::Verification(
                    "plugin archives cannot contain symbolic links".to_owned(),
                ));
            }
            let enclosed = entry.enclosed_name().ok_or_else(|| {
                PluginError::Verification("plugin archive path is unsafe".to_owned())
            })?;
            let name = entry.name().to_owned();
            if enclosed.to_string_lossy() != name || !seen.insert(name.clone()) {
                return Err(PluginError::Verification(
                    "plugin archive has a noncanonical or duplicate path".to_owned(),
                ));
            }
            if name != "manifest.json"
                && name != "manifest.sig"
                && !declared.contains(name.as_str())
            {
                return Err(PluginError::Verification(
                    "plugin archive contains an undeclared payload".to_owned(),
                ));
            }
            let limit = match name.as_str() {
                "manifest.json" => MAX_MANIFEST_BYTES,
                "manifest.sig" => MAX_SIGNATURE_BYTES,
                COMPONENT_PATH => MAX_COMPONENT_BYTES,
                _ => MAX_PAYLOAD_BYTES,
            };
            if entry.size() > limit as u64 {
                return Err(PluginError::Verification(
                    "plugin archive entry exceeds its size limit".to_owned(),
                ));
            }
            let mut contents = Vec::with_capacity(entry.size() as usize);
            entry
                .by_ref()
                .take((limit + 1) as u64)
                .read_to_end(&mut contents)
                .map_err(|_| {
                    PluginError::Verification("plugin archive entry is corrupt".to_owned())
                })?;
            if contents.len() > limit {
                return Err(PluginError::Verification(
                    "plugin archive entry exceeds its size limit".to_owned(),
                ));
            }
            expanded = expanded.checked_add(contents.len()).ok_or_else(|| {
                PluginError::Verification("plugin archive size overflow".to_owned())
            })?;
            if expanded > MAX_EXPANDED_BYTES {
                return Err(PluginError::Verification(
                    "plugin archive expands beyond the size limit".to_owned(),
                ));
            }

            match name.as_str() {
                "manifest.json" if contents != canonical => {
                    return Err(PluginError::Verification(
                        "archive manifest is not the signed canonical manifest".to_owned(),
                    ));
                }
                "manifest.sig" if contents != signature => {
                    return Err(PluginError::Verification(
                        "archive signature differs from the registry entry".to_owned(),
                    ));
                }
                "manifest.json" | "manifest.sig" => {}
                _ => {
                    let expected = package.manifest.files.get(&name).ok_or_else(|| {
                        PluginError::Verification("payload is not declared".to_owned())
                    })?;
                    if sha256(&contents) != *expected {
                        return Err(PluginError::Verification(
                            "plugin payload digest does not match the manifest".to_owned(),
                        ));
                    }
                }
            }
            write_private_file(&staging_path.join(&enclosed), &contents)?;
        }

        if !seen.contains("manifest.json")
            || !seen.contains("manifest.sig")
            || !declared.iter().all(|path| seen.contains(*path))
        {
            return Err(PluginError::Verification(
                "plugin archive is missing a declared entry".to_owned(),
            ));
        }
        Ok(PreparedPackage {
            manifest: package.manifest.clone(),
            staging_path: staging_path.clone(),
        })
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&staging_path);
    }
    result
}

pub fn verify_installed_directory(
    directory: &Path,
    manifest: &PluginManifest,
    signature: &[u8],
) -> Result<(), PluginError> {
    let root_metadata =
        fs::symlink_metadata(directory).map_err(|source| io_error(directory, source))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(PluginError::InvalidState(
            "installed plugin root is not a directory".to_owned(),
        ));
    }
    let mut expected = manifest.files.keys().cloned().collect::<HashSet<_>>();
    expected.insert("manifest.json".to_owned());
    expected.insert("manifest.sig".to_owned());
    let mut actual = HashSet::new();
    collect_regular_files(directory, directory, &mut actual)?;
    if actual != expected {
        return Err(PluginError::InvalidState(
            "installed plugin files differ from the signed package".to_owned(),
        ));
    }

    let canonical = manifest
        .canonical_bytes()
        .map_err(PluginError::InvalidState)?;
    if read_regular_bounded(&directory.join("manifest.json"), MAX_MANIFEST_BYTES)? != canonical
        || read_regular_bounded(&directory.join("manifest.sig"), MAX_SIGNATURE_BYTES)? != signature
    {
        return Err(PluginError::InvalidState(
            "installed plugin metadata differs from the signed package".to_owned(),
        ));
    }
    let mut expanded = canonical.len().saturating_add(signature.len());
    for (path, expected_digest) in &manifest.files {
        let limit = if path == COMPONENT_PATH {
            MAX_COMPONENT_BYTES
        } else {
            MAX_PAYLOAD_BYTES
        };
        let contents = read_regular_bounded(&directory.join(path), limit)?;
        expanded = expanded.checked_add(contents.len()).ok_or_else(|| {
            PluginError::InvalidState("installed plugin size overflow".to_owned())
        })?;
        if expanded > MAX_EXPANDED_BYTES || sha256(&contents) != *expected_digest {
            return Err(PluginError::InvalidState(
                "installed plugin payload differs from its signed digest".to_owned(),
            ));
        }
    }
    Ok(())
}

fn collect_regular_files(
    root: &Path,
    directory: &Path,
    files: &mut HashSet<String>,
) -> Result<(), PluginError> {
    let entries = fs::read_dir(directory).map_err(|source| io_error(directory, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io_error(directory, source))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
        if metadata.file_type().is_symlink() {
            return Err(PluginError::InvalidState(
                "installed plugin contains a symbolic link".to_owned(),
            ));
        }
        if metadata.is_dir() {
            collect_regular_files(root, &path, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(PluginError::InvalidState(
                "installed plugin contains a non-file payload".to_owned(),
            ));
        }
        let relative = path.strip_prefix(root).map_err(|_| {
            PluginError::InvalidState("installed plugin path escaped its directory".to_owned())
        })?;
        let relative = relative.to_str().ok_or_else(|| {
            PluginError::InvalidState("installed plugin path is not UTF-8".to_owned())
        })?;
        files.insert(relative.replace(std::path::MAIN_SEPARATOR, "/"));
    }
    Ok(())
}

fn read_regular_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, PluginError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(PluginError::InvalidState(
            "installed plugin payload is not a bounded regular file".to_owned(),
        ));
    }
    let file = fs::File::open(path).map_err(|source| io_error(path, source))?;
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take((limit + 1) as u64)
        .read_to_end(&mut contents)
        .map_err(|source| io_error(path, source))?;
    if contents.len() > limit {
        return Err(PluginError::InvalidState(
            "installed plugin payload exceeds its size limit".to_owned(),
        ));
    }
    Ok(contents)
}

pub fn validate_trusted_key(key: &TrustedPublisherKey) -> Result<(), PluginError> {
    validate_dotted_id(&key.publisher_id, "publisher ID").map_err(PluginError::Invalid)?;
    if key.key_id.is_empty()
        || key.key_id.len() > 128
        || !key
            .key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(PluginError::Invalid(
            "publisher key ID is not a safe identifier".to_owned(),
        ));
    }
    decode_verifying_key(&key.public_key)?;
    Ok(())
}

pub fn publisher_key_sha256(
    registry: &RegistrySource,
    publisher: &super::PublisherIdentity,
) -> Result<String, PluginError> {
    let trusted = registry
        .trusted_publishers
        .iter()
        .find(|key| key.publisher_id == publisher.id && key.key_id == publisher.key_id)
        .ok_or_else(|| {
            PluginError::Verification("publisher key is not trusted by this registry".to_owned())
        })?;
    Ok(sha256(
        decode_verifying_key(&trusted.public_key)?.as_bytes(),
    ))
}

fn verify_signature(
    registry: &RegistrySource,
    manifest: &PluginManifest,
    canonical: &[u8],
    encoded_signature: &str,
) -> Result<(), PluginError> {
    if manifest.publisher.algorithm != SignatureAlgorithm::Ed25519 {
        return Err(PluginError::Verification(
            "plugin signature algorithm is unsupported".to_owned(),
        ));
    }
    let trusted = registry
        .trusted_publishers
        .iter()
        .find(|key| {
            key.publisher_id == manifest.publisher.id && key.key_id == manifest.publisher.key_id
        })
        .ok_or_else(|| {
            PluginError::Verification("publisher key is not trusted by this registry".to_owned())
        })?;
    let key = decode_verifying_key(&trusted.public_key)?;
    let signature_bytes = STANDARD.decode(encoded_signature).map_err(|_| {
        PluginError::Verification("plugin signature is not valid base64".to_owned())
    })?;
    let signature = Signature::from_slice(&signature_bytes).map_err(|_| {
        PluginError::Verification("plugin signature has an invalid length".to_owned())
    })?;
    key.verify_strict(canonical, &signature)
        .map_err(|_| PluginError::Verification("plugin signature is invalid".to_owned()))
}

fn decode_verifying_key(encoded: &str) -> Result<VerifyingKey, PluginError> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| PluginError::Invalid("publisher public key is not valid base64".to_owned()))?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        PluginError::Invalid("publisher public key must contain 32 bytes".to_owned())
    })?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| PluginError::Invalid("publisher public key is invalid".to_owned()))
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), PluginError> {
    let parent = path
        .parent()
        .ok_or_else(|| PluginError::Verification("payload path has no parent".to_owned()))?;
    create_private_dir(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.write_all(contents)
        .and_then(|_| file.sync_all())
        .map_err(|source| io_error(path, source))
}

fn sha256(contents: &[u8]) -> String {
    let digest = Sha256::digest(contents);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        plugin::{
            ManifestAdapter, PluginCapability, PublisherIdentity, RegistryPackage,
            SignatureAlgorithm,
        },
        provider::{FieldKind, FormField},
    };
    use ed25519_dalek::{Signer, SigningKey};
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

    struct SignedPackage {
        registry: RegistrySource,
        package: RegistryPackage,
        archive: Vec<u8>,
    }

    fn signed_package(component: &[u8], archived_component: &[u8]) -> SignedPackage {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let mut manifest = PluginManifest {
            schema_version: 1,
            id: "dev.example.fixture".to_owned(),
            version: "1.0.0".to_owned(),
            host_api_major: 1,
            name: "Fixture".to_owned(),
            description: "Test fixture".to_owned(),
            publisher: PublisherIdentity {
                id: "dev.example".to_owned(),
                key_id: "release-1".to_owned(),
                algorithm: SignatureAlgorithm::Ed25519,
            },
            component: COMPONENT_PATH.to_owned(),
            adapters: vec![ManifestAdapter {
                app_id: "claude".to_owned(),
                adapter_id: "example.claude".to_owned(),
                display_name: "Example Claude".to_owned(),
                schema_version: 1,
                fields: vec![FormField {
                    key: "token".to_owned(),
                    label: "Token".to_owned(),
                    kind: FieldKind::Secret,
                    required: true,
                    placeholder: String::new(),
                    help: String::new(),
                }],
            }],
            capabilities: vec![
                PluginCapability::ReadClaudeSettings,
                PluginCapability::WriteClaudeSettings,
            ],
            files: Default::default(),
        };
        manifest
            .files
            .insert(COMPONENT_PATH.to_owned(), sha256(component));
        let canonical = manifest.canonical_bytes().expect("canonical manifest");
        let signature = STANDARD.encode(signing_key.sign(&canonical).to_bytes());
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, contents) in [
            ("manifest.json", canonical.as_slice()),
            ("manifest.sig", signature.as_bytes()),
            (COMPONENT_PATH, archived_component),
        ] {
            writer.start_file(name, options).expect("start ZIP entry");
            writer.write_all(contents).expect("write ZIP entry");
        }
        let archive = writer.finish().expect("finish ZIP").into_inner();
        let package = RegistryPackage {
            manifest,
            manifest_sha256: sha256(&canonical),
            signature,
            package_url: "fixture.zip".to_owned(),
            package_sha256: sha256(&archive),
        };
        let registry = RegistrySource {
            id: "registry".to_owned(),
            revision: 1,
            label: "Test".to_owned(),
            index_url: "https://plugins.example.com/index.json".to_owned(),
            enabled: true,
            trusted_publishers: vec![TrustedPublisherKey {
                publisher_id: "dev.example".to_owned(),
                key_id: "release-1".to_owned(),
                public_key: STANDARD.encode(signing_key.verifying_key().as_bytes()),
            }],
        };
        SignedPackage {
            registry,
            package,
            archive,
        }
    }

    #[test]
    fn verifies_and_extracts_a_signed_package() {
        let component = include_bytes!("../../testdata/plugin-fixture.wasm");
        let signed = signed_package(component, component);
        let directory = tempfile::tempdir().expect("temporary directory");
        let staging = directory.path().join("staging");

        let prepared = prepare_package(
            &signed.registry,
            &signed.package,
            &signed.archive,
            staging.clone(),
        )
        .expect("prepare signed package");

        assert_eq!(
            std::fs::read(staging.join(COMPONENT_PATH)).expect("read extracted component"),
            component
        );
        assert_eq!(prepared.manifest.id, "dev.example.fixture");
    }

    #[test]
    fn rejects_a_payload_that_differs_from_the_signed_manifest() {
        let component = include_bytes!("../../testdata/plugin-fixture.wasm");
        let signed = signed_package(component, b"tampered component");
        let directory = tempfile::tempdir().expect("temporary directory");

        let result = prepare_package(
            &signed.registry,
            &signed.package,
            &signed.archive,
            directory.path().join("staging"),
        );

        assert!(matches!(result, Err(PluginError::Verification(_))));
        assert!(!directory.path().join("staging").exists());
    }

    #[test]
    fn rejects_a_signature_from_an_untrusted_key() {
        let component = include_bytes!("../../testdata/plugin-fixture.wasm");
        let mut signed = signed_package(component, component);
        signed.registry.trusted_publishers[0].public_key =
            STANDARD.encode(SigningKey::from_bytes(&[8; 32]).verifying_key().as_bytes());

        let result = verify_registry_entry(&signed.registry, &signed.package);

        assert!(matches!(result, Err(PluginError::Verification(_))));
    }

    #[test]
    fn installed_directory_must_still_match_every_signed_payload() {
        let signed = signed_package(b"component", b"component");
        let directory = tempfile::tempdir().unwrap();
        let prepared = prepare_package(
            &signed.registry,
            &signed.package,
            &signed.archive,
            directory.path().join("staging"),
        )
        .unwrap();

        verify_installed_directory(
            &prepared.staging_path,
            &prepared.manifest,
            signed.package.signature.as_bytes(),
        )
        .unwrap();
        fs::write(prepared.staging_path.join(COMPONENT_PATH), "changed").unwrap();
        assert!(verify_installed_directory(
            &prepared.staging_path,
            &prepared.manifest,
            signed.package.signature.as_bytes(),
        )
        .is_err());
    }
}
