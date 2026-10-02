#!/usr/bin/env python3
"""Exercise scenario edits against a backup of saved native A/B solver results.

The supplied database is opened read-only and copied with SQLite's backup API.
Every edit, independent clone and export is created in a fresh output directory.
No worker is launched: each CLI command reloads and revalidates the saved result.
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

from verify_scenario_exports import VIEWS, workbook_rows


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def common(receipt):
    return ["--scenario-id", receipt["scenario_id"],
            "--expected-scenario-revision", receipt["scenario_revision"],
            "--expected-timetable-revision", receipt["timetable_revision"]]


def move(activity, slot, locked=False):
    operation = ["move", "--activity-id", activity, "--start-timeslot-id", slot]
    return operation + (["--lock-after"] if locked else [])


def assignment_map(timetable):
    return {meeting["demand_id"]: meeting["assignment"] for meeting in timetable["meetings"]}


class Checks:
    def __init__(self, cli, database, output):
        self.cli, self.database, self.output = cli, database, output
        self.invocations = 0
        self.commits = 0
        self.clones = 0
        self.exports = 0

    def invoke(self, command, *arguments, error=None, readonly=False):
        before = digest(self.database) if readonly else None
        arguments = [str(self.cli), command, "--database", str(self.database), *map(str, arguments)]
        completed = subprocess.run(arguments, capture_output=True, text=True, check=False, timeout=120)
        self.invocations += 1
        with (self.output / "commands.jsonl").open("a", encoding="utf-8") as trace:
            trace.write(json.dumps({"arguments": arguments, "exitCode": completed.returncode,
                                    "stdout": completed.stdout, "stderr": completed.stderr}, ensure_ascii=False) + "\n")
        try:
            report = json.loads(completed.stdout if completed.returncode == 0 else completed.stderr)
        except json.JSONDecodeError as failure:
            raise AssertionError((command, completed.returncode, completed.stdout, completed.stderr)) from failure
        if error is None:
            assert completed.returncode == 0, (command, report)
        else:
            assert completed.returncode == 2 and report["code"] == error, (command, report, error)
        if readonly:
            assert digest(self.database) == before, (command, "unexpected database change")
        return report

    def show(self, receipt):
        shown = self.invoke("show-scenario", "--scenario-id", receipt["scenario_id"], readonly=True)
        assert shown["scenario"] == receipt and shown["hard_valid"]
        assert shown["validation"] == "independently_revalidated"
        return shown

    def preview(self, receipt, operation):
        result = self.invoke("preview-scenario-edit", *common(receipt), *operation, readonly=True)
        assert result["scenario"] == receipt and result["source_is_current"]
        problems = result["validation"]["hard_problems"]
        if result["status"] == "hard_rejected":
            assert problems and result["after_quality"] is None and not result["can_commit"]
        else:
            assert result["status"] in ("valid", "no_change") and not problems
            assert result["after_quality"] is not None
            assert result["can_commit"] == (result["status"] == "valid")
        return result

    def commit(self, receipt, operation, status="committed", error=None):
        report = self.invoke(
            "commit-scenario-edit", *common(receipt),
            "--expected-scenario-payload-hash", receipt["scenario_payload_hash"],
            "--expected-timetable-payload-hash", receipt["timetable_payload_hash"],
            *operation, error=error, readonly=error is not None or status == "no_change")
        if error is not None:
            return report
        assert report["status"] == status
        next_receipt = report["scenario"]
        if status == "no_change":
            assert next_receipt == receipt
        else:
            self.commits += 1
            for field in ("scenario_revision", "timetable_revision"):
                assert isinstance(next_receipt[field], str)
                assert int(next_receipt[field]) == int(receipt[field]) + 1
            for field in ("scenario_payload_hash", "timetable_payload_hash"):
                assert next_receipt[field] != receipt[field]
            for field in set(receipt) - {"scenario_revision", "timetable_revision",
                                         "scenario_payload_hash", "timetable_payload_hash"}:
                assert next_receipt[field] == receipt[field], field
        self.show(next_receipt)
        return next_receipt

    def entities(self, receipt, view):
        entities, offset = [], 0
        while True:
            page = self.invoke("scenario-timetable", *common(receipt), "--view", view,
                               "--offset", offset, "--limit", 100, readonly=True)
            assert page["scenario"] == receipt
            entities.extend(page["entities"])
            if not page["has_more"]:
                break
            assert page["next_offset"] > offset
            offset = page["next_offset"]
        assert len(entities) == page["total_entities"]
        return entities

    def rows(self, receipt, view, entity, limit=3):
        rows, locks, offset = [], [], 0
        while True:
            page = self.invoke("scenario-timetable", *common(receipt), "--view", view,
                               "--entity-id", entity["filter"]["id"], "--offset", offset,
                               "--limit", limit, readonly=True)
            assert page["scenario"] == receipt and page["selection"] == entity
            rows.extend(page["rows"])
            locks.extend(page["activity_locks"])
            if not page["has_more"]:
                break
            assert page["next_offset"] > offset
            offset = page["next_offset"]
        assert len(rows) == page["total_rows"] == len({row["activity_id"] for row in rows})
        page["rows"], page["activity_locks"] = rows, locks
        return page

    def payloads(self, receipt):
        with sqlite3.connect(self.database.as_uri() + "?mode=ro", uri=True) as store:
            scenario, timetable = store.execute(
                "SELECT s.payload, t.payload FROM scenario_revisions s JOIN timetable_revisions t "
                "ON s.scenario_id=t.scenario_id AND s.scenario_revision=t.scenario_revision "
                "WHERE s.scenario_id=? AND s.scenario_revision=?",
                (receipt["scenario_id"], int(receipt["scenario_revision"]))).fetchone()
        return json.loads(scenario), json.loads(timetable)

    def find_moves(self, receipt, page):
        slots = {cell["timeslot_index"]: cell["timeslot_id"] for cell in page["calendar"]}
        source_locked = {lock["activity_id"] for lock in page["activity_locks"] if lock["source_locked"]}
        candidates = sorted((row for row in page["rows"] if row["activity_id"] not in source_locked
                             and row["audience"]["kind"] == "teaching_section"),
                            key=lambda row: (row["duration_periods"], row["activity_id"]))
        valid = rejected = None
        for row in candidates:
            current = slots[row["start_timeslot_index"]]
            for slot in slots.values():
                if slot == current:
                    continue
                operation = move(row["activity_id"], slot)
                preview = self.preview(receipt, operation)
                if preview["status"] == "valid" and valid is None:
                    valid = (row, current, slot, operation, preview)
                if preview["status"] == "hard_rejected" and rejected is None:
                    rejected = (operation, preview)
                if valid is not None and rejected is not None:
                    return valid, rejected
        raise AssertionError((receipt["scenario_id"], "native timetable lacks a tested legal and illegal move"))

    def stale_checks(self, receipt, operation):
        for field, code in (("scenario_payload_hash", "APPLICATION_SCENARIO_HASH_CONFLICT"),
                            ("timetable_payload_hash", "APPLICATION_TIMETABLE_HASH_CONFLICT"),
                            ("scenario_revision", "APPLICATION_SCENARIO_REVISION_CONFLICT"),
                            ("timetable_revision", "APPLICATION_TIMETABLE_REVISION_CONFLICT")):
            stale = dict(receipt)
            stale[field] = "00" * 32 if field.endswith("hash") else "9007199254740993"
            self.commit(stale, operation, error=code)

    def clone(self, receipt):
        child_id = str(uuid.uuid4())
        report = self.invoke("clone-scenario", "--project-id", receipt["project_id"],
                             "--expected-source-revision", receipt["source_project_revision"],
                             "--parent-scenario-id", receipt["scenario_id"],
                             "--expected-scenario-revision", receipt["scenario_revision"],
                             "--expected-timetable-revision", receipt["timetable_revision"],
                             "--scenario-id", child_id, "--display-name", "Edited native copy")
        child = report["scenario"]
        self.clones += 1
        assert child["scenario_revision"] == child["timetable_revision"] == "0"
        assert child["scenario_id"] != receipt["scenario_id"] and child["timetable_id"] != receipt["timetable_id"]
        shown = self.show(child)
        for field in ("scenario_id", "scenario_revision", "scenario_payload_hash",
                      "timetable_id", "timetable_revision", "timetable_payload_hash"):
            assert shown["clone_lineage"][field] == receipt[field]
        parent_scenario, parent_timetable = self.payloads(receipt)
        scenario, timetable = self.payloads(child)
        assert scenario["schema_version"] == timetable["schema_version"] == 2
        assert scenario["edit"]["previous_revision"] is None and scenario["edit"]["operation"] is None
        assert scenario["materialized_sectioning"] == parent_scenario["materialized_sectioning"]
        assert assignment_map(timetable) == assignment_map(parent_timetable)
        assert timetable["quality"] == parent_timetable["quality"]
        for field in ("origin_run_id", "origin_artifact_hash", "source_payload_hash"):
            assert scenario[field] == parent_scenario[field]
        assert {meeting["id"] for meeting in timetable["meetings"]}.isdisjoint(
            meeting["id"] for meeting in parent_timetable["meetings"])
        assert len(timetable["user_locks"]) == len(parent_timetable["user_locks"]) == 1
        assert timetable["user_locks"][0]["assignment"] == parent_timetable["user_locks"][0]["assignment"]
        for field in ("id", "scheduled_meeting_id"):
            assert timetable["user_locks"][0][field] != parent_timetable["user_locks"][0][field]
        return child

    def views_and_exports(self, receipt, activity):
        evidence = []
        _, timetable = self.payloads(receipt)
        assignment = assignment_map(timetable)[activity]
        for view in VIEWS:
            for entity in self.entities(receipt, view):
                page = self.rows(receipt, view, entity)
                selected = next((row for row in page["rows"] if row["activity_id"] == activity), None)
                if selected is not None:
                    break
            else:
                raise AssertionError((view, activity, "edited activity absent from every real audience"))
            slot = next(cell for cell in page["calendar"] if cell["timeslot_id"] == assignment["start"])
            assert selected["start_timeslot_index"] == slot["timeslot_index"]
            assert selected["teacher"]["id"] == assignment["teacher_id"]
            assert selected["room"]["id"] == assignment["room_id"]
            assert page["quality"] == timetable["quality"]
            evidence.append({"view": view, "entityId": entity["filter"]["id"], "activities": len(page["rows"])})
            if view != "grade":
                continue
            for format_name in ("csv", "xlsx"):
                path = self.output / f"{receipt['scenario_id']}.{format_name}"
                exported = self.invoke("export-scenario", *common(receipt), "--view", view,
                                       "--entity-id", entity["filter"]["id"], "--format", format_name,
                                       "--output", path, readonly=True)
                self.exports += 1
                assert exported["scenario"] == receipt and exported["meeting_count"] == len(page["rows"])
                assert int(exported["byte_length"]) == path.stat().st_size
                if format_name == "csv":
                    records = list(csv.DictReader(io.StringIO(path.read_text(encoding="utf-8-sig"), newline="")))
                    assert len(records) == len(page["rows"])
                    for record, row in zip(records, page["rows"]):
                        assert record["activity_id"] == row["activity_id"]
                        assert record["day"] == row["day_label"] and record["period"] == row["period_label"]
                        for field in ("scenario_revision", "timetable_revision", "scenario_payload_hash", "timetable_payload_hash"):
                            assert record[field] == receipt[field]
                else:
                    source, grid, courses = workbook_rows(path)
                    assert len(courses) == len(page["rows"]) + 1
                    assert sum(bool(cell and cell != "无课程") for row in grid[3:] for cell in row[1:]) == sum(
                        row["duration_periods"] for row in page["rows"])
                    for record, row in zip(courses[1:], page["rows"]):
                        assert record[:3] == [row["day_label"], row["period_label"], str(row["duration_periods"])]
                        assert record[-1] == row["activity_id"]
                    assert receipt["timetable_payload_hash"] in "\n".join("\t".join(row) for row in source)
        return evidence


def database_snapshot(path):
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True) as store:
        immutable = {table: store.execute(f"SELECT * FROM {table}").fetchall() for table in
                     ("projects", "project_revisions", "solver_runs", "solve_artifacts", "solve_artifact_attempts")}
        original = {table: store.execute(f"SELECT * FROM {table} WHERE scenario_revision=0").fetchall()
                    for table in ("scenario_revisions", "timetable_revisions")}
        counts = tuple(store.execute(f"SELECT count(*) FROM {table}").fetchone()[0] for table in
                       ("scenarios", "scenario_revisions", "timetable_revisions"))
    return immutable, original, counts


def main():
    assert __debug__, "run without -O so validation assertions remain active"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--output-parent", type=Path, default=Path("target"))
    args = parser.parse_args()
    cli, source = args.cli.resolve(strict=True), args.database.resolve(strict=True)
    source_hash = digest(source)
    output = Path(tempfile.mkdtemp(prefix="scenario-edits-", dir=args.output_parent.resolve(strict=True)))
    database = output / "projects.sqlite3"
    with sqlite3.connect(source.as_uri() + "?mode=ro", uri=True) as origin:
        with sqlite3.connect(database) as destination:
            origin.backup(destination)
    assert digest(source) == source_hash
    checks = Checks(cli, database, output)
    immutable, originals, counts = database_snapshot(database)
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as store:
        scenario_ids = [row[0] for row in store.execute("SELECT scenario_id FROM scenarios ORDER BY scenario_id")]
    assert len(scenario_ids) == 4, "provide the four adopted/copied native A/B scenarios"
    evidence, modes = [], set()
    try:
        for scenario_id in scenario_ids:
            shown = checks.invoke("show-scenario", "--scenario-id", scenario_id, readonly=True)
            receipt = shown["scenario"]
            assert shown["hard_valid"] and shown["source_is_current"]
            assert receipt["scenario_revision"] == receipt["timetable_revision"] == "0"
            modes.add(shown["has_materialized_sectioning"])
            scenario, timetable = checks.payloads(receipt)
            assert scenario["schema_version"] == timetable["schema_version"] == 1
            grade = checks.entities(receipt, "grade")[0]
            page = checks.rows(receipt, "grade", grade)
            valid, rejected = checks.find_moves(receipt, page)
            row, original_slot, target, operation, preview = valid
            activity = row["activity_id"]
            assert preview["changes"][0]["after"]["start"] == target
            noop = move(activity, original_slot)
            assert checks.preview(receipt, noop)["status"] == "no_change"
            checks.commit(receipt, noop, status="no_change")
            checks.commit(receipt, rejected[0], error="APPLICATION_SCENARIO_EDIT_HARD_REJECTED")
            checks.stale_checks(receipt, operation)
            moved = checks.commit(receipt, operation)
            assert checks.show(moved)["quality"] == preview["after_quality"]
            checks.commit(receipt, operation, error="APPLICATION_SCENARIO_REVISION_CONFLICT")
            lock = ["lock-current", "--activity-id", activity]
            assert checks.preview(moved, lock)["status"] == "valid"
            locked = checks.commit(moved, lock)
            checks.commit(locked, lock, status="no_change")
            for lock_after in (False, True):
                forbidden = move(activity, original_slot, lock_after)
                problems = checks.preview(locked, forbidden)["validation"]["hard_problems"]
                assert any(problem["code"] == "LockedAssignmentChanged" for problem in problems)
                checks.commit(locked, forbidden, error="APPLICATION_SCENARIO_EDIT_HARD_REJECTED")
            child = checks.clone(locked)
            unlocked = checks.commit(locked, ["unlock", "--activity-id", activity])
            assert not checks.payloads(unlocked)[1]["user_locks"]
            assert checks.payloads(child)[1]["user_locks"]
            assert checks.preview(child, move(activity, original_slot))["status"] == "hard_rejected"
            current_page = checks.rows(unlocked, "grade", grade)
            edited_row = next(item for item in current_page["rows"] if item["activity_id"] == activity)
            other = next(item for item in current_page["rows"] if item["activity_id"] != activity
                         and item["start_timeslot_index"] != edited_row["start_timeslot_index"])
            swap = ["swap-starts", "--left", activity, "--right", other["activity_id"]]
            swap_preview = checks.preview(unlocked, swap)
            if swap_preview["status"] == "valid":
                current = checks.commit(unlocked, swap)
                assert checks.show(current)["quality"] == swap_preview["after_quality"]
            else:
                assert swap_preview["status"] == "hard_rejected"
                checks.commit(unlocked, swap, error="APPLICATION_SCENARIO_EDIT_HARD_REJECTED")
                current = unlocked
            assert assignment_map(checks.payloads(child)[1])[activity]["start"] == target
            parent_views = checks.views_and_exports(current, activity)
            child_views = checks.views_and_exports(child, activity)
            evidence.append({"sourceScenario": scenario_id, "autoSectioned": shown["has_materialized_sectioning"],
                             "activityId": activity, "originalTimeslot": original_slot, "editedTimeslot": target,
                             "hardMoveProblems": rejected[1]["validation"]["hard_problems"],
                             "swapStatus": swap_preview["status"], "finalReceipt": current,
                             "copyReceipt": child, "parentViews": parent_views, "copyViews": child_views})
            print(json.dumps({"checkedScenario": scenario_id, "revision": current["scenario_revision"],
                              "swapStatus": swap_preview["status"]}), flush=True)
        assert modes == {False, True}, "both native A and materialized B are required"
        after_immutable, after_originals, after_counts = database_snapshot(database)
        assert after_immutable == immutable
        for table, rows in originals.items():
            assert all(row in after_originals[table] for row in rows), "original v1 payload was overwritten"
        assert after_counts == (counts[0] + checks.clones, counts[1] + checks.commits + checks.clones,
                                counts[2] + checks.commits + checks.clones)
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as store:
            assert not store.execute("PRAGMA foreign_key_check").fetchall()
            assert store.execute("PRAGMA integrity_check").fetchone() == ("ok",)
        assert digest(source) == source_hash
        report = {"output": str(output), "sourceDatabaseUnchanged": True,
                  "sourceSha256": source_hash, "database": str(database), "invocations": checks.invocations,
                  "committedEdits": checks.commits, "independentCopies": checks.clones,
                  "exports": checks.exports, "revisionCounts": after_counts, "scenarios": evidence}
        (output / "evidence.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({key: value for key, value in report.items() if key != "scenarios"}, ensure_ascii=False))
    finally:
        assert digest(source) == source_hash, "the original database changed during verification"


if __name__ == "__main__":
    main()
