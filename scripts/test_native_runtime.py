"""Packaging regressions that require neither native SDKs nor network access."""
import hashlib
import importlib.util
import io
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import native_runtime_sources as sources

SPEC = importlib.util.spec_from_file_location("native_bundle", Path(__file__).with_name("bundle-native-runtime.py"))
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)


class NativeRuntimeTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
