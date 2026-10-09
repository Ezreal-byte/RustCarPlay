"""Verify release boundaries without building code or touching native hardware."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

SPEC = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package-release.py"))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class ReleasePackagingTests(unittest.TestCase):
    def test_windows_archive_contains_helpers_but_no_runtime_or_secrets(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "dist"
            output.mkdir()
            target = "x86_64-pc-windows-msvc"
            executable_dir = root / "target" / target / "release"
            (executable_dir / "examples").mkdir(parents=True)
            for name in ("carplay-desktop.exe", "rustcarplay.exe", "must-not-ship.dll", "private.pdb"):
                (executable_dir / name).write_bytes(b"fixture")
            for name in release.USB_HELPERS:
                (executable_dir / "examples" / (name + ".exe")).write_bytes(b"helper")
            (root / "scripts").mkdir()
            for name in release.WINDOWS_SCRIPTS:
                (root / "scripts" / name).write_text("fixture", encoding="utf-8")
            (root / "docs").mkdir()
            for name in ("LICENSE", "README.md", "README.en.md", "docs/THIRD_PARTY_NOTICES.md"):
                (root / name).write_text("notice", encoding="utf-8")
            (root / ".local/auth").mkdir(parents=True)
            (root / ".local/auth/identity.pk8").write_bytes(b"DO NOT DISTRIBUTE")
            with patch.object(release, "ROOT", root):
                release.binary("0.1.0", target, output, "owner/repository")
            with zipfile.ZipFile(next(output.iterdir())) as archive:
                names = [str(Path(name).relative_to("RustCarPlay-0.1.0-windows-x86_64")).replace("\\", "/") for name in archive.namelist()]
                self.assertIn("tools/usb_probe.exe", names)
                self.assertIn("scripts/usb-runtime-packages.json", names)
                self.assertIn("DEPENDENCIES-SOURCE.txt", names)
                self.assertIn("RUNTIME.txt", names)
                self.assertFalse(any(name.endswith((".dll", ".pdb", ".pk8")) for name in names))
                self.assertFalse(any(".local" in name for name in names))

    def test_missing_helper_prevents_incomplete_windows_archive(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch.object(release, "ROOT", Path(temporary)):
                with self.assertRaises(FileNotFoundError):
                    release.binary("0.1.0", "x86_64-pc-windows-msvc", Path(temporary), "owner/repository")

    def test_version_mismatch_is_rejected_before_cargo(self):
        with patch.object(release, "version", return_value="0.1.0"):
            with patch.object(release.subprocess, "check_output") as cargo:
                with self.assertRaises(ValueError):
                    release.validate_tag("v0.2.0")
                cargo.assert_not_called()

    def test_checksums_require_every_platform_and_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for label in release.TARGETS.values():
                suffix = ".zip" if label.startswith("windows") else ".tar.gz"
                (root / ("RustCarPlay-0.1.0-" + label + suffix)).write_bytes(b"archive")
            with self.assertRaises(ValueError):
                release.checksums("0.1.0", root)
            (root / "RustCarPlay-0.1.0-source.tar.gz").write_bytes(b"source")
            release.checksums("0.1.0", root)
            lines = (root / "SHA256SUMS").read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(lines), 6)
            for line in lines:
                digest, name = line.split("  ", 1)
                self.assertEqual(digest, hashlib.sha256((root / name).read_bytes()).hexdigest())
            (root / "unexpected.dll").write_bytes(b"native")
            with self.assertRaises(ValueError):
                release.checksums("0.1.0", root)


if __name__ == "__main__":
    unittest.main()
