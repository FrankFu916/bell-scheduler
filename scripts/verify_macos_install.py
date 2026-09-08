#!/usr/bin/env python3
"""Relocate the development app, deny repository reads/network, and run real native gates."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

import stage_macos_worker as stage


def execute(command, expected_code=0):
    result = subprocess.run([str(value) for value in command], env={}, cwd="/private/tmp", capture_output=True, text=True)
    if result.returncode != expected_code:
        raise RuntimeError(f"isolated process exit {result.returncode}: {result.stderr}")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--probe", type=Path, required=True)
    args = parser.parse_args()
    manifest = json.loads((args.app / "Contents" / stage.MANIFEST_RELATIVE).read_bytes())
    stage.inspect_installed(args.app / "Contents", manifest)
    with tempfile.TemporaryDirectory(prefix="Bell 隔离 验证 ", dir="/private/tmp") as directory:
        root = Path(directory)
        app = root / args.app.name
        shutil.copytree(args.app, app)
        probe = root / "managed_worker_probe"
        shutil.copy2(args.probe, probe)
        for kind in ["small", "medium"]:
            shutil.copytree(stage.ROOT / "fixtures" / kind, root / kind)
        profile = root / "deny-development.sb"
        profile.write_text(f"(version 1)\n(allow default)\n(deny file-read* (subpath {json.dumps(str(stage.ROOT))}))\n(deny network*)\n")
        prefix = ["/usr/bin/sandbox-exec", "-f", profile]
        execute([*prefix, "/bin/cat", root / "small/students.csv"])
        denied = subprocess.run([*map(str, prefix), "/bin/cat", str(stage.WORKER)], env={}, cwd=root, capture_output=True)
        if denied.returncode == 0 or b"Operation not permitted" not in denied.stderr or str(stage.WORKER).encode() not in denied.stderr:
            raise RuntimeError("isolation did not prove that the original development worker is unreadable")
        reports = []
        for mode, fixture in [("existing", "small"), ("auto", "small"), ("cancel", "medium"), ("timeout", "medium")]:
            result = execute([*prefix, probe, app / "Contents", root / fixture, mode])
            report = json.loads(result.stdout)
            expected_statuses = {"existing": {"Feasible", "Optimal"}, "auto": {"Feasible"},
                                 "cancel": {"Cancelled"}, "timeout": {"Timeout"}}
            if (report.get("mode") != mode or report.get("runSaved") is not True
                    or report.get("databaseReopened") is not True or report.get("sourceUnchanged") is not True
                    or report.get("restoredStatus") not in expected_statuses[mode]
                    or report.get("provenanceRestoredExactly") is not True
                    or report.get("executionAttemptCount") != (2 if mode == "auto" else 1)
                    or report.get("restoredIndependentlyValidated") is not (mode in {"existing", "auto"})
                    or report.get("manifestSha256") != stage.digest(args.app / "Contents" / stage.MANIFEST_RELATIVE)):
                raise RuntimeError(f"{mode} did not prove native terminal save, database reopen and independent replay")
            reports.append(report)
        library = app / "Contents" / next(item["path"] for item in manifest["artifacts"] if item["role"] == "dylib")
        original = library.read_bytes()
        altered = bytearray(original)
        altered[-1] ^= 1
        library.write_bytes(altered)
        rejected = execute([*prefix, probe, app / "Contents", root / "small", "existing"], 1)
        if "DESKTOP_WORKER_ARTIFACT_HASH_MISMATCH" not in rejected.stderr:
            raise RuntimeError("tampered installed dylib was not rejected by the runtime resolver")
        library.write_bytes(original)
        library.unlink()
        rejected = execute([*prefix, probe, app / "Contents", root / "small", "existing"], 1)
        if "DESKTOP_WORKER_ARTIFACT_MISSING" not in rejected.stderr:
            raise RuntimeError("missing installed dylib was not rejected by the runtime resolver")
        print(json.dumps({"repositoryReadDenied": True, "networkDenied": True, "relocatedUnicodeAndSpacePath": True,
                          "tamperedDylibRejected": True, "missingDylibRejected": True, "runs": reports}, ensure_ascii=False))


if __name__ == "__main__":
    main()
