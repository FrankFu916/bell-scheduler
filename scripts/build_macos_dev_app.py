#!/usr/bin/env python3
"""Build a macOS arm64 development .app with a verified, app-relative worker closure."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

import stage_macos_worker as worker_stage

ROOT = worker_stage.ROOT


def checked(*arguments, cwd=ROOT, environment=None):
    subprocess.run([str(argument) for argument in arguments], cwd=cwd, env=environment, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true", help="use release Rust profile; still ad-hoc development native artifacts")
    parser.add_argument("--skip-isolation", action="store_true", help="build only; does not claim the isolated native gate")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("this build supports macOS arm64 only")
    lock = json.loads(worker_stage.LOCK.read_text())
    archive = ROOT / ".cache/ortools" / lock["distribution"]["file_name"]
    if worker_stage.digest(archive) != lock["distribution"]["sha256"]:
        raise ValueError("pinned archive checksum mismatch before native build")
    checked("cmake", "-S", ROOT / "solver/ortools-worker", "-B", ROOT / "solver/ortools-worker/build",
            "-DORTOOLS_WORKER_BUILD_TESTS=ON", "-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_OSX_ARCHITECTURES=arm64", "-DCMAKE_OSX_DEPLOYMENT_TARGET=26.0")
    checked("cmake", "--build", ROOT / "solver/ortools-worker/build", "--parallel")
    checked("ctest", "--test-dir", ROOT / "solver/ortools-worker/build", "--output-on-failure")
    worker_stage.stage()
    tauri = ROOT / "frontend/node_modules/.bin/tauri"
    if not tauri.exists():
        checked("npm", "--prefix", ROOT / "frontend", "ci")
    cargo = Path(worker_stage.run("rustup", "which", "--toolchain", "1.88.0", "cargo").strip())
    environment = dict(os.environ)
    environment.update(PATH=f"{cargo.parent}{os.pathsep}{environment.get('PATH', '')}",
                       RUSTUP_TOOLCHAIN="1.88.0", CARGO_TARGET_DIR=str(ROOT / "target/rust-1.88.0"),
                       MACOSX_DEPLOYMENT_TARGET="26.0")
    command = [tauri, "build", "--bundles", "app", "--no-sign", "--features", "managed-worker",
               "--config", worker_stage.STAGE / "tauri-overlay.json"]
    if not args.release:
        command.append("--debug")
    checked(*command, cwd=ROOT / "apps/desktop/src-tauri", environment=environment)
    profile = "release" if args.release else "debug"
    product = json.loads((ROOT / "apps/desktop/src-tauri/tauri.conf.json").read_text())["productName"]
    app = ROOT / "target/rust-1.88.0" / profile / "bundle/macos" / f"{product}.app"
    checked(sys.executable, ROOT / "scripts/stage_macos_worker.py", "verify", "--contents", app / "Contents")
    if not args.skip_isolation:
        example = [cargo, "build", "--locked", "-p", "class-schedule-desktop", "--example", "managed_worker_probe", "--features", "managed-worker"]
        if args.release:
            example.append("--release")
        checked(*example, environment=environment)
        probe = ROOT / "target/rust-1.88.0" / profile / "examples/managed_worker_probe"
        checked(sys.executable, ROOT / "scripts/verify_macos_install.py", "--app", app, "--probe", probe)
    print(json.dumps({"app": str(app), "profile": profile, "isolationExecuted": not args.skip_isolation,
                      "distributionStatus": "development; no Developer ID or notarization; macOS arm64 only"}, ensure_ascii=False))


if __name__ == "__main__":
    main()
