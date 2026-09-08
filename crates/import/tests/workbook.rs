use class_schedule_import::{
    ColumnMapping, CsvImporter, CsvSource, DatasetKind, ImportConfig, ImportProblemCode,
    WorkbookFailure, WorkbookProblemCode, WorkbookSheetMapping, XlsxWorkbook,
};
use std::fmt::Write as _;
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn text_cell(reference: &str, value: &str) -> String {
    format!(
        "<c r=\"{reference}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
        xml_escape(value)
    )
}

fn teacher_data(last_cell: &str) -> String {
    format!(
        "<row r=\"1\">{}{}</row><row r=\"2\">{}{last_cell}</row>",
        text_cell("A1", "teacher_code"),
        text_cell("B1", "name"),
        text_cell("A2", "001")
    )
}

fn write_entry(writer: &mut ZipWriter<Cursor<Vec<u8>>>, name: &str, contents: &str) {
    writer
        .start_file(
            name,
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
    writer.write_all(contents.as_bytes()).unwrap();
}

/// A real OOXML ZIP container. Only the test data is constructed here; production parsing is Calamine.
fn workbook(sheets: &[(&str, String, &str)], extra: Option<(&str, &str)>) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let mut content_types = String::from(
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>",
    );
    let mut workbook_xml = String::from(
        "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>",
    );
    let mut relationships = String::from(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for (index, (name, data, tail)) in sheets.iter().enumerate() {
        let id = index + 1;
        write!(content_types, "<Override PartName=\"/xl/worksheets/sheet{id}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>").unwrap();
        write!(
            workbook_xml,
            "<sheet name=\"{}\" sheetId=\"{id}\" r:id=\"rId{id}\"/>",
            xml_escape(name)
        )
        .unwrap();
        write!(relationships, "<Relationship Id=\"rId{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{id}.xml\"/>").unwrap();
        write_entry(
            &mut writer,
            &format!("xl/worksheets/sheet{id}.xml"),
            &format!(
                "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>{data}</sheetData>{tail}</worksheet>"
            ),
        );
    }
    content_types.push_str("</Types>");
    workbook_xml.push_str("</sheets></workbook>");
    relationships.push_str("</Relationships>");
    write_entry(&mut writer, "[Content_Types].xml", &content_types);
    write_entry(
        &mut writer,
        "_rels/.rels",
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>",
    );
    write_entry(&mut writer, "xl/workbook.xml", &workbook_xml);
    write_entry(&mut writer, "xl/_rels/workbook.xml.rels", &relationships);
    write_entry(
        &mut writer,
        "xl/styles.xml",
        "<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><cellXfs count=\"2\"><xf numFmtId=\"0\"/><xf numFmtId=\"14\"/></cellXfs></styleSheet>",
    );
    if let Some((name, contents)) = extra {
        write_entry(&mut writer, name, contents);
    }
    writer.finish().unwrap().into_inner()
}

fn isolated_config() -> ImportConfig {
    ImportConfig::default()
        .with_required_core_datasets(false)
        .with_exact_subject_choices(None)
}

fn expect_code(failure: &WorkbookFailure, code: WorkbookProblemCode) {
    assert_eq!(failure.problems()[0].code, code);
}

#[test]
fn workbook_preserves_text_codes_quotes_unicode_and_strict_csv_semantics() {
    let config = isolated_config();
    let bytes = workbook(
        &[(
            "teachers",
            teacher_data(&text_cell("B2", "王老师,\"数学\"")),
            "",
        )],
        None,
    );
    let decoded = XlsxWorkbook::read(&bytes, &config, &[]).unwrap();
    let imported = CsvImporter::new(config)
        .import(
            decoded
                .iter()
                .map(|sheet| CsvSource::new(sheet.dataset, &sheet.bytes)),
        )
        .unwrap();
    assert_eq!(imported.teachers()[0].teacher_code, "001");
    assert_eq!(imported.teachers()[0].name, "王老师,\"数学\"");
    assert_eq!(decoded[0].sheet_name, "teachers");
}

#[test]
fn workbook_accepts_explicit_sheet_and_column_mapping_with_numeric_fields() {
    let config = isolated_config().with_mapping(
        DatasetKind::Rooms,
        ColumnMapping::new([
            ("教室编号", "room_code"),
            ("教室名称", "name"),
            ("楼栋", "building_code"),
            ("容量", "capacity"),
        ]),
    );
    let data = format!(
        "<row r=\"1\">{}{}{}{}</row><row r=\"2\">{}{}{}<c r=\"D2\"><v>40</v></c></row>",
        text_cell("A1", "教室编号"),
        text_cell("B1", "教室名称"),
        text_cell("C1", "楼栋"),
        text_cell("D1", "容量"),
        text_cell("A2", "0001"),
        text_cell("B2", "一楼教室"),
        text_cell("C2", "主楼")
    );
    let bytes = workbook(&[("教室", data, "")], None);
    let decoded = XlsxWorkbook::read(
        &bytes,
        &config,
        &[WorkbookSheetMapping {
            sheet_name: "教室".into(),
            dataset: DatasetKind::Rooms,
        }],
    )
    .unwrap();
    let batch = CsvImporter::new(config)
        .import([CsvSource::new(decoded[0].dataset, &decoded[0].bytes)])
        .unwrap();
    assert_eq!(batch.rooms()[0].room_code, "0001");
    assert_eq!(batch.rooms()[0].capacity, 40);
}

#[test]
fn workbook_rejects_unmapped_unknown_duplicate_and_stale_mappings() {
    let config = isolated_config();
    let data = teacher_data(&text_cell("B2", "Teacher"));
    let unknown = workbook(&[("Sheet1", data.clone(), "")], None);
    expect_code(
        &XlsxWorkbook::read(&unknown, &config, &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookUnknownSheet,
    );
    let valid = workbook(&[("teachers", data.clone(), "")], None);
    let missing = [WorkbookSheetMapping {
        sheet_name: "missing".into(),
        dataset: DatasetKind::Teachers,
    }];
    expect_code(
        &XlsxWorkbook::read(&valid, &config, &missing).unwrap_err(),
        WorkbookProblemCode::WorkbookInvalidSheetMapping,
    );
    let duplicate = workbook(&[("teachers", data.clone(), ""), ("staff", data, "")], None);
    let mapping = [WorkbookSheetMapping {
        sheet_name: "staff".into(),
        dataset: DatasetKind::Teachers,
    }];
    expect_code(
        &XlsxWorkbook::read(&duplicate, &config, &mapping).unwrap_err(),
        WorkbookProblemCode::WorkbookDuplicateDataset,
    );
}

#[test]
fn workbook_rejects_formula_cache_shared_formula_errors_dates_and_multiline_with_location() {
    let config = isolated_config();
    for (cell, expected) in [
        (
            "<c r=\"B2\"><f>1+1</f><v>2</v></c>",
            WorkbookProblemCode::WorkbookFormulaCell,
        ),
        (
            "<c r=\"B2\"><f t=\"shared\" si=\"4294967295\"/><v>2</v></c>",
            WorkbookProblemCode::WorkbookFormulaCell,
        ),
        (
            "<c r=\"B2\" t=\"e\"><v>#VALUE!</v></c>",
            WorkbookProblemCode::WorkbookErrorCell,
        ),
        (
            "<c r=\"B2\" s=\"1\"><v>46000</v></c>",
            WorkbookProblemCode::WorkbookDateCell,
        ),
        (
            "<c r=\"B2\" t=\"d\"><v>2026-09-07</v></c>",
            WorkbookProblemCode::WorkbookDateCell,
        ),
        (
            "<c r=\"B2\" t=\"inlineStr\"><is><t>first&#10;second</t></is></c>",
            WorkbookProblemCode::WorkbookMultilineCell,
        ),
    ] {
        let bytes = workbook(&[("teachers", teacher_data(cell), "")], None);
        let error = XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err();
        expect_code(&error, expected);
        assert_eq!(error.problems()[0].sheet_name.as_deref(), Some("teachers"));
        assert_eq!(error.problems()[0].row, Some(2));
        assert_eq!(error.problems()[0].column, Some(2));
        let json = serde_json::to_string(error.problems()).unwrap();
        assert!(!json.contains("46000"));
        assert!(!json.contains("VALUE!"));
    }
}

#[test]
fn workbook_rejects_numeric_codes_and_fractional_counts() {
    let config = isolated_config();
    let data = format!(
        "<row r=\"1\">{}{}</row><row r=\"2\"><c r=\"A2\"><v>1</v></c>{}</row>",
        text_cell("A1", "teacher_code"),
        text_cell("B1", "name"),
        text_cell("B2", "Teacher")
    );
    let bytes = workbook(&[("teachers", data, "")], None);
    expect_code(
        &XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookTextRequired,
    );
    let data = format!(
        "<row r=\"1\">{}</row><row r=\"2\"><c r=\"A2\"><v>40.5</v></c></row>",
        text_cell("A1", "capacity")
    );
    let bytes = workbook(&[("rooms", data, "")], None);
    expect_code(
        &XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookInvalidNumber,
    );
}

#[test]
fn workbook_preserves_excel_row_number_after_blank_rows_and_combines_with_csv() {
    let config = isolated_config();
    let data = format!(
        "<row r=\"1\">{}{}</row><row r=\"3\">{}{}</row><row r=\"4\">{}{}</row>",
        text_cell("A1", "teacher_code"),
        text_cell("B1", "name"),
        text_cell("A3", "T1"),
        text_cell("B3", "Teacher"),
        text_cell("A4", "T1"),
        text_cell("B4", "Duplicate")
    );
    let bytes = workbook(&[("teachers", data, "")], None);
    let decoded = XlsxWorkbook::read(&bytes, &config, &[]).unwrap();
    let failure = CsvImporter::new(config)
        .import([
            CsvSource::new(decoded[0].dataset, &decoded[0].bytes),
            CsvSource::new(
                DatasetKind::TeacherUnavailability,
                b"teacher_code,day,period\nmissing,Monday,1\n",
            ),
        ])
        .unwrap_err();
    let duplicate = failure
        .problems()
        .iter()
        .find(|problem| problem.code() == ImportProblemCode::ImportDuplicateExternalCode)
        .unwrap();
    assert_eq!(duplicate.location().row_number(), Some(4));
    assert_eq!(duplicate.related_row(), Some(3));
    assert!(
        failure
            .problems()
            .iter()
            .any(|problem| problem.code() == ImportProblemCode::ImportMissingReference)
    );
}

#[test]
fn workbook_rejects_merged_sparse_duplicate_cells_and_missing_header() {
    let config = isolated_config();
    let bytes = workbook(
        &[(
            "teachers",
            teacher_data(&text_cell("B2", "Teacher")),
            "<mergeCells count=\"1\"><mergeCell ref=\"A2:B2\"/></mergeCells>",
        )],
        None,
    );
    expect_code(
        &XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookMergedCells,
    );
    let bytes = workbook(
        &[(
            "teachers",
            format!(
                "<row r=\"1\">{}</row><row r=\"1048576\">{}</row>",
                text_cell("A1", "teacher_code"),
                text_cell("XFD1048576", "sparse")
            ),
            "",
        )],
        None,
    );
    expect_code(
        &XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookResourceLimit,
    );
    let bytes = workbook(
        &[(
            "teachers",
            format!(
                "<row r=\"1\">{}{}</row>",
                text_cell("A1", "teacher_code"),
                text_cell("A1", "name")
            ),
            "",
        )],
        None,
    );
    expect_code(
        &XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookInvalidCellOrder,
    );
    let bytes = workbook(
        &[(
            "teachers",
            format!("<row r=\"2\">{}</row>", text_cell("A2", "teacher_code")),
            "",
        )],
        None,
    );
    expect_code(
        &XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookMissingHeader,
    );
}

#[test]
fn workbook_rejects_invalid_zip_xml_dtd_and_oversized_expansion() {
    let config = isolated_config();
    expect_code(
        &XlsxWorkbook::read(b"not a ZIP", &config, &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookInvalidArchive,
    );
    for (extra, expected) in [
        (
            "<!DOCTYPE document [<!ENTITY x 'expanded'>]><document/>",
            WorkbookProblemCode::WorkbookDtdForbidden,
        ),
        ("<root><unclosed>", WorkbookProblemCode::WorkbookInvalidXml),
    ] {
        let bytes = workbook(
            &[("teachers", teacher_data(&text_cell("B2", "Teacher")), "")],
            Some(("extra.xml", extra)),
        );
        expect_code(
            &XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err(),
            expected,
        );
    }
    let expanded = " ".repeat(16 * 1024 * 1024 + 1);
    let bytes = workbook(
        &[("teachers", teacher_data(&text_cell("B2", "Teacher")), "")],
        Some(("padding.txt", &expanded)),
    );
    assert!(bytes.len() < 1024 * 1024);
    expect_code(
        &XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookResourceLimit,
    );
}

#[test]
fn workbook_rejects_unsafe_paths_and_case_aliases() {
    let config = isolated_config();
    for entry in ["../outside.xml", "XL/WORKBOOK.XML"] {
        let bytes = workbook(
            &[("teachers", teacher_data(&text_cell("B2", "Teacher")), "")],
            Some((entry, "<root/>")),
        );
        expect_code(
            &XlsxWorkbook::read(&bytes, &config, &[]).unwrap_err(),
            WorkbookProblemCode::WorkbookUnsupportedArchiveEntry,
        );
    }
}

#[test]
fn workbook_metadata_is_bounded_and_never_substitutes_for_import_validation() {
    let data = teacher_data("<c r=\"B2\"><f>1+1</f><v>2</v></c>");
    let bytes = workbook(&[("任意工作表", data, "")], None);
    assert_eq!(XlsxWorkbook::sheet_names(&bytes).unwrap(), ["任意工作表"]);
    let mapping = [WorkbookSheetMapping {
        sheet_name: "任意工作表".into(),
        dataset: DatasetKind::Teachers,
    }];
    expect_code(
        &XlsxWorkbook::read(&bytes, &isolated_config(), &mapping).unwrap_err(),
        WorkbookProblemCode::WorkbookFormulaCell,
    );
    let oversized = vec![0; class_schedule_import::MAXIMUM_WORKBOOK_BYTES + 1];
    expect_code(
        &XlsxWorkbook::sheet_names(&oversized).unwrap_err(),
        WorkbookProblemCode::WorkbookResourceLimit,
    );
    let names: Vec<_> = (0..33).map(|index| format!("sheet{index}")).collect();
    let sheets: Vec<_> = names
        .iter()
        .map(|name| (name.as_str(), String::new(), ""))
        .collect();
    let bytes = workbook(&sheets, None);
    expect_code(
        &XlsxWorkbook::sheet_names(&bytes).unwrap_err(),
        WorkbookProblemCode::WorkbookResourceLimit,
    );
}

#[test]
fn workbook_rejects_deep_xml_and_oversized_cell_text_before_csv_import() {
    let deep_xml = format!("{}{}", "<nested>".repeat(65), "</nested>".repeat(65));
    let bytes = workbook(
        &[("teachers", teacher_data(&text_cell("B2", "Teacher")), "")],
        Some(("deep.XML", &deep_xml)),
    );
    expect_code(
        &XlsxWorkbook::sheet_names(&bytes).unwrap_err(),
        WorkbookProblemCode::WorkbookResourceLimit,
    );
    let large_text = "a".repeat(128 * 1024 + 1);
    let bytes = workbook(
        &[("teachers", teacher_data(&text_cell("B2", &large_text)), "")],
        None,
    );
    expect_code(
        &XlsxWorkbook::read(&bytes, &isolated_config(), &[]).unwrap_err(),
        WorkbookProblemCode::WorkbookResourceLimit,
    );
}

#[test]
fn workbook_guards_overflowing_coordinates_and_backwards_ranges_before_calamine() {
    for reference in [
        "ZZZZZZZZZZZZZZ1",
        "A9999999999999999999",
        "A0",
        "1",
        "A1:B1",
    ] {
        let data = format!(
            "<row r=\"1\">{}</row>",
            text_cell(reference, "teacher_code")
        );
        let bytes = workbook(&[("teachers", data, "")], None);
        expect_code(
            &XlsxWorkbook::read(&bytes, &isolated_config(), &[]).unwrap_err(),
            WorkbookProblemCode::WorkbookInvalidCellReference,
        );
    }
    for tail in [
        "<mergeCells><mergeCell ref=\"B2:A1\"/></mergeCells>",
        "<dimension ref=\"B2:A1\"/>",
    ] {
        let bytes = workbook(
            &[("teachers", teacher_data(&text_cell("B2", "Teacher")), tail)],
            None,
        );
        expect_code(
            &XlsxWorkbook::read(&bytes, &isolated_config(), &[]).unwrap_err(),
            WorkbookProblemCode::WorkbookInvalidCellReference,
        );
    }
}

#[test]
fn workbook_small_fixture_is_identical_to_existing_csv_import() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/small");
    let mut csv_files = Vec::new();
    let mut sheets = Vec::new();
    for dataset in DatasetKind::ALL {
        let path = root.join(format!("{}.csv", dataset.as_str()));
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(path).unwrap();
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(bytes.as_slice());
        let mut data = String::new();
        for (row_index, record) in reader.records().enumerate() {
            let row = row_index + 1;
            write!(data, "<row r=\"{row}\">").unwrap();
            for (column, value) in record.unwrap().iter().enumerate() {
                // Current strict fixture schemas have fewer than 26 columns.
                let letter = char::from(b'A' + u8::try_from(column).unwrap());
                data.push_str(&text_cell(&format!("{letter}{row}"), value));
            }
            data.push_str("</row>");
        }
        sheets.push((dataset.as_str(), data, ""));
        csv_files.push((dataset, bytes));
    }
    let config = ImportConfig::default();
    let bytes = workbook(&sheets, None);
    let decoded = XlsxWorkbook::read(&bytes, &config, &[]).unwrap();
    let importer = CsvImporter::new(config);
    let from_csv = importer
        .import(
            csv_files
                .iter()
                .map(|(kind, bytes)| CsvSource::new(*kind, bytes)),
        )
        .unwrap();
    let from_xlsx = importer
        .import(
            decoded
                .iter()
                .map(|sheet| CsvSource::new(sheet.dataset, &sheet.bytes)),
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(from_csv).unwrap(),
        serde_json::to_value(from_xlsx).unwrap()
    );
}
