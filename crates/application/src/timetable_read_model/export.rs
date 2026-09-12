//! Complete, bounded exports of one explicitly selected adopted scenario object.

mod publish;
mod tabular;
mod xlsx;

use std::io::{self, Cursor, Seek, SeekFrom, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use class_schedule_domain::{Revision, ScenarioId};
use class_schedule_persistence::SqliteStore;
use rust_xlsxwriter::XlsxError;
use thiserror::Error;

use super::{
    ScenarioTimetableQueryError, TimetableEntityOption, TimetableFilter, TimetableGridCell,
    TimetableQueryError, TimetableRow, projection, scenario,
};
use crate::ScenarioReceipt;

pub use publish::publish_scenario_timetable_export;

pub const SCENARIO_TIMETABLE_EXPORT_SCHEMA_VERSION: u32 = 1;
pub const MAXIMUM_TIMETABLE_EXPORT_MEETINGS: usize = 100_000;
pub const MAXIMUM_TIMETABLE_EXPORT_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_CELL_CHARACTERS: usize = 32_767;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioTimetableExportFormat {
    Csv,
    Xlsx,
}

impl ScenarioTimetableExportFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Xlsx => "xlsx",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScenarioTimetableExportCommand {
    pub scenario_id: ScenarioId,
    pub expected_scenario_revision: Revision,
    pub expected_timetable_revision: Revision,
    pub filter: TimetableFilter,
    pub format: ScenarioTimetableExportFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioTimetableExportMetadata {
    pub schema_version: u32,
    pub receipt: ScenarioReceipt,
    /// Whether the source was current when revalidated for this export; it is never rebased.
    pub source_is_current: bool,
    pub scenario_display_name: String,
    pub selection: TimetableEntityOption,
    pub format: ScenarioTimetableExportFormat,
    pub meeting_count: u32,
    pub generated_at: DateTime<Utc>,
    pub byte_length: u64,
    pub payload_hash: String,
}

/// Only a completely revalidated and rendered export can reach the publishing operation.
#[derive(Debug)]
pub struct PreparedScenarioTimetableExport {
    metadata: ScenarioTimetableExportMetadata,
    bytes: Vec<u8>,
}

impl PreparedScenarioTimetableExport {
    pub const fn metadata(&self) -> &ScenarioTimetableExportMetadata {
        &self.metadata
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedScenarioTimetableExport {
    pub metadata: ScenarioTimetableExportMetadata,
    pub path: PathBuf,
}

#[derive(Debug, Error)]
pub enum ScenarioTimetableExportError {
    #[error(transparent)]
    Query(Box<ScenarioTimetableQueryError>),
    #[error("export request or content failed validation: {code}")]
    Invalid { code: &'static str },
    #[error("cannot encode the timetable CSV")]
    Csv(#[from] csv::Error),
    #[error("cannot encode the timetable workbook")]
    Xlsx(#[from] XlsxError),
    #[error("cannot publish the timetable file: {code}")]
    Io {
        code: &'static str,
        #[source]
        source: io::Error,
    },
}

impl ScenarioTimetableExportError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Query(error) => error.code(),
            Self::Invalid { code } | Self::Io { code, .. } => code,
            Self::Csv(error) => match error.kind() {
                csv::ErrorKind::Io(error) if error.kind() == io::ErrorKind::FileTooLarge => {
                    "APPLICATION_TIMETABLE_EXPORT_RESOURCE_LIMIT"
                }
                _ => "APPLICATION_TIMETABLE_EXPORT_CSV_FAILED",
            },
            Self::Xlsx(_) => "APPLICATION_TIMETABLE_EXPORT_XLSX_FAILED",
        }
    }
}

impl From<ScenarioTimetableQueryError> for ScenarioTimetableExportError {
    fn from(error: ScenarioTimetableQueryError) -> Self {
        Self::Query(Box::new(error))
    }
}

/// Loads the exact scenario revisions once and renders every meeting of the requested object.
/// No database or destination-file mutation occurs. This operation starts no worker.
///
/// # Errors
/// Rejects invalid identities, revisions, damaged payloads, unknown objects and size limits.
pub fn prepare_scenario_timetable_export(
    store: &SqliteStore,
    command: &ScenarioTimetableExportCommand,
) -> Result<PreparedScenarioTimetableExport, ScenarioTimetableExportError> {
    let loaded = scenario::load_expected_scenario(
        store,
        command.scenario_id,
        command.expected_scenario_revision,
        command.expected_timetable_revision,
    )?;
    let input = scenario::projection_input(&loaded);
    let selection = projection::entities(&input, command.filter.view())
        .into_iter()
        .find(|entity| entity.filter == command.filter)
        .ok_or_else(|| {
            ScenarioTimetableQueryError::from(TimetableQueryError::EntityNotFound {
                view: command.filter.view(),
            })
        })?;
    let (rows, calendar, meeting_count) = projection::project_complete(
        &input,
        command.filter,
        MAXIMUM_TIMETABLE_EXPORT_MEETINGS,
        MAXIMUM_TIMETABLE_EXPORT_BYTES,
    )
    .map_err(|error| match error {
        TimetableQueryError::ResourceLimit => resource_limit(),
        other => ScenarioTimetableQueryError::from(other).into(),
    })?;
    let mut metadata = ScenarioTimetableExportMetadata {
        schema_version: SCENARIO_TIMETABLE_EXPORT_SCHEMA_VERSION,
        receipt: loaded.receipt().clone(),
        source_is_current: loaded.source_is_current(),
        scenario_display_name: loaded.display_name().to_owned(),
        selection,
        format: command.format,
        meeting_count,
        generated_at: DateTime::from_timestamp_millis(Utc::now().timestamp_millis())
            .ok_or_else(resource_limit)?,
        byte_length: 0,
        payload_hash: String::new(),
    };
    let data = ExportData {
        metadata: &metadata,
        rows: &rows,
        calendar: &calendar,
    };
    let bytes = match command.format {
        ScenarioTimetableExportFormat::Csv => tabular::render_csv(&data)?,
        ScenarioTimetableExportFormat::Xlsx => xlsx::render(&data)?,
    };
    metadata.byte_length = u64::try_from(bytes.len()).map_err(|_| resource_limit())?;
    metadata.payload_hash = blake3::hash(&bytes).to_hex().to_string();
    Ok(PreparedScenarioTimetableExport { metadata, bytes })
}

struct ExportData<'a> {
    metadata: &'a ScenarioTimetableExportMetadata,
    rows: &'a [TimetableRow],
    calendar: &'a [TimetableGridCell],
}

fn resource_limit() -> ScenarioTimetableExportError {
    ScenarioTimetableExportError::Invalid {
        code: "APPLICATION_TIMETABLE_EXPORT_RESOURCE_LIMIT",
    }
}

#[derive(Default)]
struct TextBudget {
    bytes: usize,
}

impl TextBudget {
    fn check(&mut self, text: &str) -> Result<(), ScenarioTimetableExportError> {
        if text.chars().count() > MAXIMUM_CELL_CHARACTERS {
            return Err(ScenarioTimetableExportError::Invalid {
                code: "APPLICATION_TIMETABLE_EXPORT_CELL_LIMIT",
            });
        }
        self.bytes = self
            .bytes
            .checked_add(text.len())
            .ok_or_else(resource_limit)?;
        if self.bytes > MAXIMUM_TIMETABLE_EXPORT_BYTES {
            return Err(resource_limit());
        }
        Ok(())
    }
}

#[derive(Default)]
struct BoundedBuffer {
    cursor: Cursor<Vec<u8>>,
    exceeded: bool,
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .cursor
            .position()
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > MAXIMUM_TIMETABLE_EXPORT_BYTES as u64)
        {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "export byte limit exceeded",
            ));
        }
        self.cursor.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.cursor.flush()
    }
}

impl Seek for BoundedBuffer {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.cursor.seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BoundedBuffer, MAXIMUM_CELL_CHARACTERS, MAXIMUM_TIMETABLE_EXPORT_BYTES, TextBudget,
    };
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn cell_and_total_text_limits_count_unicode_without_silent_truncation() {
        let mut budget = TextBudget::default();
        budget.check(&"中".repeat(MAXIMUM_CELL_CHARACTERS)).unwrap();
        assert_eq!(
            budget
                .check(&"中".repeat(MAXIMUM_CELL_CHARACTERS + 1))
                .unwrap_err()
                .code(),
            "APPLICATION_TIMETABLE_EXPORT_CELL_LIMIT"
        );
        let mut budget = TextBudget {
            bytes: MAXIMUM_TIMETABLE_EXPORT_BYTES - 1,
        };
        budget.check("x").unwrap();
        assert_eq!(
            budget.check("x").unwrap_err().code(),
            "APPLICATION_TIMETABLE_EXPORT_RESOURCE_LIMIT"
        );
    }

    #[test]
    fn encoded_buffer_limit_rejects_growth_before_allocating_or_publishing() {
        let mut buffer = BoundedBuffer::default();
        buffer.write_all(b"ok").unwrap();
        buffer
            .seek(SeekFrom::Start(MAXIMUM_TIMETABLE_EXPORT_BYTES as u64 - 1))
            .unwrap();
        assert_eq!(
            buffer.write(b"xx").unwrap_err().kind(),
            std::io::ErrorKind::FileTooLarge
        );
        assert!(buffer.exceeded);
        assert_eq!(buffer.cursor.into_inner(), b"ok");
    }
}
