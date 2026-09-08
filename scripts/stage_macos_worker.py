#!/usr/bin/env python3
"""Build-time-only staging for the pinned macOS arm64 worker. No Python solver runtime."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
LOCK = ROOT / "solver/ortools-worker/third_party/ortools.lock.json"
STAGE = ROOT / "target/managed-worker/macos-arm64"
WORKER = ROOT / "solver/ortools-worker/build/ortools-scheduler-worker"
NATIVE_LICENSES = ROOT / "third_party/native"
MANIFEST_RELATIVE = "Resources/solver/macos-arm64/manifest-v1.json"
WORKER_RELATIVE = "MacOS/ortools-scheduler-worker"
SYSTEM_LIBRARIES = {
    "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
    "/usr/lib/libSystem.B.dylib",
    "/usr/lib/libc++.1.dylib",
}


def run(*args):
    return subprocess.check_output([str(arg) for arg in args], text=True, stderr=subprocess.STDOUT)


def digest(path):
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def native_info(path):
    lines = run("/usr/bin/otool", "-L", path).splitlines()[1:]
    if path.suffix == ".dylib":
        lines = lines[1:]  # LC_ID_DYLIB is identity, not a dependency edge.
    dependencies = [line.strip().split(" (compatibility")[0] for line in lines]
    commands = run("/usr/bin/otool", "-l", path).splitlines()
    rpaths = []
    minimum = []
    for index, line in enumerate(commands):
        if line.strip() == "cmd LC_RPATH":
            rpaths.append(commands[index + 2].strip().split(" (offset")[0].removeprefix("path "))
        if line.strip().startswith("minos "):
            minimum.append(line.strip().split()[1])
    architecture = run("/usr/bin/lipo", "-archs", path).strip()
    if architecture != "arm64":
        raise ValueError(f"unsupported architecture for {path.name}: {architecture}")
    if len(minimum) != 1:
        raise ValueError(f"missing or ambiguous deployment target: {path.name}")
    if tuple(map(int, minimum[0].split("."))) > (26, 0):
        raise ValueError(f"deployment target exceeds the development contract: {path.name}")
    return {"dependencies": dependencies, "rpaths": rpaths, "minimumMacos": minimum[0]}


def dependency_source(dependency, loader, archive_root):
    if dependency.startswith("@rpath/"):
        candidate = archive_root / "lib" / dependency.removeprefix("@rpath/")
    elif dependency.startswith("@loader_path/"):
        candidate = loader.parent / dependency.removeprefix("@loader_path/")
    else:
        raise ValueError(f"unmanaged dependency of {loader.name}: {dependency}")
    actual = candidate.resolve(strict=True)
    if not actual.is_relative_to(archive_root / "lib") or not actual.is_file():
        raise ValueError(f"dependency escaped pinned archive: {dependency}")
    return candidate.name, actual


def collect_closure(worker, archive_root):
    libraries = {}
    pending = [worker]
    seen = set()
    while pending:
        source = pending.pop()
        if source in seen:
            continue
        seen.add(source)
        for dependency in native_info(source)["dependencies"]:
            if dependency in SYSTEM_LIBRARIES:
                continue
            name, actual = dependency_source(dependency, source, archive_root)
            if name in libraries and libraries[name] != actual:
                raise ValueError(f"ambiguous library load name: {name}")
            libraries[name] = actual
            pending.append(actual)
    if not libraries or len(libraries) > 192:
        raise ValueError("unbounded or empty native dependency closure")
    if len({name.casefold() for name in libraries}) != len(libraries):
        raise ValueError("case-insensitive native load-name collision")
    return libraries


def verified_archive_copy(archive, archive_root, source, destination):
    member_name = f"{archive_root.name}/{source.relative_to(archive_root).as_posix()}"
    member = archive.getmember(member_name)
    if not member.isfile():
        raise ValueError(f"expected a regular pinned archive member: {member_name}")
    with archive.extractfile(member) as stream:
        data = stream.read()
    if hashlib.sha256(data).hexdigest() != digest(source):
        raise ValueError(f"extracted archive member was modified: {member_name}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(data)
    destination.chmod(0o755 if destination.suffix == ".dylib" else 0o644)


def rewrite_native(path, role):
    information = native_info(path)
    arguments = ["/usr/bin/install_name_tool"]
    for dependency in information["dependencies"]:
        if dependency in SYSTEM_LIBRARIES:
            continue
        if not dependency.startswith(("@rpath/", "@loader_path/")):
            raise ValueError(f"unmanaged native dependency: {dependency}")
        name = Path(dependency).name
        replacement = f"@loader_path/{'../Frameworks/' if role == 'worker' else ''}{name}"
        arguments.extend(["-change", dependency, replacement])
    for rpath in information["rpaths"]:
        arguments.extend(["-delete_rpath", rpath])
    if role == "dylib":
        arguments.extend(["-id", f"@rpath/{path.name}"])
    run(*arguments, path)
    run("/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none", path)
    run("/usr/bin/codesign", "--verify", "--strict", path)


def inspect_installed(contents, manifest):
    contents = contents.resolve(strict=True)
    listed = set()
    total = 0
    for artifact in manifest["artifacts"]:
        relative = Path(artifact["path"])
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("unsafe manifest path")
        path = contents / relative
        if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(contents):
            raise ValueError(f"missing or unsafe staged artifact: {relative}")
        if path.stat().st_size != artifact["sizeBytes"] or digest(path) != artifact["sha256"]:
            raise ValueError(f"artifact digest mismatch: {relative}")
        total += path.stat().st_size
        listed.add(path)
    native = [artifact for artifact in manifest["artifacts"] if artifact["role"] != "license"]
    for artifact in native:
        path = contents / artifact["path"]
        info = native_info(path)
        if info["rpaths"]:
            raise ValueError(f"unexpected RPATH in installed artifact: {path.name}")
        for dependency in info["dependencies"]:
            if dependency in SYSTEM_LIBRARIES:
                continue
            if not dependency.startswith("@loader_path/"):
                raise ValueError(f"non-local installed dependency: {dependency}")
            resolved = (path.parent / dependency.removeprefix("@loader_path/")).resolve(strict=True)
            if resolved not in listed or not resolved.is_relative_to(contents):
                raise ValueError(f"dependency outside managed install: {dependency}")
        run("/usr/bin/codesign", "--verify", "--strict", path)
    return {"nativeArtifactCount": len(native), "installedBytes": total}


def artifact_record(contents, destination, role, source, source_name):
    return {"role": role, "path": destination.relative_to(contents).as_posix(),
            "sizeBytes": destination.stat().st_size, "sha256": digest(destination),
            "sourcePath": source_name, "sourceSha256": digest(source)}


def read_native_licenses(license_root, libraries, distribution_sha256):
    inventory_path = license_root / "inventory-v1.json"
    inventory = json.loads(inventory_path.read_text())
    if (inventory.get("schemaVersion") != 1 or inventory.get("target") != "aarch64-apple-darwin"
            or inventory.get("distributionSha256") != distribution_sha256):
        raise ValueError("native license inventory does not match the pinned distribution")
    paths = set()
    files = [(inventory_path, "inventory-v1.json")]
    for entry in inventory["files"]:
        relative = Path(entry["path"])
        if (relative.is_absolute() or not relative.parts or ".." in relative.parts
                or entry["path"] in paths or relative.as_posix() != entry["path"]):
            raise ValueError("unsafe or duplicate native license path")
        paths.add(entry["path"])
        source = license_root / relative
        if (not source.is_file() or any((license_root / Path(*relative.parts[:index])).is_symlink()
                                       for index in range(1, len(relative.parts) + 1))
                or not source.resolve().is_relative_to(license_root.resolve())):
            raise ValueError("missing or unsafe native license file")
        if not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]) or digest(source) != entry["sha256"]:
            raise ValueError(f"native license digest mismatch: {relative}")
        files.append((source, relative.as_posix()))
    covered = []
    for component in inventory["components"]:
        if not component["licenseFiles"] or not set(component["licenseFiles"]).issubset(paths):
            raise ValueError("native component has missing license references")
        covered.extend(component["dylibs"])
    if len(covered) != len(set(covered)) or set(covered) != set(libraries):
        raise ValueError("native license inventory does not cover the exact dylib closure")
    if not isinstance(inventory.get("binaryRedistributionReady"), bool):
        raise ValueError("native license inventory must state its release status")
    return inventory, files


def stage_native_licenses(contents, files):
    records = []
    for source, relative in files:
        destination = contents / "Resources/solver/macos-arm64/licenses/native" / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        records.append(artifact_record(contents, destination, "license", source,
                                       f"third_party/native/{relative}"))
    return records


def build_configuration():
    entries = {}
    for line in (WORKER.parent / "CMakeCache.txt").read_text().splitlines():
        match = re.match(r"([^#/:][^:]*):[^=]+=(.*)", line)
        if match:
            entries[match.group(1)] = match.group(2)
    for key, expected in {"CMAKE_BUILD_TYPE": "Release", "CMAKE_OSX_ARCHITECTURES": "arm64", "CMAKE_OSX_DEPLOYMENT_TARGET": "26.0"}.items():
        if entries.get(key) != expected:
            raise ValueError(f"worker build must explicitly set {key}={expected}")
    return {"compiler": run(entries["CMAKE_CXX_COMPILER"], "--version").splitlines()[0],
            "cmake": run("cmake", "--version").splitlines()[0], "buildType": entries["CMAKE_BUILD_TYPE"],
            "cxxFlags": entries.get("CMAKE_CXX_FLAGS", ""), "releaseFlags": entries.get("CMAKE_CXX_FLAGS_RELEASE", ""),
            "cxxStandard": 20, "deploymentTarget": entries["CMAKE_OSX_DEPLOYMENT_TARGET"]}


def build_stage(temporary, lock, archive_path, archive_root):
    configuration = build_configuration()
    contents = temporary / "Contents"
    worker_destination = contents / WORKER_RELATIVE
    worker_destination.parent.mkdir(parents=True)
    shutil.copyfile(WORKER, worker_destination)
    worker_destination.chmod(0o755)
    libraries = collect_closure(WORKER, archive_root)
    license_inventory, license_files = read_native_licenses(NATIVE_LICENSES, libraries, lock["distribution"]["sha256"])
    records = []
    with tarfile.open(archive_path, "r:gz") as archive:
        for name, source in sorted(libraries.items()):
            destination = contents / "Frameworks" / name
            verified_archive_copy(archive, archive_root, source, destination)
            rewrite_native(destination, "dylib")
            records.append(artifact_record(contents, destination, "dylib", source, source.relative_to(archive_root).as_posix()))
        licenses = [archive_root / "share/doc/ortools/LICENSE"]
        licenses.extend(sorted(path for path in (archive_root / "share/licenses").rglob("*") if path.is_file()))
        for source in licenses:
            destination = contents / "Resources/solver/macos-arm64/licenses" / source.relative_to(archive_root)
            verified_archive_copy(archive, archive_root, source, destination)
            records.append(artifact_record(contents, destination, "license", source, source.relative_to(archive_root).as_posix()))
    records.extend(stage_native_licenses(contents, license_files))
    rewrite_native(worker_destination, "worker")
    records.append(artifact_record(contents, worker_destination, "worker", WORKER, WORKER.relative_to(ROOT).as_posix()))
    header = (ROOT / "solver/ortools-worker/include/ortools_worker/solver_worker.h").read_text()
    adapter = re.search(r'kAdapterVersion\[\] = "([^"]+)"', header).group(1)
    source_paths = sorted(path for path in (ROOT / "solver/ortools-worker").rglob("*") if path.is_file() and "build" not in path.relative_to(ROOT / "solver/ortools-worker").parts)
    source_paths.append(ROOT / "crates/solver-contract/proto/scheduler/v1/solver.proto")
    source_paths.append(Path(__file__).resolve())
    manifest = {
        "schemaVersion": 1, "target": "aarch64-apple-darwin", "engineVersion": lock["engine_version"],
        "protocolVersion": 1, "adapterVersion": adapter, "minimumMacos": "26.0", "workerPath": WORKER_RELATIVE,
        "distributionSha256": lock["distribution"]["sha256"], "signing": "adhoc-development",
        "artifacts": sorted(records, key=lambda item: item["path"]),
        "provenance": {"upstream": lock, **configuration,
            "timestampPolicy": "omitted; ad-hoc signatures use --timestamp=none",
            "sourceFiles": {path.relative_to(ROOT).as_posix(): digest(path) for path in source_paths},
            "systemDependencies": sorted(SYSTEM_LIBRARIES),
            "licenseStatus": license_inventory["status"],
            "nativeLicenseInventory": {"path": "Resources/solver/macos-arm64/licenses/native/inventory-v1.json",
                "sha256": digest(NATIVE_LICENSES / "inventory-v1.json"),
                "componentCount": len(license_inventory["components"]),
                "binaryRedistributionReady": license_inventory["binaryRedistributionReady"],
                "limitations": license_inventory["limitations"]},
            "releaseStatus": "macOS arm64 development install; no Developer ID, notarization or other-platform claim"},
    }
    inspect_installed(contents, manifest)
    encoded = (json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()
    (temporary / "manifest-v1.json").write_bytes(encoded)
    (contents / MANIFEST_RELATIVE).write_bytes(encoded)
    external = temporary / "external/ortools-scheduler-worker-aarch64-apple-darwin"
    external.parent.mkdir()
    shutil.copy2(worker_destination, external)
    overlay = {"bundle": {"active": True,
        "externalBin": [str(STAGE / "external/ortools-scheduler-worker")],
        "resources": {str(STAGE / "manifest-v1.json"): "solver/macos-arm64/manifest-v1.json",
                      str(STAGE / "Contents/Resources/solver/macos-arm64/licenses"): "solver/macos-arm64/licenses"},
        "macOS": {"minimumSystemVersion": "26.0", "frameworks": [str(STAGE / "Contents/Frameworks" / name) for name in sorted(libraries)]}}}
    (temporary / "tauri-overlay.json").write_text(json.dumps(overlay, ensure_ascii=False, indent=2) + "\n")
    return {"dylibCount": len(libraries), "artifactCount": len(records), "installedBytes": sum(record["sizeBytes"] for record in records), "manifestSha256": hashlib.sha256(encoded).hexdigest()}


def stage():
    if sys.platform != "darwin":
        raise ValueError("this development staging tool requires macOS; artifacts must be arm64")
    lock = json.loads(LOCK.read_text())
    archive_path = ROOT / ".cache/ortools" / lock["distribution"]["file_name"]
    if digest(archive_path) != lock["distribution"]["sha256"]:
        raise ValueError("pinned OR-Tools archive checksum mismatch")
    archive_root = archive_path.with_suffix("").with_suffix("").resolve(strict=True)
    STAGE.parent.mkdir(parents=True, exist_ok=True)
    if STAGE.is_symlink() or (STAGE.exists() and not (STAGE / "manifest-v1.json").is_file()):
        raise ValueError("refusing to replace an unmanaged staging path")
    with tempfile.TemporaryDirectory(prefix=".macos-worker-", dir=STAGE.parent) as directory:
        temporary = Path(directory)
        report = build_stage(temporary, lock, archive_path, archive_root)
        backup = STAGE.with_name(".macos-arm64-previous")
        if backup.exists():
            raise ValueError("previous staging backup exists; inspect it before continuing")
        if STAGE.exists():
            STAGE.rename(backup)
        try:
            temporary.rename(STAGE)
        except BaseException:
            if backup.exists():
                backup.rename(STAGE)
            raise
        if backup.exists():
            shutil.rmtree(backup)
    print(json.dumps(report, ensure_ascii=False))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["stage", "verify", "audit-licenses"])
    parser.add_argument("--contents", type=Path)
    parser.add_argument("--require-redistributable", action="store_true")
    args = parser.parse_args()
    if args.require_redistributable and args.command != "audit-licenses":
        parser.error("--require-redistributable is only valid for audit-licenses")
    if args.command == "stage":
        if args.contents is not None:
            parser.error("stage output is fixed and does not accept --contents")
        stage()
    elif args.command == "audit-licenses":
        if args.contents is not None:
            parser.error("audit-licenses reads the fixed pinned build and does not accept --contents")
        lock = json.loads(LOCK.read_text())
        archive_path = ROOT / ".cache/ortools" / lock["distribution"]["file_name"]
        if digest(archive_path) != lock["distribution"]["sha256"]:
            raise ValueError("pinned OR-Tools archive checksum mismatch")
        libraries = collect_closure(WORKER, archive_path.with_suffix("").with_suffix("").resolve(strict=True))
        inventory, files = read_native_licenses(NATIVE_LICENSES, libraries, lock["distribution"]["sha256"])
        print(json.dumps({"dylibCount": len(libraries), "componentCount": len(inventory["components"]),
                          "noticeAndReferenceFileCount": len(files),
                          "binaryRedistributionReady": inventory["binaryRedistributionReady"],
                          "limitations": inventory["limitations"]}, ensure_ascii=False))
        if args.require_redistributable and not inventory["binaryRedistributionReady"]:
            raise ValueError("native binary redistribution gate is incomplete; see inventory limitations")
    else:
        if args.contents is None:
            parser.error("verify requires --contents")
        installed = (args.contents / MANIFEST_RELATIVE).read_bytes()
        if installed != (STAGE / "manifest-v1.json").read_bytes():
            raise ValueError("installed manifest differs from the build stage")
        report = inspect_installed(args.contents, json.loads(installed))
        report["manifestSha256"] = hashlib.sha256(installed).hexdigest()
        print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"managed-worker stage rejected: {error}", file=sys.stderr)
        raise SystemExit(1) from error
