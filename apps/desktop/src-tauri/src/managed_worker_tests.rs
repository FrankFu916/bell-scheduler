use super::*;

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fixture() -> (tempfile::TempDir, serde_json::Value) {
    let directory = tempfile::tempdir().unwrap();
    let contents = directory.path();
    let native = [0xcf, 0xfa, 0xed, 0xfe, 12, 0, 0, 1, 0, 0, 0, 0];
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    fs::create_dir_all(contents.join("Frameworks")).unwrap();
    fs::create_dir_all(contents.join("Resources/solver/macos-arm64")).unwrap();
    for path in [WORKER_PATH, "Frameworks/libtest.dylib"] {
        fs::write(contents.join(path), native).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            contents.join(WORKER_PATH),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let artifacts = [
        ("worker", WORKER_PATH),
        ("dylib", "Frameworks/libtest.dylib"),
    ]
    .map(|(role, path)| {
        serde_json::json!({
            "role": role, "path": path, "sizeBytes": native.len(), "sha256": sha256(&native),
            "sourcePath": path, "sourceSha256": sha256(&native),
        })
    });
    let manifest = serde_json::json!({
        "schemaVersion": 1, "target": "aarch64-apple-darwin", "engineVersion": "9.15.6755",
        "protocolVersion": 1, "adapterVersion": "0.1.0", "minimumMacos": "26.0",
        "workerPath": WORKER_PATH, "distributionSha256": ARCHIVE_SHA256,
        "signing": "adhoc-development", "artifacts": artifacts, "provenance": {},
    });
    (directory, manifest)
}

fn install_manifest(contents: &Path, manifest: &serde_json::Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(manifest).unwrap();
    fs::write(contents.join(MANIFEST_PATH), &bytes).unwrap();
    bytes
}

#[test]
fn controlled_install_resolves_only_manifest_worker() {
    let (directory, manifest) = fixture();
    let expected = install_manifest(directory.path(), &manifest);
    let verified = verify_with_anchor(directory.path(), &expected).unwrap();
    assert_eq!(
        verified.path(),
        directory.path().join(WORKER_PATH).canonicalize().unwrap()
    );
    assert_eq!(verified.manifest_sha256(), sha256(&expected));
    assert!(format!("{:?}", verified.sidecar_spec()).contains("inherit_environment: false"));
}

#[test]
fn no_embedded_install_does_not_fall_back_to_a_supplied_directory() {
    let (directory, manifest) = fixture();
    install_manifest(directory.path(), &manifest);
    assert_eq!(
        verify_with_anchor(directory.path(), &[]).unwrap_err(),
        ManagedWorkerError::NotInstalled
    );
}

#[test]
fn replacing_manifest_and_artifact_cannot_replace_the_build_anchor() {
    let (directory, mut manifest) = fixture();
    let expected = install_manifest(directory.path(), &manifest);
    let changed = b"different executable";
    fs::write(directory.path().join(WORKER_PATH), changed).unwrap();
    manifest["artifacts"][0]["sha256"] = sha256(changed).into();
    manifest["artifacts"][0]["sizeBytes"] = changed.len().into();
    install_manifest(directory.path(), &manifest);
    assert_eq!(
        verify_with_anchor(directory.path(), &expected).unwrap_err(),
        ManagedWorkerError::ManifestMismatch
    );
}

#[test]
fn every_native_file_is_hashed_and_missing_files_fail_closed() {
    for path in [WORKER_PATH, "Frameworks/libtest.dylib"] {
        let (directory, manifest) = fixture();
        let expected = install_manifest(directory.path(), &manifest);
        let mut bytes = fs::read(directory.path().join(path)).unwrap();
        bytes[10] ^= 1;
        fs::write(directory.path().join(path), bytes).unwrap();
        assert_eq!(
            verify_with_anchor(directory.path(), &expected).unwrap_err(),
            ManagedWorkerError::ArtifactHashMismatch
        );
        fs::remove_file(directory.path().join(path)).unwrap();
        assert_eq!(
            verify_with_anchor(directory.path(), &expected).unwrap_err(),
            ManagedWorkerError::ArtifactMissing
        );
    }
}

#[test]
fn invalid_schema_platform_pin_and_duplicate_paths_are_rejected() {
    let (directory, original) = fixture();
    for (field, value) in [
        ("schemaVersion", serde_json::json!(2)),
        ("target", serde_json::json!("x86_64-apple-darwin")),
        ("engineVersion", serde_json::json!("9.99.0")),
        ("protocolVersion", serde_json::json!(2)),
        ("distributionSha256", serde_json::json!("00")),
        ("minimumMacos", serde_json::json!("10.13")),
    ] {
        let mut manifest = original.clone();
        manifest[field] = value;
        let expected = install_manifest(directory.path(), &manifest);
        assert_eq!(
            verify_with_anchor(directory.path(), &expected).unwrap_err(),
            ManagedWorkerError::ManifestInvalid
        );
    }
    let mut manifest = original;
    let duplicate = manifest["artifacts"][0].clone();
    manifest["artifacts"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let expected = install_manifest(directory.path(), &manifest);
    assert_eq!(
        verify_with_anchor(directory.path(), &expected).unwrap_err(),
        ManagedWorkerError::ManifestInvalid
    );
}

#[test]
fn path_escape_and_unexpected_worker_locations_are_rejected() {
    let (directory, original) = fixture();
    for path in ["/tmp/worker", "MacOS/../worker", "MacOS/user-worker"] {
        let mut manifest = original.clone();
        manifest["artifacts"][0]["path"] = path.into();
        let expected = install_manifest(directory.path(), &manifest);
        assert_eq!(
            verify_with_anchor(directory.path(), &expected).unwrap_err(),
            ManagedWorkerError::ArtifactPathInvalid
        );
    }
}

#[test]
fn correct_hash_does_not_make_a_wrong_architecture_executable_valid() {
    let (directory, mut manifest) = fixture();
    let mut wrong_arch = fs::read(directory.path().join(WORKER_PATH)).unwrap();
    wrong_arch[4] = 7;
    fs::write(directory.path().join(WORKER_PATH), &wrong_arch).unwrap();
    manifest["artifacts"][0]["sha256"] = sha256(&wrong_arch).into();
    let expected = install_manifest(directory.path(), &manifest);
    assert_eq!(
        verify_with_anchor(directory.path(), &expected).unwrap_err(),
        ManagedWorkerError::ArtifactArchitectureMismatch
    );
}

#[cfg(unix)]
#[test]
fn symlink_escape_and_non_executable_worker_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (directory, manifest) = fixture();
    let expected = install_manifest(directory.path(), &manifest);
    fs::set_permissions(
        directory.path().join(WORKER_PATH),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_eq!(
        verify_with_anchor(directory.path(), &expected).unwrap_err(),
        ManagedWorkerError::ArtifactNotExecutable
    );
    fs::remove_file(directory.path().join(WORKER_PATH)).unwrap();
    symlink(
        directory.path().join("Frameworks/libtest.dylib"),
        directory.path().join(WORKER_PATH),
    )
    .unwrap();
    assert_eq!(
        verify_with_anchor(directory.path(), &expected).unwrap_err(),
        ManagedWorkerError::ArtifactPathInvalid
    );
}

#[test]
fn oversized_or_malformed_manifests_are_rejected_before_native_reads() {
    let (directory, _) = fixture();
    fs::write(directory.path().join(MANIFEST_PATH), b"{}").unwrap();
    assert_eq!(
        verify_with_anchor(directory.path(), b"{}").unwrap_err(),
        ManagedWorkerError::ManifestInvalid
    );
    let large = vec![b' '; usize::try_from(MAX_MANIFEST_BYTES).unwrap() + 1];
    assert_eq!(
        verify_with_anchor(directory.path(), &large).unwrap_err(),
        ManagedWorkerError::ManifestInvalid
    );
}
