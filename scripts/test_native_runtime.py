"""Packaging regressions that require neither native SDKs nor network access."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import zstandard
import native_runtime_sources as sources

SPEC = importlib.util.spec_from_file_location("native_bundle", Path(__file__).with_name("bundle-native-runtime.py"))
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)

LINUX_SPEC = importlib.util.spec_from_file_location("ci_linux_native", Path(__file__).with_name("ci-linux-native.py"))
linux_ci = importlib.util.module_from_spec(LINUX_SPEC)
LINUX_SPEC.loader.exec_module(linux_ci)


class NativeRuntimeTests(unittest.TestCase):
    def test_apt_source_uris_preserve_mirror_transports_and_validate_records(self):
        uris = ["mirror+file:/etc/apt/apt-mirrors.txt/pool/main/f/freetype/freetype_1.0.dsc",
                "mirror+http://example.invalid/mirrors/pool/main/f/freetype/freetype_1.0.orig.tar.xz",
                "mirror+https://example.invalid/mirrors/pool/main/f/freetype/freetype_1.0.debian.tar.xz",
                "http://ports.ubuntu.com/ubuntu-ports/pool/main/a/alsa-lib/alsa-lib_1.0.dsc",
                "https://archive.ubuntu.com/ubuntu/pool/main/a/alsa-lib/alsa-lib_1.0.dsc"]
        output = "Reading package lists...\n" + "\n".join(f"'{uri}' file-{index}.tar.xz 123 SHA256:abcd" for index, uri in enumerate(uris))
        self.assertEqual(sources.parse_apt_source_uris(output), uris)
        for record, message in (("'http://example.invalid/f' ../f 12 SHA256:abcd", "Invalid apt source filename"),
                                ("'javascript:bad' f 12 SHA256:abcd", "Unsupported apt source transport"),
                                ("'http://example.invalid/f' f invalid SHA256:abcd", "Malformed apt source URI record"),
                                ("Reading package lists...\n", "no source locations")):
            with self.subTest(record=record), self.assertRaisesRegex(RuntimeError, message):
                sources.parse_apt_source_uris(record)

    def test_apt_source_query_pins_source_version_in_an_empty_directory(self):
        def query(command, **kwargs):
            self.assertIn("freetype=2.13.2+dfsg-1ubuntu0.2", command)
            self.assertIn("--only-source", command)
            self.assertEqual(command[-1], "--print-uris")
            self.assertEqual(list(Path(kwargs["cwd"]).iterdir()), [])
            return "'mirror+file:/etc/apt/apt-mirrors.txt/pool/freetype_2.13.2.dsc' freetype_2.13.2.dsc 100 SHA256:abcd\n"
        with patch.object(sources.subprocess, "check_output", side_effect=query):
            self.assertEqual(len(sources.apt_source_locations("freetype", "2.13.2+dfsg-1ubuntu0.2")), 1)

    def test_debian_source_descriptor_must_match_exact_binary_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            payload = folder / "freetype.tar.xz"
            payload.write_bytes(b"synthetic complete source")
            descriptor = folder / "freetype.dsc"
            descriptor.write_text("Source: freetype\nVersion: 2.13.2+dfsg-1ubuntu0.2\nChecksums-Sha256:\n "
                                  + sources.sha256(payload) + " " + str(payload.stat().st_size) + " freetype.tar.xz\n", encoding="utf-8")
            self.assertEqual(len(sources.dsc_artifacts(folder, ("freetype", "2.13.2+dfsg-1ubuntu0.2"))), 2)
            with self.assertRaisesRegex(RuntimeError, "does not match"):
                sources.dsc_artifacts(folder, ("freetype", "2.13.2+dfsg-1ubuntu0.1"))

    def test_debian_mirror_source_bundle_records_urls_mirror_choices_and_exact_payloads(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            mirror = root / "apt-mirrors.txt"
            mirror.write_text("# fixture mirror list\nhttps://archive.ubuntu.com/ubuntu/\tpriority:1\nhttp://azure.archive.ubuntu.com/ubuntu/\tpriority:2\n", encoding="utf-8")
            uri = "mirror+file:" + mirror.as_posix() + "/pool/main/f/fixture/fixture_1.0-2.dsc"
            locations = "'" + uri + "' fixture_1.0-2.dsc 100 SHA256:abcd\n"
            def download(command, **kwargs):
                self.assertEqual(command[-1], "fixture=1.0-2")
                folder = Path(kwargs["cwd"])
                payload = folder / "fixture_1.0.orig.tar.xz"
                payload.write_bytes(b"synthetic source archive")
                (folder / "fixture_1.0-2.dsc").write_text("Source: fixture\nVersion: 1.0-2\nChecksums-Sha256:\n "
                    + sources.sha256(payload) + " " + str(payload.stat().st_size) + " " + payload.name + "\n", encoding="utf-8")
            packages = [{"binary_package": "libfixture1:amd64", "binary_version": "1.0-2", "source_package": "fixture", "source_version": "1.0-2"}]
            with patch.object(sources.subprocess, "check_output", return_value=locations), \
                 patch.object(sources.subprocess, "run", side_effect=download) as command:
                result = sources.debian_sources(packages, root / "cache", root / "dist", "0.1.1", "linux-x86_64")
            command.assert_called_once()
            with tarfile.open(root / "dist" / result["file"]) as archive:
                manifest = json.load(archive.extractfile("SOURCE-MANIFEST.json"))
                self.assertEqual(manifest["binary_packages"], packages)
                source = manifest["source_packages"][0]
                self.assertEqual(source["version"], "1.0-2")
                self.assertEqual(source["urls"], [uri])
                self.assertEqual(len(source["apt_mirror_lists"][0]["entries"]), 2)
                self.assertEqual(len(source["files"]), 2)
                self.assertEqual(len(archive.getmembers()), 3)

    def test_linux_preflight_follows_actual_elf_owners_without_upgrading_system_baseline(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            plugin, codec, config = [root / name for name in ("plugin.so", "libcodec.so.1", "alsa.conf")]
            for path in (plugin, codec, config):
                path.write_bytes(b"fixture")
            owners = {plugin: "plugins:amd64", codec: "codec:amd64", config: "alsa-data"}
            def run(command):
                if command[0] == "ldd":
                    if command[1] == str(plugin):
                        # Use the ldd parser fixture below to avoid host path conventions.
                        return "plugin dependencies"
                    return "codec dependencies"
                self.assertEqual(command[0], "dpkg-query")
                binary = command[-1]
                return "\t".join((binary, "1.0", binary.split(":")[0], "1.0"))
            def dependencies(output):
                if output == "plugin dependencies":
                    return {"libcodec.so.1": codec, "libc.so.6": root / "system-libc", "libEGL.so.1": root / "system-driver"}
                return {}
            with patch.object(linux_ci, "native_inputs", return_value=([plugin], [config])), \
                 patch.object(linux_ci.bundle, "dpkg_owner", side_effect=lambda path: owners[path]) as owner, \
                 patch.object(linux_ci.bundle, "parse_ldd", side_effect=dependencies), \
                 patch.object(linux_ci, "run", side_effect=run):
                packages = linux_ci.installed_packages("x86_64-unknown-linux-gnu")
            self.assertEqual({package["binary_package"] for package in packages}, {"plugins:amd64", "codec:amd64", "alsa-data"})
            self.assertEqual(owner.call_count, 3)

    def test_linux_alignment_upgrades_only_owners_of_unavailable_exact_sources(self):
        old = {"binary_package": "libfreetype6:arm64", "binary_version": "1.0-1", "source_package": "freetype", "source_version": "1.0-1"}
        new = {**old, "binary_version": "1.0-2", "source_version": "1.0-2"}
        unchanged = {"binary_package": "libopus0:arm64", "binary_version": "1.0", "source_package": "opus", "source_version": "1.0"}
        introduced = {"binary_package": "libbrotli1:arm64", "binary_version": "2.0", "source_package": "brotli", "source_version": "2.0"}
        events = []
        def locations(name, version):
            events.append(("source", name, version))
            if (name, version) == ("freetype", "1.0-1"):
                raise subprocess.CalledProcessError(100, ["apt-get"])
            return ["http://example.invalid/" + name + "_" + version + ".dsc"]
        def upgrade(packages):
            self.assertEqual(packages, [new])
            self.assertIn(("source", "freetype", "1.0-2"), events)
            events.append(("upgrade",))
        with patch.object(linux_ci, "installed_packages", side_effect=[[old, unchanged], [new, unchanged, introduced]]), \
             patch.object(linux_ci, "candidate_package", return_value=new) as candidate, \
             patch.object(linux_ci, "apt_source_locations", side_effect=locations), \
             patch.object(linux_ci, "upgrade_packages", side_effect=upgrade):
            report = linux_ci.align_sources("aarch64-unknown-linux-gnu", True)
        candidate.assert_called_once_with(old)
        self.assertEqual(report["binary_packages"], [new, unchanged, introduced])
        self.assertIn(("source", "brotli", "2.0"), events)
        self.assertNotIn({"name": "freetype", "version": "1.0-1"}, report["sources"])
        self.assertEqual(len(report["upgrades"]), 1)

    def test_linux_alignment_never_upgrades_to_a_candidate_without_exact_source(self):
        package = {"binary_package": "libfreetype6:amd64", "binary_version": "1", "source_package": "freetype", "source_version": "1"}
        new = {**package, "binary_version": "2", "source_version": "2"}
        with patch.object(linux_ci, "installed_packages", return_value=[package]), \
             patch.object(linux_ci, "candidate_package", return_value=new), \
             patch.object(linux_ci, "apt_source_locations", side_effect=subprocess.CalledProcessError(100, ["apt-get"])), \
             patch.object(linux_ci, "upgrade_packages") as upgrade:
            with self.assertRaisesRegex(RuntimeError, "Exact source versions unavailable"):
                linux_ci.align_sources("x86_64-unknown-linux-gnu", False)
            with self.assertRaises(subprocess.CalledProcessError):
                linux_ci.align_sources("x86_64-unknown-linux-gnu", True)
            upgrade.assert_not_called()

    def test_linux_candidate_metadata_tracks_source_versions_and_rejects_no_upgrade(self):
        package = {"binary_package": "libfreetype6:arm64", "binary_version": "1.0-1", "source_package": "freetype", "source_version": "1.0-1"}
        with patch.object(linux_ci, "run", side_effect=["  Candidate: 1.0-2+b1\n", "Package: libfreetype6\nVersion: 1.0-2+b1\nSource: freetype (1.0-2)\n"]), \
             patch.object(linux_ci.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)):
            result = linux_ci.candidate_package(package)
            self.assertEqual(result["binary_version"], "1.0-2+b1")
            self.assertEqual(result["source_version"], "1.0-2")
        with patch.object(linux_ci, "run", return_value="  Candidate: 1.0-1\n"), \
             patch.object(linux_ci.subprocess, "run", return_value=subprocess.CompletedProcess([], 1)):
            with self.assertRaisesRegex(RuntimeError, "No newer candidate"):
                linux_ci.candidate_package(package)

    def test_linux_upgrade_is_ci_only_pinned_and_cannot_remove_packages(self):
        package = {"binary_package": "libfreetype6:amd64", "binary_version": "1.0-2"}
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}), patch.object(linux_ci.subprocess, "run") as command:
            with self.assertRaisesRegex(RuntimeError, "restricted to GitHub Actions"):
                linux_ci.upgrade_packages([package])
            command.assert_not_called()
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "true"}), patch.object(linux_ci.subprocess, "run") as command:
            linux_ci.upgrade_packages([package])
            args = command.call_args.args[0]
            self.assertEqual(args[-1], "libfreetype6:amd64=1.0-2")
            for flag in ("--only-upgrade", "--no-remove", "--no-install-recommends"):
                self.assertIn(flag, args)
            self.assertNotIn("upgrade", args)

    @staticmethod
    def msys_zstd_fixture() -> bytes:
        original = io.BytesIO()
        with tarfile.open(fileobj=original, mode="w") as archive:
            for name, body in (("package/PKGBUILD", b"exit 99 # never execute"),
                               ("package/upstream.tar.gz", b"fixture upstream source")):
                member = tarfile.TarInfo(name)
                member.size = len(body)
                archive.addfile(member, io.BytesIO(body))
        parameters = zstandard.ZstdCompressionParameters.from_level(
            1, write_content_size=0, write_checksum=1)
        frame = bytearray(zstandard.ZstdCompressor(compression_params=parameters).compress(original.getvalue()))
        # With no content-size field this is Window_Descriptor. Requiring a
        # 128 MiB window reproduces the real MSYS2 frame without a huge fixture.
        frame[5] = 0x88
        assert zstandard.get_frame_parameters(frame).window_size == 128 * 1024 * 1024
        return bytes(frame)

    def test_msys_large_window_source_uses_verified_python_decoder(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "fixture.src.tar.zst"
            path.write_bytes(self.msys_zstd_fixture())
            with patch.object(sources.subprocess, "check_output") as external:
                result = sources.validate_msys_allsource(path)
            self.assertEqual(result, {"upstream_archives": ["package/upstream.tar.gz"]})
            external.assert_not_called()

    def test_msys_zstd_rejects_incomplete_frame_checksum_and_trailing_data(self):
        frame = self.msys_zstd_fixture()
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "fixture.src.tar.zst"
            for payload, message in ((frame[:-1], "Incomplete Zstandard"),
                                     (frame[:-1] + bytes([frame[-1] ^ 1]), "checksum"),
                                     (frame + b"unexpected", "trailing data")):
                with self.subTest(message=message):
                    path.write_bytes(payload)
                    with self.assertRaisesRegex(RuntimeError, message):
                        sources.validate_msys_allsource(path)

    def test_msys_zstd_bounds_frame_window_and_expanded_disk_usage(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "fixture.src.tar.zst"
            frame = bytearray(self.msys_zstd_fixture())
            frame[5] = 0xA8  # Advertise 2 GiB, before any allocation is attempted.
            path.write_bytes(frame)
            with self.assertRaisesRegex(RuntimeError, "window exceeds"):
                sources.validate_msys_allsource(path)
            path.write_bytes(self.msys_zstd_fixture())
            with patch.object(sources, "MAX_SOURCE_TAR", 1024), self.assertRaisesRegex(RuntimeError, "Expanded source tar exceeds"):
                sources.validate_msys_allsource(path)

    def test_source_download_never_promotes_truncated_http_response(self):
        def response(*args, **kwargs):
            stream = io.BytesIO(b"truncated")
            stream.headers = {"Content-Length": "1024"}
            return stream
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "source.tar.zst"
            with patch.object(sources.urllib.request, "urlopen", side_effect=response) as requests, \
                 patch.object(sources.time, "sleep"), self.assertRaisesRegex(OSError, "expected 1024 bytes, received 9"):
                sources.download("https://example.invalid/source.tar.zst", path)
            self.assertEqual(requests.call_count, 3)
            self.assertFalse(path.exists())

    @unittest.skipUnless(shutil.which("git"), "Git is needed for bare source verification")
    def test_msys_vcs_source_is_verified_offline_without_running_recipe(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            repository = folder / "upstream"
            repository.mkdir()
            subprocess.run(["git", "init", "-q", str(repository)], check=True)
            (repository / "COPYING").write_text("fixture source license\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(repository), "add", "COPYING"], check=True)
            subprocess.run(["git", "-C", str(repository), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                            "-c", "commit.gpgsign=false", "-c", "core.hooksPath=", "commit", "-qm", "fixture"], check=True)
            commit = subprocess.check_output(["git", "-C", str(repository), "rev-parse", "HEAD"], text=True).strip()
            package = folder / "package"
            package.mkdir()
            (package / "PKGBUILD").write_text("exit 99 # must never execute\n", encoding="utf-8")
            (package / ".SRCINFO").write_text("source = upstream::git+https://example.invalid/upstream#commit=" + commit + "\n", encoding="utf-8")
            archive_path = folder / "allsource.tar"
            with tarfile.open(archive_path, "w") as archive:
                archive.add(package, arcname="package")
                archive.add(repository / ".git", arcname="package/upstream")
            result = sources.validate_msys_allsource(archive_path)
            self.assertEqual(result, {"git_commit": commit, "git_source_complete": True})
            with tarfile.open(archive_path, "w") as archive:
                archive.add(package, arcname="package")
            with self.assertRaisesRegex(RuntimeError, "no complete upstream"):
                sources.validate_msys_allsource(archive_path)

    def test_elf_closure_preserves_dependency_names_and_rejects_missing_libraries(self):
        parsed = bundle.parse_ldd("""linux-vdso.so.1 (0x1234)
        libgstapp-1.0.so.0 => /lib/x86_64-linux-gnu/libgstapp-1.0.so.0 (0x1234)
        /lib64/ld-linux-x86-64.so.2 (0x1234)
        """)
        self.assertEqual(parsed["libgstapp-1.0.so.0"], Path("/lib/x86_64-linux-gnu/libgstapp-1.0.so.0"))
        self.assertIn("ld-linux-x86-64.so.2", parsed)
        self.assertTrue(bundle.LINUX_SYSTEM.fullmatch("libc.so.6"))
        self.assertTrue(bundle.LINUX_SYSTEM.fullmatch("libEGL.so.1"))
        self.assertFalse(bundle.LINUX_SYSTEM.fullmatch("libglib-2.0.so.0"))
        self.assertFalse(bundle.LINUX_SYSTEM.fullmatch("libstdc++.so.6"))
        with self.assertRaisesRegex(RuntimeError, "Unresolved ELF dependency"):
            bundle.parse_ldd("libgstapp-1.0.so.0 => not found")

    def test_pe_reads_eager_and_delay_imports_without_loading_binary(self):
        data = bytearray(2048)
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 0x3C, 0x80)
        data[0x80:0x84] = b"PE\0\0"
        struct.pack_into("<HH", data, 0x84, 0x8664, 1)
        struct.pack_into("<H", data, 0x94, 240)
        optional = 0x98
        struct.pack_into("<H", data, optional, 0x20B)
        struct.pack_into("<II", data, optional + 112 + 8, 0x1100, 40)
        struct.pack_into("<II", data, optional + 112 + 13 * 8, 0x1200, 64)
        struct.pack_into("<IIII", data, optional + 240 + 8, 1024, 0x1000, 1024, 512)
        struct.pack_into("<I", data, 0x300 + 12, 0x1300)
        struct.pack_into("<II", data, 0x400, 1, 0x1310)
        data[0x500:0x500 + 13] = b"KERNEL32.dll\0"
        data[0x510:0x510 + 11] = b"codec.dll\0\0"
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "probe.dll"
            path.write_bytes(data)
            self.assertEqual(bundle.pe_imports(path), {"kernel32.dll", "codec.dll"})
        self.assertTrue(bundle.windows_system_dll("api-ms-win-core-file-l1-1-0.dll"))
        self.assertFalse(bundle.windows_system_dll("vcruntime140.dll"))
        self.assertFalse(bundle.windows_system_dll("gstapp-1.0-0.dll"))

    def test_complete_debian_source_requires_all_hashed_payloads(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            payload = b"complete original source archive"
            name = "library_1.0.orig.tar.xz"
            (folder / name).write_bytes(payload)
            descriptor = folder / "library_1.0-1.dsc"
            descriptor.write_text(f"Format: 3.0 (quilt)\nChecksums-Sha256:\n {hashlib.sha256(payload).hexdigest()} {len(payload)} {name}\n", encoding="utf-8")
            self.assertEqual(len(sources.dsc_artifacts(folder)), 2)
            (folder / name).write_bytes(b"wrong archive")
            with self.assertRaisesRegex(RuntimeError, "Missing or corrupt"):
                sources.dsc_artifacts(folder)
            descriptor.write_text("Checksums-Sha256:\n " + "0" * 64 + " 1 ../outside.tar.xz\n", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "Invalid Debian source filename"):
                sources.dsc_artifacts(folder)

    def test_nested_corresponding_source_licenses_and_corrupt_cache_recovery(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            archive_path = folder / "source.tar.xz"
            with tarfile.open(archive_path, "w:xz") as archive:
                for component in ("ffmpeg-1.0", "x264-1.0", "x265-1.0"):
                    nested = io.BytesIO()
                    with tarfile.open(fileobj=nested, mode="w:gz") as inner:
                        body = (component + " complete license\n").encode()
                        info = tarfile.TarInfo(component + "/COPYING")
                        info.size = len(body)
                        inner.addfile(info, io.BytesIO(body))
                    payload = nested.getvalue()
                    info = tarfile.TarInfo("cerbero/sources/" + component + "/source.tar.gz")
                    info.size = len(payload)
                    archive.addfile(info, io.BytesIO(payload))
            destination = folder / "licenses"
            result = sources.collect_cerbero_licenses(archive_path, destination)
            self.assertEqual(len(result["licenses"]), 3)
            item = result["licenses"][0]
            (destination / item["file"]).write_bytes(b"corrupt cached license")
            sources.collect_cerbero_licenses(archive_path, destination)
            self.assertEqual(sources.sha256(destination / item["file"]), item["sha256"])

    def test_universal_macho_sdk_rpaths_are_deduplicated(self):
        output = """Load command 1
          cmd LC_RPATH
      cmdsize 96
         path /Library/Frameworks/GStreamer.framework/Versions/1.0/lib (offset 12)
Load command 1
          cmd LC_RPATH
      cmdsize 96
         path /Library/Frameworks/GStreamer.framework/Versions/1.0/lib (offset 12)
"""
        with patch.object(bundle, "run", return_value=output):
            self.assertEqual(bundle.macho_rpaths(Path("universal.dylib")), ["/Library/Frameworks/GStreamer.framework/Versions/1.0/lib"])

    def test_macho_rpath_uses_declared_nested_backend_before_top_level_library(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            prefix, destination = folder / "SDK", folder / "runtime/gstreamer"
            nested = destination / "lib/libproxy/libpxbackend-1.0.dylib"
            nested.parent.mkdir(parents=True)
            nested.write_bytes(b"nested backend")
            (destination / "lib/libpxbackend-1.0.dylib").write_bytes(b"wrong same-name library")
            image = destination / "lib/libproxy.1.dylib"
            for rpath in (str(prefix / "lib/libproxy"), "@loader_path/libproxy",
                          "/Library/Frameworks/GStreamer.framework/Versions/Current/lib/libproxy"):
                with self.subTest(rpath=rpath):
                    resolved = bundle.resolve_macho_dependency(
                        "@rpath/libpxbackend-1.0.dylib", image,
                        [rpath], prefix, destination)
                    self.assertEqual(resolved, nested.resolve())

    def test_macho_rpath_order_and_inherited_common_library_path(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            prefix, destination = folder / "SDK", folder / "runtime/gstreamer"
            for subdirectory in ("first", "second", ""):
                path = destination / "lib" / subdirectory / "libfixture.dylib"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(subdirectory.encode() or b"common")
            image = destination / "lib/libconsumer.dylib"
            ordered = [str(prefix / "lib/second"), str(prefix / "lib/first")]
            self.assertEqual(bundle.resolve_macho_dependency(
                "@rpath/libfixture.dylib", image, ordered, prefix, destination),
                (destination / "lib/second/libfixture.dylib").resolve())
            self.assertEqual(bundle.resolve_macho_dependency(
                "@rpath/libfixture.dylib", image, [], prefix, destination),
                (destination / "lib/libfixture.dylib").resolve())

    def test_macho_never_resolves_a_missing_private_library_from_the_sdk(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            prefix, destination = folder / "SDK", folder / "runtime/gstreamer"
            original = prefix / "lib/libproxy/libpxbackend-1.0.dylib"
            original.parent.mkdir(parents=True)
            original.write_bytes(b"only installed on developer machine")
            image = destination / "lib/libproxy.1.dylib"
            for dependency, rpaths in (
                ("@rpath/libpxbackend-1.0.dylib", [str(original.parent)]),
                (str(original), []),
                ("@loader_path/../../../SDK/lib/libproxy/libpxbackend-1.0.dylib", []),
            ):
                with self.subTest(dependency=dependency), self.assertRaisesRegex(RuntimeError, "private Mach-O"):
                    bundle.resolve_macho_dependency(dependency, image, rpaths, prefix, destination)

    def test_macho_loader_reference_normalizes_short_name_and_symlink_aliases(self):
        # Model the real filesystem canonicalization deterministically on all
        # hosts: Windows may disable 8.3 names or require symlink privileges.
        # The hosted Windows integration test also uses its actual TEMP alias.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            resolve = Path.resolve
            for alias_parts, canonical_parts in (("RUNNER~1/Temp", "runneradmin/Temp"),
                                                  ("var/folders", "private/var/folders")):
                alias = root / alias_parts
                canonical = root / canonical_parts
                image = alias / "package/runtime/gstreamer/lib/libproxy.1.dylib"
                dependency = canonical / "package/runtime/gstreamer/lib/libproxy/libpxbackend-1.0.dylib"
                def normalize(path, *args, **kwargs):
                    if path.is_relative_to(alias):
                        path = canonical / path.relative_to(alias)
                    return resolve(path, *args, **kwargs)
                with self.subTest(alias=alias_parts), patch.object(Path, "resolve", autospec=True, side_effect=normalize):
                    result = bundle.macho_loader_reference(image, dependency)
                self.assertEqual(result, "@loader_path/libproxy/libpxbackend-1.0.dylib")
                # Changing the installation directory must preserve the name.
                moved = root / "other-install-location"
                self.assertEqual(bundle.macho_loader_reference(moved / "runtime/gstreamer/lib/libproxy.1.dylib",
                    moved / "runtime/gstreamer/lib/libproxy/libpxbackend-1.0.dylib"), result)

    def test_macho_loader_reference_keeps_app_to_private_runtime_path_portable(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            image = root / "package/app/carplay-desktop"
            dependency = root / "package/runtime/gstreamer/lib/libgstreamer-1.0.dylib"
            result = bundle.macho_loader_reference(image, dependency)
            self.assertEqual(result, "@loader_path/../runtime/gstreamer/lib/libgstreamer-1.0.dylib")

    def test_macos_relocation_rewrites_nested_libproxy_and_removes_sdk_rpath(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            prefix, runtime = folder / "SDK", folder / "runtime"
            for name in ("lib/libproxy.1.dylib", "lib/libproxy/libpxbackend-1.0.dylib",
                         "lib/gstreamer-1.0/libgstosxaudio.dylib", "bin/gst-inspect-1.0",
                         "bin/gst-launch-1.0", "libexec/gstreamer-1.0/gst-plugin-scanner"):
                path = prefix / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"\xcf\xfa\xed\xfe" + b"fixture")
            original_rpath = str(prefix / "lib/libproxy")
            dependency = "@rpath/libpxbackend-1.0.dylib"
            with patch.object(bundle.platform, "system", return_value="Darwin"), \
                    patch.object(bundle, "PLUGINS", ()), \
                    patch.object(bundle, "macho_dependencies", side_effect=lambda path: {dependency} if path.name == "libproxy.1.dylib" else set()), \
                    patch.object(bundle, "macho_rpaths", side_effect=lambda path: [original_rpath] if path.name == "libproxy.1.dylib" else []), \
                    patch.object(bundle, "run", return_value="") as commands:
                bundle.macos_runtime(prefix, runtime, [])
            relocation = next(call.args[0] for call in commands.call_args_list
                              if call.args[0][0] == "install_name_tool"
                              and call.args[0][-1].name == "libproxy.1.dylib")
            self.assertEqual(relocation[1:4], ["-change", dependency, "@loader_path/libproxy/libpxbackend-1.0.dylib"])
            self.assertIn("-delete_rpath", relocation)
            self.assertIn(original_rpath, relocation)

    def test_macos_nested_dylib_install_id_is_not_treated_as_a_dependency(self):
        with tempfile.TemporaryDirectory() as temporary:
            folder = Path(temporary)
            prefix, runtime = folder / "SDK", folder / "runtime"
            for name in ("lib/libproxy/libpxbackend-1.0.dylib",
                         "lib/gstreamer-1.0/libgstosxaudio.dylib", "bin/gst-inspect-1.0",
                         "bin/gst-launch-1.0", "libexec/gstreamer-1.0/gst-plugin-scanner"):
                path = prefix / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"\xcf\xfa\xed\xfe" + b"fixture")
            install_id = "@rpath/libpxbackend-1.0.dylib"

            def commands(arguments):
                if arguments[:2] == ["otool", "-D"] and arguments[-1].name == "libpxbackend-1.0.dylib":
                    return f"{arguments[-1]} (architecture x86_64):\n{install_id}\n{arguments[-1]} (architecture arm64):\n{install_id}\n"
                return ""

            with patch.object(bundle.platform, "system", return_value="Darwin"), \
                    patch.object(bundle, "PLUGINS", ()), \
                    patch.object(bundle, "macho_dependencies", side_effect=lambda path: {install_id} if path.name == "libpxbackend-1.0.dylib" else set()), \
                    patch.object(bundle, "macho_rpaths", return_value=[]), \
                    patch.object(bundle, "run", side_effect=commands) as calls:
                bundle.macos_runtime(prefix, runtime, [])
            relocation = next(call.args[0] for call in calls.call_args_list
                              if call.args[0][0] == "install_name_tool"
                              and call.args[0][-1].name == "libpxbackend-1.0.dylib")
            self.assertEqual(relocation[1:-1], ["-id", "@loader_path/libpxbackend-1.0.dylib"])


if __name__ == "__main__":
    unittest.main()
