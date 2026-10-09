"""Installer layout checks without installing an application or driver."""
import importlib.util
import json
from pathlib import Path
import stat
import struct
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("installers", Path(__file__).with_name("package-installers.py"))
installers = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(installers)


class InstallerPackagingTests(unittest.TestCase):
    def test_ico_keeps_the_existing_png_pixels_and_valid_directory_offset(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "icon.ico"
            installers.png_icon_container(installers.ICON, path)
            data = path.read_bytes()
            self.assertEqual(struct.unpack_from("<HHH", data), (0, 1, 1))
            width, height, _, _, planes, bits, size, offset = struct.unpack_from("<BBBBHHII", data, 6)
            self.assertEqual((width, height, planes, bits), (192, 192, 1, 32))
            self.assertEqual(data[offset:offset + size], installers.ICON.read_bytes())

    def test_installed_marker_replaces_portable_state_instructions(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "RUNTIME.txt").write_text("Write state beside the executable", encoding="utf-8")
            installers.installed_marker(root, "0.1.1", "x86_64-pc-windows-msvc")
            marker = json.loads((root / "INSTALLATION.json").read_text())
            self.assertEqual((marker["schema"], marker["mode"], marker["product"]), (1, "installed", "RustCarPlay"))
            self.assertIn("user data directory", (root / "RUNTIME.txt").read_text(encoding="utf-8"))
            self.assertFalse((root / ".local").exists())

    def test_deb_layout_handles_both_architectures_without_embedding_user_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "portable"
            source.mkdir()
            launcher = source / "RustCarPlay"
            launcher.write_text("#!/bin/sh\nexit 0\n", encoding="ascii")
            launcher.chmod(0o755)
            public_identity = source / "resources/auth/identity.pk8"
            public_identity.parent.mkdir(parents=True)
            public_identity.write_text("public release fixture", encoding="ascii")
            public_identity.chmod(0o600)
            for target, arch in (("x86_64-unknown-linux-gnu", "amd64"), ("aarch64-unknown-linux-gnu", "arm64")):
                installers.installed_marker(source, "0.1.1", target)
                destination = root / arch
                self.assertEqual(installers.linux_tree(source, destination, "0.1.1", target), arch)
                control = (destination / "DEBIAN/control").read_text(encoding="utf-8")
                self.assertIn("Architecture: " + arch, control)
                for dependency in ("bluez", "network-manager", "usbmuxd", "libc6 (>= 2.39)"):
                    self.assertIn(dependency, control)
                wrapper = destination / "usr/bin/rustcarplay"
                self.assertIn('exec /opt/rustcarplay/RustCarPlay "$@"', wrapper.read_text())
                if not installers.os.name == "nt":
                    self.assertTrue(wrapper.stat().st_mode & stat.S_IXUSR)
                    self.assertFalse(wrapper.stat().st_mode & stat.S_IWOTH)
                    self.assertEqual((destination / "opt/rustcarplay/resources/auth/identity.pk8").stat().st_mode & 0o777, 0o644)
                self.assertTrue((destination / "opt/rustcarplay/INSTALLATION.json").is_file())
                self.assertTrue((destination / "usr/share/applications/rustcarplay.desktop").is_file())
                self.assertFalse((destination / "opt/rustcarplay/.local").exists())
                self.assertFalse((destination / "DEBIAN/postinst").exists())
                self.assertFalse((destination / "DEBIAN/postrm").exists())

    def test_installer_refuses_cross_os_build_or_verification(self):
        with patch.object(installers.platform, "system", return_value="Linux"), patch.object(installers.platform, "machine", return_value="x86_64"):
            self.assertEqual(installers.require_native("x86_64-unknown-linux-gnu"), "Linux")
            with self.assertRaisesRegex(RuntimeError, "native operating system"):
                installers.require_native("x86_64-pc-windows-msvc")
            with self.assertRaisesRegex(RuntimeError, "native operating system"):
                installers.require_native("aarch64-unknown-linux-gnu")


if __name__ == "__main__":
    unittest.main()
