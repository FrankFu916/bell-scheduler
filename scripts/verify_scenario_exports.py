#!/usr/bin/env python3
"""Check complete Excel/CSV exports against real saved scenario timetable queries.

The supplied database is read-only and must already contain independently adopted
native A and B results. Files are created only in a fresh output directory.
"""

import argparse
import csv
import hashlib
import io
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import uuid
import xml.etree.ElementTree as ET
import zipfile


NS = {"s": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
VIEWS = ("administrative-class", "teaching-section", "teacher", "room", "student", "subject", "grade")


def workbook_rows(path):
    with zipfile.ZipFile(path) as archive:
        assert archive.testzip() is None
        workbook = ET.fromstring(archive.read("xl/workbook.xml"))
        names = [sheet.attrib["name"] for sheet in workbook.findall("s:sheets/s:sheet", NS)]
        assert names == ["来源说明", "周课表", "课程清单"], names
        strings = ET.fromstring(archive.read("xl/sharedStrings.xml"))
        shared = ["".join(item.itertext()) for item in strings.findall("s:si", NS)]
        sheets = []
        for index in range(1, 4):
            sheet = ET.fromstring(archive.read(f"xl/worksheets/sheet{index}.xml"))
            assert not sheet.findall(".//s:f", NS), "export must never contain formulas"
            if index == 2:
                setup = sheet.find("s:pageSetup", NS)
                assert setup.attrib["orientation"] == "landscape" and setup.attrib["paperSize"] == "9"
                assert setup.attrib["fitToHeight"] == "0", "do not shrink all lessons onto one page"
                assert all(float(row.attrib["ht"]) <= 409 for row in sheet.findall("s:sheetData/s:row", NS) if "ht" in row.attrib)
            rows = []
            for row in sheet.findall("s:sheetData/s:row", NS):
                cells = []
                for cell in row.findall("s:c", NS):
                    value = cell.find("s:v", NS)
                    if cell.attrib.get("t") == "s":
                        cells.append(shared[int(value.text)])
                    elif cell.attrib.get("t") == "inlineStr":
                        cells.append("".join(cell.find("s:is", NS).itertext()))
                    else:
                        cells.append("" if value is None else value.text)
                rows.append(cells)
            sheets.append(rows)
        return sheets


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--output-parent", type=Path, default=Path("target"))
    args = parser.parse_args()
    cli = args.cli.resolve(strict=True)
    database = args.database.resolve(strict=True)
    output = Path(tempfile.mkdtemp(prefix="scenario-exports-", dir=args.output_parent.resolve(strict=True)))
    database_hash = hashlib.sha256(database.read_bytes()).hexdigest()

    def invoke(command, *arguments, expected_code=None):
        completed = subprocess.run([str(cli), command, "--database", str(database), *map(str, arguments)],
                                   capture_output=True, text=True, check=False)
        report = json.loads(completed.stdout if completed.returncode == 0 else completed.stderr)
        if expected_code is None:
            assert completed.returncode == 0, (command, report)
        else:
            assert completed.returncode == 2 and report["code"] == expected_code, (command, report)
        return report

    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as store:
        scenario_ids = [row[0] for row in store.execute("SELECT scenario_id FROM scenarios ORDER BY scenario_id")]
    assert scenario_ids, "provide a database with adopted native scenarios"
    evidence, modes = [], set()
    for scenario_id in scenario_ids:
        shown = invoke("show-scenario", "--scenario-id", scenario_id)
        assert shown["hard_valid"]
        receipt = shown["scenario"]
        modes.add(shown["has_materialized_sectioning"])
        common = ["--scenario-id", scenario_id, "--expected-scenario-revision", receipt["scenario_revision"],
                  "--expected-timetable-revision", receipt["timetable_revision"]]
        for view in VIEWS:
            options = invoke("scenario-timetable", *common, "--view", view)
            assert options["entities"]
            selection = options["entities"][0]
            selected = [*common, "--view", view, "--entity-id", selection["filter"]["id"]]
            rows, offset = [], 0
            while True:
                page = invoke("scenario-timetable", *selected, "--limit", 3, "--offset", offset)
                rows.extend(page["rows"])
                if not page["has_more"]:
                    break
                assert page["next_offset"] > offset
                offset = page["next_offset"]
            assert len(rows) == page["total_rows"]
            for format_name in ("csv", "xlsx"):
                path = output / f"{scenario_id}-{view}.{format_name}"
                exported = invoke("export-scenario", *selected, "--format", format_name, "--output", path)
                assert exported["scenario"] == receipt
                assert exported["source_is_current"] == shown["source_is_current"]
                assert exported["selection"] == selection and exported["meeting_count"] == len(rows)
                assert exported["format"] == format_name and exported["validation"] == "independently_revalidated"
                assert int(exported["byte_length"]) == path.stat().st_size
                if format_name == "csv":
                    content = path.read_bytes()
                    assert content.startswith(b"\xef\xbb\xbf") and b"\r\n" in content
                    records = list(csv.DictReader(io.StringIO(content.decode("utf-8-sig"), newline="")))
                    assert len(records) == len(rows)
                    for record, row in zip(records, rows):
                        for field in ("project_id", "source_project_revision", "source_payload_hash", "scenario_id",
                                      "scenario_revision", "scenario_payload_hash", "timetable_id", "timetable_revision", "timetable_payload_hash"):
                            assert record[field] == receipt[field], (field, record, receipt)
                        for field, expected in {"activity_id": row["activity_id"], "day": row["day_label"],
                                                "period": row["period_label"], "duration_periods": str(row["duration_periods"]),
                                                "teacher_code": row["teacher"]["code"], "room_code": row["room"]["code"],
                                                "audience_code": row["audience"]["entity"]["code"],
                                                "student_count": str(row["student_count"])}.items():
                            assert record[field] == expected, (field, record, row)
                else:
                    source_rows, grid_rows, course_rows = workbook_rows(path)
                    assert grid_rows and len(course_rows) == len(rows) + 1
                    occupied_cells = sum(1 for values in grid_rows[3:] for cell in values[1:] if cell and cell != "无课程")
                    assert occupied_cells == sum(row["duration_periods"] for row in rows), "grid hid parallel or continued lessons"
                    source_text = "\n".join("\t".join(row) for row in source_rows)
                    for field in ("scenario_id", "scenario_payload_hash", "timetable_id", "source_payload_hash"):
                        assert receipt[field] in source_text, (field, source_rows)
                    assert course_rows[0][-1] == "课次 ID"
                    for record, row in zip(course_rows[1:], rows):
                        assert record[0:3] == [row["day_label"], row["period_label"], str(row["duration_periods"])]
                        assert record[6] == row["audience"]["entity"]["code"]
                        assert record[8] == row["teacher"]["code"] and record[10] == row["room"]["code"]
                        assert record[13] == row["activity_id"]
                file_hash = hashlib.sha256(path.read_bytes()).hexdigest()
                invoke("export-scenario", *selected, "--format", format_name, "--output", path,
                       expected_code="APPLICATION_TIMETABLE_EXPORT_TARGET_EXISTS")
                assert hashlib.sha256(path.read_bytes()).hexdigest() == file_hash
            evidence.append({"scenarioId": scenario_id, "view": view, "meetings": len(rows), "formats": ["csv", "xlsx"]})
        for flag, code in (("--expected-scenario-revision", "APPLICATION_SCENARIO_REVISION_CONFLICT"),
                           ("--expected-timetable-revision", "APPLICATION_TIMETABLE_REVISION_CONFLICT")):
            stale = selected.copy()
            stale[stale.index(flag) + 1] = "9007199254740993"
            destination = output / f"{uuid.uuid4()}.xlsx"
            invoke("export-scenario", *stale, "--output", destination, expected_code=code)
            assert not destination.exists()
    assert modes == {False, True}, "fixture must exercise existing and automatically materialized sections"
    assert hashlib.sha256(database.read_bytes()).hexdigest() == database_hash
    assert len(list(output.iterdir())) == len(evidence) * 2, "unexpected temporary or partial export"
    print(json.dumps({"output": str(output), "databaseUnchanged": True, "files": len(evidence) * 2,
                      "exports": evidence}, ensure_ascii=False))


if __name__ == "__main__":
    main()
