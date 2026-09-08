//! Real OOXML ZIP fixtures for application/transport tests; not a production decoding path.

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

pub fn from_csv(sheets: &[(&str, &[u8])]) -> Vec<u8> {
    let xml_sheets = sheets.iter().map(|(name, bytes)| {
        let mut data = String::new();
        let mut reader = csv::ReaderBuilder::new().has_headers(false).from_reader(*bytes);
        for (index, record) in reader.records().enumerate() {
            let row = index + 1;
            write!(data, "<row r=\"{row}\">").unwrap();
            for (index, value) in record.unwrap().iter().enumerate() {
                assert!(index < 26, "fixture schemas fit single-letter columns");
                let column = char::from(b'A' + u8::try_from(index).unwrap());
                write!(data, "<c r=\"{column}{row}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", xml_escape(value)).unwrap();
            }
            data.push_str("</row>");
        }
        (*name, data)
    }).collect::<Vec<_>>();
    from_sheet_xml(
        &xml_sheets
            .iter()
            .map(|(name, xml)| (*name, xml.as_str()))
            .collect::<Vec<_>>(),
    )
}

pub fn from_sheet_xml(sheets: &[(&str, &str)]) -> Vec<u8> {
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
    for (index, (name, xml)) in sheets.iter().enumerate() {
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
                "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>{xml}</sheetData></worksheet>"
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
    writer.finish().unwrap().into_inner()
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
