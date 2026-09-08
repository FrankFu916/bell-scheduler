"""Boundary tests for native staging; real tar/filesystem input, no substitute solver."""

from pathlib import Path
import json
import tarfile
import tempfile
import unittest

import stage_macos_worker as stage


class StagingBoundaries(unittest.TestCase):
    def license_fixture(self, root):
        notice = root / "component/LICENSE"
        notice.parent.mkdir()
        notice.write_bytes(b"upstream license\n")
        inventory = {"schemaVersion": 1, "target": "aarch64-apple-darwin", "distributionSha256": "a" * 64,
                     "binaryRedistributionReady": False,
                     "files": [{"path": "component/LICENSE", "sha256": stage.digest(notice)}],
                     "components": [{"licenseFiles": ["component/LICENSE"], "dylibs": ["libcomponent.dylib"]}]}
        (root / "inventory-v1.json").write_text(json.dumps(inventory))
        return inventory

    def test_license_inventory_checks_exact_closure_and_stages_original_notice_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            original = self.license_fixture(root)
            inventory, files = stage.read_native_licenses(root, {"libcomponent.dylib": None}, "a" * 64)
            self.assertEqual(inventory, original)
            records = stage.stage_native_licenses(root / "Contents", files)
            self.assertEqual(len(records), 2)
            copied = root / "Contents/Resources/solver/macos-arm64/licenses/native/component/LICENSE"
            self.assertEqual(copied.read_bytes(), b"upstream license\n")
            self.assertTrue(all(record["role"] == "license" for record in records))
            with self.assertRaisesRegex(ValueError, "exact dylib closure"):
                stage.read_native_licenses(root, {"libcomponent.dylib": None, "libnew.dylib": None}, "a" * 64)
            with self.assertRaisesRegex(ValueError, "pinned distribution"):
                stage.read_native_licenses(root, {"libcomponent.dylib": None}, "b" * 64)

    def test_license_inventory_rejects_modified_missing_and_escaping_notice_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            inventory = self.license_fixture(root)
            notice = root / "component/LICENSE"
            notice.write_bytes(b"changed terms\n")
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                stage.read_native_licenses(root, {"libcomponent.dylib": None}, "a" * 64)
            notice.unlink()
            with self.assertRaisesRegex(ValueError, "missing or unsafe"):
                stage.read_native_licenses(root, {"libcomponent.dylib": None}, "a" * 64)
            inventory["files"][0]["path"] = "../LICENSE"
            (root / "inventory-v1.json").write_text(json.dumps(inventory))
            with self.assertRaisesRegex(ValueError, "unsafe or duplicate"):
                stage.read_native_licenses(root, {"libcomponent.dylib": None}, "a" * 64)

    def test_license_inventory_rejects_duplicate_component_ownership(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            inventory = self.license_fixture(root)
            inventory["components"].append(inventory["components"][0])
            (root / "inventory-v1.json").write_text(json.dumps(inventory))
            with self.assertRaisesRegex(ValueError, "exact dylib closure"):
                stage.read_native_licenses(root, {"libcomponent.dylib": None}, "a" * 64)

    def test_dependencies_must_resolve_inside_pinned_library_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            archive = root / "archive"
            (archive / "lib").mkdir(parents=True)
            source = archive / "lib/libvalid.dylib"
            source.write_bytes(b"source")
            name, actual = stage.dependency_source("@rpath/libvalid.dylib", root / "worker", archive)
            self.assertEqual(name, "libvalid.dylib")
            self.assertEqual(actual, source)
            outside = root / "outside.dylib"
            outside.write_bytes(b"outside")
            (archive / "lib/escape.dylib").symlink_to(outside)
            for dependency in ["/usr/local/lib/unpinned.dylib", "@rpath/escape.dylib", "@rpath/../../outside.dylib"]:
                with self.assertRaises(ValueError):
                    stage.dependency_source(dependency, root / "worker", archive)

    def test_archive_bytes_are_checked_before_copying_an_extracted_dependency(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            extracted = root / "pinned-archive"
            (extracted / "lib").mkdir(parents=True)
            source = extracted / "lib/checked.dylib"
            source.write_bytes(b"pinned bytes")
            archive_path = root / "archive.tar.gz"
            with tarfile.open(archive_path, "w:gz") as archive:
                archive.add(source, arcname="pinned-archive/lib/checked.dylib")
            destination = root / "install/checked.dylib"
            with tarfile.open(archive_path, "r:gz") as archive:
                stage.verified_archive_copy(archive, extracted, source, destination)
                self.assertEqual(destination.read_bytes(), b"pinned bytes")
                source.write_bytes(b"locally changed")
                rejected = root / "rejected/checked.dylib"
                with self.assertRaisesRegex(ValueError, "was modified"):
                    stage.verified_archive_copy(archive, extracted, source, rejected)
                self.assertFalse(rejected.exists())

    def test_install_verification_rejects_unsafe_or_modified_artifacts_before_loading(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / "MacOS").mkdir()
            source = root / "MacOS/worker"
            source.write_bytes(b"original")
            artifact = {"role": "worker", "path": "MacOS/worker", "sizeBytes": source.stat().st_size, "sha256": stage.digest(source)}
            source.write_bytes(b"modified")
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                stage.inspect_installed(root, {"artifacts": [artifact]})
            artifact["path"] = "../outside"
            with self.assertRaisesRegex(ValueError, "unsafe manifest path"):
                stage.inspect_installed(root, {"artifacts": [artifact]})


if __name__ == "__main__":
    unittest.main()
