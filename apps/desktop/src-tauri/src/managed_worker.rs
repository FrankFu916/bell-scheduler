//! App-relative, manifest-anchored native worker resolution. No frontend executable paths.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use solver_client::SidecarSpec;

const EMBEDDED_MANIFEST: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/managed-worker-manifest.json"));
const MANIFEST_PATH: &str = "Resources/solver/macos-arm64/manifest-v1.json";
const WORKER_PATH: &str = "MacOS/ortools-scheduler-worker";
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_INSTALL_BYTES: u64 = 256 * 1024 * 1024;
const ARCHIVE_SHA256: &str = "de0400a45939a66ee13cd8360c230e830fc5e03a6ed5a8a8b60f58a39e4a67bc";

#[derive(Debug)]
pub struct VerifiedManagedWorker {
    path: PathBuf,
    contents: PathBuf,
    manifest_sha256: String,
}

impl VerifiedManagedWorker {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    /// Only this verified executable is launched; inherited DYLD variables are discarded.
    #[must_use]
    pub fn sidecar_spec(&self) -> SidecarSpec {
        SidecarSpec::new(&self.path)
            .clear_environment()
            .current_dir(&self.contents)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedWorkerError {
    NotInstalled,
    UnsupportedPlatform,
    InvalidAppLayout,
    ManifestInvalid,
    ManifestMismatch,
    ArtifactMissing,
    ArtifactPathInvalid,
    ArtifactHashMismatch,
    ArtifactArchitectureMismatch,
    ArtifactNotExecutable,
    Io,
}

impl ManagedWorkerError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotInstalled => "DESKTOP_WORKER_NOT_INSTALLED",
            Self::UnsupportedPlatform => "DESKTOP_WORKER_PLATFORM_UNSUPPORTED",
            Self::InvalidAppLayout => "DESKTOP_WORKER_APP_LAYOUT_INVALID",
            Self::ManifestInvalid => "DESKTOP_WORKER_MANIFEST_INVALID",
            Self::ManifestMismatch => "DESKTOP_WORKER_MANIFEST_MISMATCH",
            Self::ArtifactMissing => "DESKTOP_WORKER_ARTIFACT_MISSING",
            Self::ArtifactPathInvalid => "DESKTOP_WORKER_ARTIFACT_PATH_INVALID",
            Self::ArtifactHashMismatch => "DESKTOP_WORKER_ARTIFACT_HASH_MISMATCH",
            Self::ArtifactArchitectureMismatch => "DESKTOP_WORKER_ARCHITECTURE_MISMATCH",
            Self::ArtifactNotExecutable => "DESKTOP_WORKER_NOT_EXECUTABLE",
            Self::Io => "DESKTOP_WORKER_IO_FAILED",
        }
    }
}

impl std::fmt::Display for ManagedWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for ManagedWorkerError {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    target: String,
    engine_version: String,
    protocol_version: u32,
    adapter_version: String,
    minimum_macos: String,
    worker_path: String,
    distribution_sha256: String,
    signing: String,
    artifacts: Vec<Artifact>,
    provenance: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Artifact {
    role: ArtifactRole,
    path: String,
    size_bytes: u64,
    sha256: String,
    source_path: String,
    source_sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum ArtifactRole {
    Worker,
    Dylib,
    License,
}

/// Resolves only the worker installed beside the current desktop executable.
///
/// # Errors
/// Missing build anchors, unsupported platforms, wrong app layout and any manifest/artifact
/// mismatch fail closed. There is no PATH, environment, repository or cache fallback.
pub fn resolve_managed_worker(
    _app: &tauri::AppHandle,
) -> Result<VerifiedManagedWorker, ManagedWorkerError> {
    let executable = std::env::current_exe().map_err(|_| ManagedWorkerError::Io)?;
    let executable = executable
        .canonicalize()
        .map_err(|_| ManagedWorkerError::Io)?;
    let macos = executable
        .parent()
        .ok_or(ManagedWorkerError::InvalidAppLayout)?;
    if macos.file_name().is_none_or(|name| name != "MacOS") {
        return Err(ManagedWorkerError::InvalidAppLayout);
    }
    let contents = macos.parent().ok_or(ManagedWorkerError::InvalidAppLayout)?;
    if contents.file_name().is_none_or(|name| name != "Contents") {
        return Err(ManagedWorkerError::InvalidAppLayout);
    }
    verify_managed_worker_install(contents)
}

/// Verifies a developer-selected install tree against this binary's embedded build anchor.
/// This Rust-only entry point supports install-tree verification tools, never a Tauri DTO.
///
/// # Errors
/// Returns the same fail-closed installation errors as [`resolve_managed_worker`].
pub fn verify_managed_worker_install(
    contents: &Path,
) -> Result<VerifiedManagedWorker, ManagedWorkerError> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err(ManagedWorkerError::UnsupportedPlatform);
    }
    verify_with_anchor(contents, EMBEDDED_MANIFEST)
}

fn verify_with_anchor(
    contents: &Path,
    expected: &[u8],
) -> Result<VerifiedManagedWorker, ManagedWorkerError> {
    if expected.is_empty() {
        return Err(ManagedWorkerError::NotInstalled);
    }
    if expected.len() > usize::try_from(MAX_MANIFEST_BYTES).unwrap_or(usize::MAX) {
        return Err(ManagedWorkerError::ManifestInvalid);
    }
    let contents = contents
        .canonicalize()
        .map_err(|_| ManagedWorkerError::ArtifactMissing)?;
    let manifest_path = checked_path(&contents, MANIFEST_PATH)?;
    let installed = read_bounded(&manifest_path, MAX_MANIFEST_BYTES)?;
    if installed != expected {
        return Err(ManagedWorkerError::ManifestMismatch);
    }
    let manifest: Manifest =
        serde_json::from_slice(&installed).map_err(|_| ManagedWorkerError::ManifestInvalid)?;
    validate_manifest(&manifest)?;
    for artifact in &manifest.artifacts {
        verify_artifact(&contents, artifact)?;
    }
    Ok(VerifiedManagedWorker {
        path: checked_path(&contents, WORKER_PATH)?,
        contents,
        manifest_sha256: format!("{:x}", Sha256::digest(&installed)),
    })
}

fn validate_manifest(manifest: &Manifest) -> Result<(), ManagedWorkerError> {
    if manifest.schema_version != 1
        || manifest.target != "aarch64-apple-darwin"
        || manifest.engine_version != "9.15.6755"
        || manifest.protocol_version != 1
        || manifest.adapter_version != "0.1.0"
        || manifest.minimum_macos != "26.0"
        || manifest.worker_path != WORKER_PATH
        || manifest.distribution_sha256 != ARCHIVE_SHA256
        || manifest.signing != "adhoc-development"
        || !manifest.provenance.is_object()
        || manifest.artifacts.is_empty()
        || manifest.artifacts.len() > 256
    {
        return Err(ManagedWorkerError::ManifestInvalid);
    }
    let mut paths = BTreeSet::new();
    let mut total = 0_u64;
    let mut workers = 0;
    let mut dylibs = 0;
    for artifact in &manifest.artifacts {
        total = total
            .checked_add(artifact.size_bytes)
            .ok_or(ManagedWorkerError::ManifestInvalid)?;
        if artifact.size_bytes == 0
            || artifact.size_bytes > MAX_ARTIFACT_BYTES
            || !hex_digest(&artifact.sha256)
            || !hex_digest(&artifact.source_sha256)
            || artifact.source_path.is_empty()
            || !paths.insert(&artifact.path)
        {
            return Err(ManagedWorkerError::ManifestInvalid);
        }
        let path = Path::new(&artifact.path);
        match artifact.role {
            ArtifactRole::Worker if artifact.path == WORKER_PATH => workers += 1,
            ArtifactRole::Dylib
                if path.parent() == Some(Path::new("Frameworks"))
                    && path
                        .extension()
                        .is_some_and(|extension| extension == "dylib") =>
            {
                dylibs += 1;
            }
            ArtifactRole::License if path.starts_with("Resources/solver/macos-arm64/licenses") => {}
            _ => return Err(ManagedWorkerError::ArtifactPathInvalid),
        }
    }
    if workers != 1 || dylibs == 0 || total > MAX_INSTALL_BYTES {
        return Err(ManagedWorkerError::ManifestInvalid);
    }
    Ok(())
}

fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn checked_path(contents: &Path, relative: &str) -> Result<PathBuf, ManagedWorkerError> {
    let mut path = contents.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(name) = component else {
            return Err(ManagedWorkerError::ArtifactPathInvalid);
        };
        path.push(name);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ManagedWorkerError::ArtifactMissing
            } else {
                ManagedWorkerError::Io
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ManagedWorkerError::ArtifactPathInvalid);
        }
    }
    if path == contents {
        return Err(ManagedWorkerError::ArtifactPathInvalid);
    }
    Ok(path)
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, ManagedWorkerError> {
    let file = File::open(path).map_err(|_| ManagedWorkerError::Io)?;
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ManagedWorkerError::Io)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(ManagedWorkerError::ManifestInvalid);
    }
    Ok(bytes)
}

fn verify_artifact(contents: &Path, artifact: &Artifact) -> Result<(), ManagedWorkerError> {
    let path = checked_path(contents, &artifact.path)?;
    let mut file = File::open(&path).map_err(|_| ManagedWorkerError::Io)?;
    let metadata = file.metadata().map_err(|_| ManagedWorkerError::Io)?;
    if !metadata.is_file() || metadata.len() != artifact.size_bytes {
        return Err(ManagedWorkerError::ArtifactHashMismatch);
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut read_bytes = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(|_| ManagedWorkerError::Io)?;
        if count == 0 {
            break;
        }
        read_bytes += u64::try_from(count).map_err(|_| ManagedWorkerError::ArtifactHashMismatch)?;
        if read_bytes > artifact.size_bytes {
            return Err(ManagedWorkerError::ArtifactHashMismatch);
        }
        hasher.update(&buffer[..count]);
    }
    if format!("{:x}", hasher.finalize()) != artifact.sha256 {
        return Err(ManagedWorkerError::ArtifactHashMismatch);
    }
    if artifact.role != ArtifactRole::License {
        file.rewind().map_err(|_| ManagedWorkerError::Io)?;
        let mut header = [0; 12];
        file.read_exact(&mut header)
            .map_err(|_| ManagedWorkerError::ArtifactArchitectureMismatch)?;
        if header[..4] != [0xcf, 0xfa, 0xed, 0xfe] || header[4..8] != [12, 0, 0, 1] {
            return Err(ManagedWorkerError::ArtifactArchitectureMismatch);
        }
    }
    #[cfg(unix)]
    if artifact.role == ArtifactRole::Worker {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(ManagedWorkerError::ArtifactNotExecutable);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "managed_worker_tests.rs"]
mod tests;
