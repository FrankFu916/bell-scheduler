#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Transaction-neutral CSV and XLSX import foundation.
//!
//! This crate does not write to persistence. Bounded XLSX decoding normalizes explicitly mapped
//! worksheets to CSV. The shared strict parser validates all supplied tables and their complete
//! cross-file graph before returning an immutable [`ImportBatch`].

mod config;
mod error;
mod model;
mod parser;
mod validate;
mod values;
mod workbook;

pub use config::{ColumnMapping, ImportConfig};
pub use error::{ImportFailure, ImportLocation, ImportProblem, ImportProblemCode};
pub use model::*;
pub use parser::{CsvDatasetSchema, CsvImporter, csv_dataset_schema};
pub use workbook::{
    MAXIMUM_WORKBOOK_BYTES, WorkbookCsvDataset, WorkbookFailure, WorkbookProblem,
    WorkbookProblemCode, WorkbookSheetMapping, XlsxWorkbook,
};
