#!/usr/bin/env python3
"""Verify adoption/copy/reload and read-only views using isolated real saved-run fixtures.

This script never modifies the supplied database and never invents solver output.
Provide a fixture database containing successful native A and B runs for coverage of both modes.
"""

import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--output-parent", type=Path, default=Path("target"))
    args = parser.parse_args()
    cli = args.cli.resolve(strict=True)
    source = args.database.resolve(strict=True)
    output = Path(tempfile.mkdtemp(prefix="scenario-native-", dir=args.output_parent.resolve(strict=True)))
    database = output / "projects.sqlite3"
    with sqlite3.connect(source.as_uri() + "?mode=ro", uri=True) as original:
        with sqlite3.connect(database) as copy:
            original.backup(copy)

    def invoke(command, *arguments, expected_code=None):
        completed = subprocess.run([str(cli), command, "--database", str(database), *map(str, arguments)],
                                   capture_output=True, text=True, check=False)
        report = json.loads(completed.stdout if completed.returncode == 0 else completed.stderr)
        if expected_code is None:
            assert completed.returncode == 0, (command, completed.returncode, report)
        else:
            assert completed.returncode == 2, (command, completed.returncode)
            assert report["code"] == expected_code, (command, report)
        return report

    def query_views(receipt, summary):
        before = hashlib.sha256(database.read_bytes()).hexdigest()
        common = ["--scenario-id", receipt["scenario_id"],
                  "--expected-scenario-revision", receipt["scenario_revision"],
                  "--expected-timetable-revision", receipt["timetable_revision"]]
        views = {}
        for view in ("administrative-class", "teaching-section", "teacher", "room", "student", "subject", "grade"):
            options = invoke("scenario-timetable", *common, "--view", view)
            assert options["scenario"] == receipt and options["source_is_current"]
            assert options["entities"], (receipt["scenario_id"], view)
            selected = options["entities"][0]
            entity_id = selected["filter"]["id"]
            page = invoke("scenario-timetable", *common, "--view", view, "--entity-id", entity_id, "--limit", 100)
            assert page["scenario"] == receipt and page["selection"] == selected
            assert page["quality"] == summary["quality"]
            assert page["validation"] == "independently_revalidated"
            assert not page["has_more"] and len(page["rows"]) == page["total_rows"]
            assert page["calendar"]
            if view == "grade":
                assert page["total_rows"] == summary["activity_count"]
            if view == "student":
                rows, offset = [], 0
                full_counts = {cell["timeslot_index"]: cell["occupied_count"] for cell in page["calendar"]}
                while True:
                    part = invoke("scenario-timetable", *common, "--view", view, "--entity-id", entity_id,
                                  "--limit", 3, "--offset", offset)
                    assert {cell["timeslot_index"]: cell["occupied_count"] for cell in part["calendar"]} == full_counts
                    rows.extend(part["rows"])
                    if not part["has_more"]:
                        break
                    assert part["next_offset"] > offset
                    offset = part["next_offset"]
                assert rows == page["rows"]
            views[view] = page["total_rows"]
        for flag, code in (("--expected-scenario-revision", "APPLICATION_SCENARIO_REVISION_CONFLICT"),
                           ("--expected-timetable-revision", "APPLICATION_TIMETABLE_REVISION_CONFLICT")):
            stale = common.copy()
            stale[stale.index(flag) + 1] = "9007199254740993"
            invoke("scenario-timetable", *stale, "--view", "grade", expected_code=code)
        invoke("scenario-timetable", *common, "--view", "student", "--entity-id", str(uuid.uuid4()),
               expected_code="APPLICATION_TIMETABLE_ENTITY_NOT_FOUND")
        invoke("scenario-timetable", *common, "--view", "grade", "--limit", 101,
               expected_code="APPLICATION_TIMETABLE_INVALID_PAGE")
        assert hashlib.sha256(database.read_bytes()).hexdigest() == before
        return views

    with sqlite3.connect(database) as store:
        sources_before = store.execute("SELECT project_id, current_revision FROM projects ORDER BY project_id").fetchall()
        revisions_before = store.execute("SELECT project_id, revision, payload_hash FROM project_revisions ORDER BY project_id, revision").fetchall()
        runs = store.execute("SELECT a.run_id, a.project_id, a.project_revision, a.status_code FROM solve_artifacts a JOIN projects p ON p.project_id=a.project_id AND p.current_revision=a.project_revision ORDER BY a.run_id").fetchall()
    assert any(run[3] in ("Feasible", "Optimal") for run in runs), "fixture requires a successful current-source run"
    evidence = []
    for run_id, project_id, revision, status in runs:
        scenario_id, clone_id = str(uuid.uuid4()), str(uuid.uuid4())
        common = ["--project-id", project_id, "--expected-source-revision", revision]
        adoption = [*common, "--run-id", run_id, "--scenario-id", scenario_id, "--display-name", "合成验证方案"]
        if status not in ("Feasible", "Optimal"):
            invoke("adopt-run", *adoption, expected_code="APPLICATION_SCENARIO_RUN_NOT_ADOPTABLE")
            evidence.append({"runId": run_id, "status": status, "adoptionRejected": True})
            continue
        adopted = invoke("adopt-run", *adoption)["scenario"]
        shown = invoke("show-scenario", "--scenario-id", scenario_id)
        assert shown["scenario"] == adopted and shown["hard_valid"] and shown["source_is_current"]
        clone_args = [*common, "--parent-scenario-id", scenario_id, "--expected-scenario-revision", "0",
                      "--expected-timetable-revision", "0", "--scenario-id", clone_id, "--display-name", "独立复制验证"]
        cloned = invoke("clone-scenario", *clone_args)["scenario"]
        clone_shown = invoke("show-scenario", "--scenario-id", clone_id)
        assert clone_shown["scenario"] == cloned and clone_shown["hard_valid"]
        assert cloned["timetable_id"] != adopted["timetable_id"]
        assert clone_shown["quality"] == shown["quality"]
        assert clone_shown["activity_count"] == shown["activity_count"]
        assert clone_shown["clone_lineage"]["scenario_payload_hash"] == adopted["scenario_payload_hash"]
        invoke("adopt-run", *adoption, expected_code="PERSISTENCE_SCENARIO_ALREADY_EXISTS")
        stale = clone_args.copy()
        stale[stale.index("--expected-scenario-revision") + 1] = "1"
        stale[stale.index("--scenario-id") + 1] = str(uuid.uuid4())
        invoke("clone-scenario", *stale, expected_code="APPLICATION_SCENARIO_REVISION_CONFLICT")
        assert invoke("show-scenario", "--scenario-id", scenario_id) == shown
        assert invoke("show-scenario", "--scenario-id", clone_id) == clone_shown
        views = query_views(adopted, shown)
        assert query_views(cloned, clone_shown) == views
        evidence.append({"runId": run_id, "scenarioId": scenario_id, "cloneId": clone_id,
                         "hardValid": True, "activityCount": shown["activity_count"],
                         "materializedSectioning": shown["has_materialized_sectioning"],
                         "duplicateAndStaleRejected": True, "viewRowCounts": views,
                         "readQueriesPreservedDatabaseBytes": True})
    with sqlite3.connect(database) as store:
        assert sources_before == store.execute("SELECT project_id, current_revision FROM projects ORDER BY project_id").fetchall()
        assert revisions_before == store.execute("SELECT project_id, revision, payload_hash FROM project_revisions ORDER BY project_id, revision").fetchall()
        assert not store.execute("PRAGMA foreign_key_check").fetchall()
        assert store.execute("PRAGMA integrity_check").fetchone() == ("ok",)
        counts = {table: store.execute("SELECT COUNT(*) FROM " + table).fetchone()[0]
                  for table in ("scenarios", "scenario_revisions", "timetable_revisions")}
    print(json.dumps({"output": str(output), "sourceUnchanged": True, "runs": evidence,
                      "counts": counts, "integrity": "ok"}, ensure_ascii=False))


if __name__ == "__main__":
    main()
