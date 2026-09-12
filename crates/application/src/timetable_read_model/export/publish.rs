use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{
    PreparedScenarioTimetableExport, PublishedScenarioTimetableExport, ScenarioTimetableExportError,
};

/// Publishes a fully rendered file without overwriting an existing target, including a symlink.
/// Uses an existing parent directory and a same-directory temporary file. The publication is the
/// commit point: no fallible operation follows it. Crash recovery is not a filesystem journal.
///
/// # Errors
/// Rejects invalid filenames/extensions, missing parents, existing targets and write/sync errors.
pub fn publish_scenario_timetable_export(
    prepared: &PreparedScenarioTimetableExport,
    path: &Path,
) -> Result<PublishedScenarioTimetableExport, ScenarioTimetableExportError> {
    let target = validated_path(path, prepared.metadata.format.extension())?;
    match fs::symlink_metadata(&target) {
        Ok(_) => return Err(invalid("APPLICATION_TIMETABLE_EXPORT_TARGET_EXISTS")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(failure(
                "APPLICATION_TIMETABLE_EXPORT_TARGET_UNAVAILABLE",
                error,
            ));
        }
    }
    let parent = target
        .parent()
        .ok_or_else(|| invalid("APPLICATION_TIMETABLE_EXPORT_PATH_INVALID"))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".bell-export-")
        .tempfile_in(parent)
        .map_err(|error| failure("APPLICATION_TIMETABLE_EXPORT_STAGING_FAILED", error))?;
    temporary
        .write_all(&prepared.bytes)
        .map_err(|error| failure("APPLICATION_TIMETABLE_EXPORT_WRITE_FAILED", error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| failure("APPLICATION_TIMETABLE_EXPORT_SYNC_FAILED", error))?;
    let result = PublishedScenarioTimetableExport {
        metadata: prepared.metadata.clone(),
        path: target.clone(),
    };
    temporary.persist_noclobber(&target).map_err(|error| {
        let code = if error.error.kind() == io::ErrorKind::AlreadyExists {
            "APPLICATION_TIMETABLE_EXPORT_TARGET_EXISTS"
        } else {
            "APPLICATION_TIMETABLE_EXPORT_PUBLISH_FAILED"
        };
        failure(code, error.error)
    })?;
    Ok(result)
}

fn validated_path(path: &Path, extension: &str) -> Result<PathBuf, ScenarioTimetableExportError> {
    if path.to_str().is_some_and(|value| value.ends_with('/')) {
        return Err(invalid("APPLICATION_TIMETABLE_EXPORT_PATH_INVALID"));
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| invalid("APPLICATION_TIMETABLE_EXPORT_PATH_INVALID"))?;
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (1..=9).any(|index| stem == format!("COM{index}") || stem == format!("LPT{index}"));
    if name.len() > 240
        || name.starts_with('.')
        || name.ends_with(['.', ' '])
        || reserved
        || name
            .chars()
            .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
    {
        return Err(invalid("APPLICATION_TIMETABLE_EXPORT_PATH_INVALID"));
    }
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
    {
        return Err(invalid("APPLICATION_TIMETABLE_EXPORT_EXTENSION_MISMATCH"));
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|error| failure("APPLICATION_TIMETABLE_EXPORT_PARENT_UNAVAILABLE", error))?;
    if !parent.is_dir() {
        return Err(invalid("APPLICATION_TIMETABLE_EXPORT_PARENT_UNAVAILABLE"));
    }
    Ok(parent.join(name))
}

fn invalid(code: &'static str) -> ScenarioTimetableExportError {
    ScenarioTimetableExportError::Invalid { code }
}

fn failure(code: &'static str, source: io::Error) -> ScenarioTimetableExportError {
    ScenarioTimetableExportError::Io { code, source }
}
