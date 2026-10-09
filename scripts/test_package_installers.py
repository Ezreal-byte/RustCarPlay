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
    def test_macos_single_bundle_launcher_is_inspected_without_requiring_three_apps(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "Contents/MacOS"
            directory.mkdir(parents=True)
            launcher = directory / "RustCarPlay"
            launcher.write_bytes(next(iter(installers.portable.MACHO_MAGICS)) + b"fixture native code")
            with patch.object(installers.portable, "run_captured", return_value=b"/usr/lib/libSystem.B.dylib") as inspect:
                with self.assertRaisesRegex(installers.portable.VerificationError, "expected Mach-O"):
                    installers.portable.verify_macos_dependencies(directory, {})
                inspect.assert_not_called()
                installers.portable.verify_macos_dependencies(directory, {}, minimum_binaries=1)
                self.assertIn(str(launcher.resolve()), inspect.call_args.args[0])
            with patch.object(installers.portable, "run_captured", return_value=b"/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/libgstreamer.dylib"):
                with self.assertRaisesRegex(installers.portable.VerificationError, "build machine"):
                    installers.portable.verify_macos_dependencies(directory, {}, minimum_binaries=1)
            launcher.unlink()
            with self.assertRaisesRegex(installers.portable.VerificationError, "expected Mach-O"):
                installers.portable.verify_macos_dependencies(directory, {}, minimum_binaries=1)

    def test_macos_keeps_resources_out_of_the_code_only_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = root / "portable"
            package.mkdir()
            for name, data in (("RustCarPlay", b"fixture launcher"), ("LICENSE", b"fixture license"),
                               ("app/carplay-desktop", b"fixture desktop"),
                               ("runtime/gstreamer/lib/codec.dylib", b"fixture signed codec"),
                               ("resources/auth/identity.pk8", b"public release fixture")):
                path = package / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            installers.installed_marker(package, "0.1.1", "aarch64-apple-darwin")
            destination = root / "image"
            destination.mkdir()
            commands = []
            with patch.object(installers, "run", side_effect=lambda command, **kwargs: commands.append(command)), \
                 patch.object(installers.portable, "validate_runtime_manifest") as manifest:
                app = installers.macos_app(package, destination, "0.1.1")
            executables = app / "Contents/MacOS"
            payload = app / "Contents/Resources/payload"
            self.assertEqual({path.name for path in executables.iterdir()}, {"RustCarPlay"})
            self.assertEqual((executables / "RustCarPlay").read_bytes(), b"fixture launcher")
            self.assertFalse((payload / "RustCarPlay").exists())
            self.assertEqual((payload / "LICENSE").read_bytes(), b"fixture license")
            self.assertEqual((payload / "runtime/gstreamer/lib/codec.dylib").read_bytes(), b"fixture signed codec")
            self.assertTrue((payload / "app/carplay-desktop").is_file())
            self.assertTrue((payload / "INSTALLATION.json").is_file())
            manifest.assert_called_once_with(payload, "aarch64-apple-darwin")
            signing = [command for command in commands if command[0] == "/usr/bin/codesign"]
            self.assertEqual(len(signing), 2)
            self.assertNotIn("--deep", signing[0])  # Never re-sign and alter the runtime manifest.
            self.assertIn("--deep", signing[1])

    def test_inno_lookup_uses_real_installation_instead_of_chocolatey_shim(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            shim = root / "chocolatey/bin/ISCC.exe"
            compiler = root / "programs/Inno Setup 6/ISCC.exe"
            for path in (shim, compiler):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"fixture compiler")
            (compiler.parent / "License.txt").write_text("fixture redistribution license", encoding="utf-8")
            environment = {"ISCC_PATH": "", "ProgramFiles(x86)": str(root / "programs"),
                           "ProgramFiles": str(root / "programs64")}
            with patch.dict(installers.os.environ, environment), patch.object(installers.shutil, "which", return_value=str(shim)):
                self.assertEqual(installers.find_iscc(), compiler)
                # An explicit override is respected and must include licensing;
                # never silently pair a different compiler with this license.
                with patch.dict(installers.os.environ, {"ISCC_PATH": str(shim)}):
                    with self.assertRaisesRegex(RuntimeError, "not a PATH shim"):
                        installers.find_iscc()
                (compiler.parent / "License.txt").unlink()
                with self.assertRaisesRegex(RuntimeError, "adjacent License.txt"):
                    installers.find_iscc()

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
