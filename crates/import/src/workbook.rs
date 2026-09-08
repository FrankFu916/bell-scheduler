//! Bounded XLSX decoding into the existing strict CSV import contract.

use crate::{DatasetKind, ImportConfig};
use calamine::{DataRef, Reader, Xlsx};
use quick_xml::events::{BytesStart, Event};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use thiserror::Error;
use zip::{CompressionMethod, ZipArchive};

pub const MAXIMUM_WORKBOOK_BYTES: usize = 32 * 1024 * 1024;
const MAXIMUM_EXPANDED_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAXIMUM_ARCHIVE_ENTRIES: usize = 256;
const MAXIMUM_WORKSHEETS: usize = 32;
const MAXIMUM_ROWS: u32 = 100_000;
const MAXIMUM_COLUMNS: u32 = 64;
const MAXIMUM_CELLS: usize = 1_000_000;
const MAXIMUM_CELL_BYTES: usize = 128 * 1024;
const MAXIMUM_XML_DEPTH: usize = 64;

/// Explicit source-sheet identity. Column renames remain in [`ImportConfig`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkbookSheetMapping {
    pub sheet_name: String,
    pub dataset: DatasetKind,
}

/// A decoded sheet, ready to combine with CSV files in one complete import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkbookCsvDataset {
    pub sheet_name: String,
    pub dataset: DatasetKind,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkbookProblemCode {
    WorkbookInvalidArchive,
    WorkbookResourceLimit,
    WorkbookUnsupportedArchiveEntry,
    WorkbookInvalidXml,
    WorkbookDtdForbidden,
    WorkbookInvalidSheetMapping,
    WorkbookUnknownSheet,
    WorkbookDuplicateDataset,
    WorkbookInvalidWorksheet,
    WorkbookMergedCells,
    WorkbookFormulaCell,
    WorkbookErrorCell,
    WorkbookDateCell,
    WorkbookTextRequired,
    WorkbookInvalidNumber,
    WorkbookMultilineCell,
    WorkbookInvalidCellOrder,
    WorkbookInvalidCellReference,
    WorkbookMissingHeader,
}

/// Coordinates are one-based Excel row and column indices. Raw values are never retained.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookProblem {
    pub code: WorkbookProblemCode,
    pub sheet_name: Option<String>,
    pub row: Option<u32>,
    pub column: Option<u32>,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("XLSX import failed with {count} problem(s)", count = .problems.len())]
pub struct WorkbookFailure {
    problems: Vec<WorkbookProblem>,
}

impl WorkbookFailure {
    #[must_use]
    pub fn problems(&self) -> &[WorkbookProblem] {
        &self.problems
    }

    fn new(code: WorkbookProblemCode) -> Self {
        Self {
            problems: vec![WorkbookProblem {
                code,
                sheet_name: None,
                row: None,
                column: None,
            }],
        }
    }

    fn sheet(mut self, name: &str) -> Self {
        self.problems[0].sheet_name = Some(name.to_owned());
        self
    }

    fn cell(mut self, position: (u32, u32)) -> Self {
        self.problems[0].row = position.0.checked_add(1);
        self.problems[0].column = position.1.checked_add(1);
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct XlsxWorkbook;

impl XlsxWorkbook {
    /// Lists source sheet names for an explicit mapping UI without interpreting business data.
    ///
    /// # Errors
    ///
    /// Applies the same archive, XML and worksheet-count limits as [`Self::read`]. A successful
    /// listing is not an audit; [`Self::read`] independently validates the complete input again.
    pub fn sheet_names(bytes: &[u8]) -> Result<Vec<String>, WorkbookFailure> {
        Ok(open_workbook(bytes)?.sheet_names())
    }

    /// Decodes all workbook sheets without writing state or validating cross-table references.
    ///
    /// Empty mappings accept only canonical dataset names. Explicit mappings override those
    /// names, but every sheet must be resolved, and one dataset cannot occur twice. The first
    /// row is the header; textual codes remain text so Excel's numeric formatting cannot erase
    /// significant zeroes. Formula caches are never treated as authoritative imported values.
    ///
    /// # Errors
    ///
    /// Returns a stable, located error for malformed, ambiguous or resource-exceeding input.
    /// Call the existing CSV importer with *all* decoded sheets and additional CSV sources to
    /// perform schema, value and complete cross-table validation before any write transaction.
    pub fn read(
        bytes: &[u8],
        config: &ImportConfig,
        mappings: &[WorkbookSheetMapping],
    ) -> Result<Vec<WorkbookCsvDataset>, WorkbookFailure> {
        let mut workbook = open_workbook(bytes)?;
        let names = workbook.sheet_names();
        let resolved = resolve_sheets(&names, mappings)?;
        let mut datasets = Vec::with_capacity(resolved.len());
        let mut cell_count = 0;
        let mut output_bytes = 0;
        for (name, dataset) in resolved {
            let bytes = decode_sheet(&mut workbook, &name, dataset, config, &mut cell_count)
                .map_err(|error| error.sheet(&name))?;
            output_bytes += bytes.len();
            if output_bytes > MAXIMUM_WORKBOOK_BYTES {
                return Err(WorkbookFailure::new(
                    WorkbookProblemCode::WorkbookResourceLimit,
                ));
            }
            datasets.push(WorkbookCsvDataset {
                sheet_name: name,
                dataset,
                bytes,
            });
        }
        Ok(datasets)
    }
}

fn open_workbook(bytes: &[u8]) -> Result<Xlsx<Cursor<&[u8]>>, WorkbookFailure> {
    preflight_archive(bytes)?;
    let workbook = Xlsx::new(Cursor::new(bytes))
        .map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidArchive))?;
    let names = workbook.sheet_names();
    if names.is_empty() || names.len() > MAXIMUM_WORKSHEETS {
        return Err(WorkbookFailure::new(
            WorkbookProblemCode::WorkbookResourceLimit,
        ));
    }
    Ok(workbook)
}

fn preflight_archive(bytes: &[u8]) -> Result<(), WorkbookFailure> {
    if bytes.len() > MAXIMUM_WORKBOOK_BYTES {
        return Err(WorkbookFailure::new(
            WorkbookProblemCode::WorkbookResourceLimit,
        ));
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidArchive))?;
    if archive.len() > MAXIMUM_ARCHIVE_ENTRIES {
        return Err(WorkbookFailure::new(
            WorkbookProblemCode::WorkbookResourceLimit,
        ));
    }
    let mut expanded = 0_u64;
    let mut names = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidArchive))?;
        let name = entry.name().to_owned();
        if entry.encrypted()
            || entry.is_symlink()
            || entry.enclosed_name().is_none()
            || !names.insert(name.to_ascii_lowercase())
            || !matches!(
                entry.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
            || name.contains('\\')
        {
            return Err(WorkbookFailure::new(
                WorkbookProblemCode::WorkbookUnsupportedArchiveEntry,
            ));
        }
        let declared_size = entry.size();
        expanded = expanded.saturating_add(declared_size);
        if declared_size > MAXIMUM_ENTRY_BYTES || expanded > MAXIMUM_EXPANDED_BYTES {
            return Err(WorkbookFailure::new(
                WorkbookProblemCode::WorkbookResourceLimit,
            ));
        }
        let mut contents = Vec::new();
        entry
            .take(MAXIMUM_ENTRY_BYTES + 1)
            .read_to_end(&mut contents)
            .map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidArchive))?;
        if contents.len() as u64 > MAXIMUM_ENTRY_BYTES {
            return Err(WorkbookFailure::new(
                WorkbookProblemCode::WorkbookResourceLimit,
            ));
        }
        if contents.len() as u64 != declared_size {
            return Err(WorkbookFailure::new(
                WorkbookProblemCode::WorkbookInvalidArchive,
            ));
        }
        if std::path::Path::new(&name)
            .extension()
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("xml") || extension.eq_ignore_ascii_case("rels")
            })
        {
            preflight_xml(&contents)?;
        }
    }
    Ok(())
}

fn preflight_xml(bytes: &[u8]) -> Result<(), WorkbookFailure> {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut depth = 0_usize;
    loop {
        match reader.read_event() {
            Ok(Event::DocType(_)) => {
                return Err(WorkbookFailure::new(
                    WorkbookProblemCode::WorkbookDtdForbidden,
                ));
            }
            Ok(Event::Start(element)) => {
                guard_coordinates(&element)?;
                depth += 1;
                if depth > MAXIMUM_XML_DEPTH {
                    return Err(WorkbookFailure::new(
                        WorkbookProblemCode::WorkbookResourceLimit,
                    ));
                }
            }
            Ok(Event::Empty(element)) => guard_coordinates(&element)?,
            Ok(Event::End(_)) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidXml))?;
            }
            Ok(Event::Eof) if depth == 0 => return Ok(()),
            Ok(Event::Eof) | Err(_) => {
                return Err(WorkbookFailure::new(
                    WorkbookProblemCode::WorkbookInvalidXml,
                ));
            }
            Ok(_) => (),
        }
    }
}

// Calamine parses coordinates using u32 arithmetic. Validate the narrow coordinate attributes
// before invoking it, including backwards ranges, so malformed XML cannot panic or wrap there.
fn guard_coordinates(element: &BytesStart<'_>) -> Result<(), WorkbookFailure> {
    let name = element.local_name();
    let reference_attribute = match name.as_ref() {
        b"c" | b"row" => b"r".as_slice(),
        b"dimension" | b"mergeCell" | b"f" => b"ref".as_slice(),
        _ => return Ok(()),
    };
    for attribute in element.attributes() {
        let attribute =
            attribute.map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidXml))?;
        if attribute.key.as_ref() != reference_attribute {
            continue;
        }
        if name.as_ref() == b"row" {
            let row = positive_decimal(&attribute.value)?;
            check_position((row - 1, 0))?;
            continue;
        }
        let mut parts = attribute.value.split(|byte| *byte == b':');
        let start = checked_coordinate(parts.next().unwrap_or_default())?;
        if let Some(end) = parts.next() {
            let end = checked_coordinate(end)?;
            if name.as_ref() == b"c" || start.0 > end.0 || start.1 > end.1 || parts.next().is_some()
            {
                return Err(WorkbookFailure::new(
                    WorkbookProblemCode::WorkbookInvalidCellReference,
                ));
            }
        }
    }
    Ok(())
}

fn checked_coordinate(reference: &[u8]) -> Result<(u32, u32), WorkbookFailure> {
    let split = reference
        .iter()
        .position(u8::is_ascii_digit)
        .ok_or_else(|| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidCellReference))?;
    let mut column = 0_u32;
    for byte in &reference[..split] {
        if !byte.is_ascii_alphabetic() {
            return Err(WorkbookFailure::new(
                WorkbookProblemCode::WorkbookInvalidCellReference,
            ));
        }
        column = column
            .checked_mul(26)
            .and_then(|value| value.checked_add(u32::from(byte.to_ascii_uppercase() - b'A') + 1))
            .ok_or_else(|| {
                WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidCellReference)
            })?;
    }
    let column = column
        .checked_sub(1)
        .ok_or_else(|| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidCellReference))?;
    let row = positive_decimal(&reference[split..])? - 1;
    check_position((row, column))?;
    Ok((row, column))
}

fn positive_decimal(value: &[u8]) -> Result<u32, WorkbookFailure> {
    let mut result = 0_u32;
    for byte in value {
        if !byte.is_ascii_digit() {
            return Err(WorkbookFailure::new(
                WorkbookProblemCode::WorkbookInvalidCellReference,
            ));
        }
        result = result
            .checked_mul(10)
            .and_then(|number| number.checked_add(u32::from(byte - b'0')))
            .ok_or_else(|| {
                WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidCellReference)
            })?;
    }
    if result == 0 {
        Err(WorkbookFailure::new(
            WorkbookProblemCode::WorkbookInvalidCellReference,
        ))
    } else {
        Ok(result)
    }
}

fn resolve_sheets(
    names: &[String],
    mappings: &[WorkbookSheetMapping],
) -> Result<Vec<(String, DatasetKind)>, WorkbookFailure> {
    let mut explicit = BTreeMap::new();
    for mapping in mappings {
        if !names.contains(&mapping.sheet_name)
            || explicit
                .insert(mapping.sheet_name.as_str(), mapping.dataset)
                .is_some()
        {
            return Err(
                WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidSheetMapping)
                    .sheet(&mapping.sheet_name),
            );
        }
    }
    let mut seen = BTreeSet::new();
    let mut result = Vec::with_capacity(names.len());
    for name in names {
        let dataset = explicit
            .get(name.as_str())
            .copied()
            .or_else(|| {
                DatasetKind::ALL
                    .into_iter()
                    .find(|kind| kind.as_str() == name)
            })
            .ok_or_else(|| {
                WorkbookFailure::new(WorkbookProblemCode::WorkbookUnknownSheet).sheet(name)
            })?;
        if !seen.insert(dataset) {
            return Err(
                WorkbookFailure::new(WorkbookProblemCode::WorkbookDuplicateDataset).sheet(name),
            );
        }
        result.push((name.clone(), dataset));
    }
    Ok(result)
}

fn decode_sheet(
    workbook: &mut Xlsx<Cursor<&[u8]>>,
    name: &str,
    dataset: DatasetKind,
    config: &ImportConfig,
    cell_count: &mut usize,
) -> Result<Vec<u8>, WorkbookFailure> {
    let merges = workbook
        .merge_cells_by_sheet_name(name)
        .map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidWorksheet))?;
    if let Some(merge) = merges.first() {
        return Err(
            WorkbookFailure::new(WorkbookProblemCode::WorkbookMergedCells).cell(merge.start),
        );
    }
    let mut reader = workbook
        .worksheet_cells_reader(name)
        .map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidWorksheet))?;
    let dimensions = reader.dimensions();
    check_position(dimensions.end)?;
    let mut previous_position = None;
    let mut rows: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let mut headers: Vec<String> = Vec::new();
    let mut text_bytes = 0_usize;
    while let Some(cell) = reader
        .next_cell_with_formula_metadata()
        .map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidWorksheet))?
    {
        check_position(cell.pos)?;
        *cell_count += 1;
        if *cell_count > MAXIMUM_CELLS {
            return Err(
                WorkbookFailure::new(WorkbookProblemCode::WorkbookResourceLimit).cell(cell.pos),
            );
        }
        if previous_position.is_some_and(|previous| previous >= cell.pos) {
            return Err(
                WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidCellOrder).cell(cell.pos),
            );
        }
        previous_position = Some(cell.pos);
        if cell.formula.is_some() {
            return Err(
                WorkbookFailure::new(WorkbookProblemCode::WorkbookFormulaCell).cell(cell.pos),
            );
        }
        if matches!(cell.value, DataRef::Empty) {
            continue;
        }
        let column = usize::try_from(cell.pos.1).expect("bounded column fits usize");
        let field = headers.get(column).map_or("", |value| value.trim());
        let canonical = config
            .mapping(dataset)
            .map_or(field, |mapping| mapping.resolve(field));
        let value = cell_text(cell.value, canonical, cell.pos.0 == 0)
            .map_err(|error| error.cell(cell.pos))?;
        text_bytes += value.len();
        if value.len() > MAXIMUM_CELL_BYTES || text_bytes > MAXIMUM_WORKBOOK_BYTES {
            return Err(
                WorkbookFailure::new(WorkbookProblemCode::WorkbookResourceLimit).cell(cell.pos),
            );
        }
        let row = rows.entry(cell.pos.0).or_default();
        row.resize(column + 1, String::new());
        row[column] = value;
        if cell.pos.0 == 0 {
            headers.clone_from(row);
        }
    }
    encode_rows(rows)
}

fn check_position(position: (u32, u32)) -> Result<(), WorkbookFailure> {
    if position.0 >= MAXIMUM_ROWS || position.1 >= MAXIMUM_COLUMNS {
        Err(WorkbookFailure::new(WorkbookProblemCode::WorkbookResourceLimit).cell(position))
    } else {
        Ok(())
    }
}

fn cell_text(value: DataRef<'_>, field: &str, header: bool) -> Result<String, WorkbookFailure> {
    let text = match value {
        DataRef::String(value) => value,
        DataRef::SharedString(value) => value.to_owned(),
        DataRef::Empty => String::new(),
        DataRef::Error(_) => {
            return Err(WorkbookFailure::new(WorkbookProblemCode::WorkbookErrorCell));
        }
        DataRef::DateTime(_) | DataRef::DateTimeIso(_) | DataRef::DurationIso(_) => {
            return Err(WorkbookFailure::new(WorkbookProblemCode::WorkbookDateCell));
        }
        DataRef::Bool(value) if !header && field == "may_cross_breaks" => value.to_string(),
        DataRef::Int(value) if !header && numeric_field(field) && value >= 0 => value.to_string(),
        DataRef::Float(value) if !header && numeric_field(field) => {
            if !value.is_finite()
                || value < 0.0
                || value > f64::from(u16::MAX)
                || value.fract() != 0.0
            {
                return Err(WorkbookFailure::new(
                    WorkbookProblemCode::WorkbookInvalidNumber,
                ));
            }
            format!("{value:.0}")
        }
        DataRef::Int(_) | DataRef::Float(_) | DataRef::Bool(_) => {
            return Err(WorkbookFailure::new(
                WorkbookProblemCode::WorkbookTextRequired,
            ));
        }
    };
    if text.contains(['\r', '\n']) {
        return Err(WorkbookFailure::new(
            WorkbookProblemCode::WorkbookMultilineCell,
        ));
    }
    Ok(text)
}

fn numeric_field(field: &str) -> bool {
    matches!(
        field,
        "capacity"
            | "period"
            | "weekly_periods"
            | "min_days_between"
            | "max_periods_per_day"
            | "min_size"
            | "target_size"
            | "max_size"
            | "meeting_ordinal"
            | "duration"
    )
}

fn encode_rows(mut rows: BTreeMap<u32, Vec<String>>) -> Result<Vec<u8>, WorkbookFailure> {
    let Some(headers) = rows.get(&0) else {
        return Err(WorkbookFailure::new(WorkbookProblemCode::WorkbookMissingHeader).cell((0, 0)));
    };
    let width = headers.len();
    let mut writer = csv::WriterBuilder::new()
        .flexible(true)
        .from_writer(Vec::new());
    let last_row = rows.last_key_value().map_or(0, |(row, _)| *row);
    for index in 0..=last_row {
        let mut row = rows.remove(&index).unwrap_or_default();
        // A truly empty record becomes a blank physical CSV line, preserving Excel row numbers.
        if !row.is_empty() {
            row.resize(row.len().max(width), String::new());
        }
        writer
            .write_record(row)
            .map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidWorksheet))?;
        if writer.get_ref().len() > MAXIMUM_WORKBOOK_BYTES {
            return Err(WorkbookFailure::new(
                WorkbookProblemCode::WorkbookResourceLimit,
            ));
        }
    }
    writer
        .into_inner()
        .map_err(|_| WorkbookFailure::new(WorkbookProblemCode::WorkbookInvalidWorksheet))
}
