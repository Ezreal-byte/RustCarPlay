"""Synthetic ZIP tests; no real authentication material or network access."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import Mock, patch
import warnings
import zipfile

SPEC = importlib.util.spec_from_file_location("prepare_release_auth", Path(__file__).with_name("prepare-release-auth.py"))
auth = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(auth)

KEY = b"synthetic non-key bytes"
CERT = b"synthetic non-certificate bytes"


def archive_bytes(entries=None):
    output = io.BytesIO()
    entries = entries if entries is not None else [(auth.KEY_ENTRY, KEY), (auth.CERT_ENTRY, CERT)]
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", UserWarning)
        with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name, value in entries:
                if isinstance(name, str):
                    # Preserve malicious wire names even on Windows, where
                    # ZipInfo's constructor normally rewrites backslashes.
                    info = zipfile.ZipInfo(name)
                    info.filename = info.orig_filename = name
                    info.compress_type = zipfile.ZIP_DEFLATED
                else:
                    info = name
                archive.writestr(info, value)
    return output.getvalue()


class Response(io.BytesIO):
    def __init__(self, data, declared=None, url=auth.APK_URL):
        super().__init__(data)
        self.headers = {} if declared is None else {"Content-Length": str(declared)}
        self.url = url

    def geturl(self):
        return self.url


class PrepareReleaseAuthTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.apk = self.root / "fixture.apk"
        self.output = self.root / "auth"

    def prepare(self, data=None, **kwargs):
        data = archive_bytes() if data is None else data
        self.apk.write_bytes(data)
        return auth.prepare_auth(self.apk, self.output,
                                 expected_sha256=hashlib.sha256(data).hexdigest(), **kwargs)

    def assert_no_staging(self):
        self.assertFalse(list(self.root.glob(".*.stage-*")))
        self.assertFalse(list(self.root.glob(".diplay-apk-*.part")))

    def test_extracts_only_two_entries_and_non_secret_provenance(self):
        data = archive_bytes([(auth.KEY_ENTRY, KEY), (auth.CERT_ENTRY, CERT),
                              ("assets/not-selected.txt", b"unrelated"), ("classes.dex", b"ignored")])
        result = self.prepare(data)
        self.assertEqual(result["ok"], True)
        self.assertEqual(result["unchanged"], False)
        self.assertEqual({p.name for p in self.output.iterdir()},
                         {"identity.pk8", "certificate.p7b", "provenance.json"})
        self.assertEqual((self.output / "identity.pk8").read_bytes(), KEY)
        self.assertEqual((self.output / "certificate.p7b").read_bytes(), CERT)
        provenance = (self.output / "provenance.json").read_bytes()
        record = json.loads(provenance)
        self.assertEqual(record["upstream_url"], auth.APK_URL)
        self.assertEqual(record["upstream_version"], "v0.2.15")
        self.assertEqual(record["apk_sha256"], hashlib.sha256(data).hexdigest())
        self.assertIn("Allwinner V821", record["source_firmware_origin"])
        self.assertIn("unresolved", record["experimental_status"])
        self.assertNotIn(KEY, provenance)
        self.assertNotIn(CERT, provenance)
        self.assert_no_staging()

    def test_identical_output_is_idempotent_without_rewriting_files(self):
        self.prepare()
        before = {p.name: p.stat().st_mtime_ns for p in self.output.iterdir()}
        self.assertTrue(self.prepare()["unchanged"])
        self.assertEqual(before, {p.name: p.stat().st_mtime_ns for p in self.output.iterdir()})
        self.assert_no_staging()

    def test_wrong_full_apk_digest_is_rejected_before_extraction(self):
        self.apk.write_bytes(archive_bytes())
        with self.assertRaises(auth.PreparationError):
            auth.prepare_auth(self.apk, self.output)
        self.assertFalse(self.output.exists())
        self.assert_no_staging()

    def test_local_apk_size_limit(self):
        with self.assertRaises(auth.PreparationError):
            self.prepare(max_apk_bytes=8)
        self.assertFalse(self.output.exists())

    def test_missing_and_empty_entries(self):
        for entries in ([(auth.KEY_ENTRY, KEY)], [(auth.CERT_ENTRY, CERT)],
                        [(auth.KEY_ENTRY, b""), (auth.CERT_ENTRY, CERT)],
                        [(auth.KEY_ENTRY, KEY), (auth.CERT_ENTRY, b"")]):
            with self.subTest(entries=[name for name, _ in entries]):
                with self.assertRaises(auth.PreparationError):
                    self.prepare(archive_bytes(entries))
                self.assertFalse(self.output.exists())

    def test_duplicate_required_unrelated_and_directory_alias_entries(self):
        for extra in ([(auth.KEY_ENTRY, KEY)], [(auth.CERT_ENTRY, CERT)],
                      [("other", b"a"), ("other", b"b")],
                      [("other/", b""), ("other", b"file")]):
            with self.subTest(extra=[name for name, _ in extra]):
                with self.assertRaises(auth.PreparationError):
                    self.prepare(archive_bytes([(auth.KEY_ENTRY, KEY), (auth.CERT_ENTRY, CERT)] + extra))
                self.assertFalse(self.output.exists())

    def test_traversal_and_ambiguous_paths_rejected_even_if_not_extracted(self):
        for name in ("../escape", "/absolute", "C:/drive", "a\\b", "a/../b",
                     "a/./b", "a//b", "a//", "//server/share"):
            with self.subTest(path=name):
                with self.assertRaises(auth.PreparationError):
                    self.prepare(archive_bytes([(auth.KEY_ENTRY, KEY), (auth.CERT_ENTRY, CERT), (name, b"x")]))
                self.assertFalse(self.output.exists())
        info = zipfile.ZipInfo("safe")
        info.orig_filename = "safe\0hidden"
        with self.assertRaises(auth.PreparationError):
            auth._entry_name(info)

    def test_required_directory_and_symlink_entries_rejected(self):
        with self.assertRaises(auth.PreparationError):
            self.prepare(archive_bytes([(auth.KEY_ENTRY + "/", b""), (auth.CERT_ENTRY, CERT)]))
        info = zipfile.ZipInfo(auth.KEY_ENTRY)
        info.create_system = 3
        info.external_attr = (stat.S_IFLNK | 0o777) << 16
        with self.assertRaises(auth.PreparationError):
            self.prepare(archive_bytes([(info, b"elsewhere"), (auth.CERT_ENTRY, CERT)]))

    def test_compressed_identity_size_limits_and_entry_count(self):
        data = archive_bytes([(auth.KEY_ENTRY, b"x" * 4096), (auth.CERT_ENTRY, CERT)])
        with self.assertRaises(auth.PreparationError):
            self.prepare(data, entry_limits={auth.KEY_ENTRY: 1024, auth.CERT_ENTRY: 1024})
        with self.assertRaises(auth.PreparationError):
            self.prepare(entry_limits={auth.KEY_ENTRY: 1024, auth.CERT_ENTRY: 8})
        with patch.object(auth, "MAX_ZIP_ENTRIES", 1):
            with self.assertRaises(auth.PreparationError):
                self.prepare()
        self.assertFalse(self.output.exists())

    def test_invalid_zip_and_bad_crc_are_rejected(self):
        with self.assertRaises(auth.PreparationError):
            self.prepare(b"not a ZIP")
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_STORED) as archive:
            archive.writestr(auth.KEY_ENTRY, KEY)
            archive.writestr(auth.CERT_ENTRY, CERT)
        damaged = stream.getvalue().replace(KEY, b"x" * len(KEY), 1)
        with self.assertRaises(auth.PreparationError):
            self.prepare(damaged)
        self.assertFalse(self.output.exists())

    def test_changed_partial_extra_and_empty_outputs_are_preserved(self):
        self.prepare()
        key_file = self.output / "identity.pk8"
        key_file.write_bytes(b"different")
        with self.assertRaises(auth.PreparationError):
            self.prepare()
        self.assertEqual(key_file.read_bytes(), b"different")
        key_file.unlink()
        with self.assertRaises(auth.PreparationError):
            self.prepare()
        self.assertFalse(key_file.exists())
        for item in list(self.output.iterdir()):
            item.unlink()
        with self.assertRaises(auth.PreparationError):
            self.prepare()
        self.assertEqual(list(self.output.iterdir()), [])
        self.output.rmdir()
        self.prepare()
        (self.output / "unrelated").write_bytes(b"preserve")
        with self.assertRaises(auth.PreparationError):
            self.prepare()
        self.assertEqual((self.output / "unrelated").read_bytes(), b"preserve")
        self.assert_no_staging()

    def test_failed_write_and_publication_leave_no_partial_output(self):
        writer = auth._write_file
        calls = 0

        def fail_second(path, data):
            nonlocal calls
            calls += 1
            if calls == 2:
                raise OSError("simulated write failure")
            writer(path, data)

        with patch.object(auth, "_write_file", side_effect=fail_second):
            with self.assertRaises(OSError):
                self.prepare()
        self.assertFalse(self.output.exists())
        self.assert_no_staging()
        with patch.object(auth, "_rename_exclusive", side_effect=PermissionError):
            with self.assertRaises(PermissionError):
                self.prepare()
        self.assertFalse(self.output.exists())
        self.assert_no_staging()

    def test_exclusive_publish_cannot_replace_an_existing_empty_directory(self):
        source = self.root / "stage"
        source.mkdir()
        (source / "data").write_bytes(b"fixture")
        self.output.mkdir()
        with self.assertRaises(FileExistsError):
            auth._rename_exclusive(source, self.output)
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertTrue((source / "data").exists())

    def test_output_created_during_staging_is_preserved(self):
        rename = auth._rename_exclusive

        def race(source, destination):
            destination.mkdir()
            (destination / "keep").write_bytes(b"other invocation")
            rename(source, destination)

        with patch.object(auth, "_rename_exclusive", side_effect=race):
            with self.assertRaises(auth.PreparationError):
                self.prepare()
        self.assertEqual((self.output / "keep").read_bytes(), b"other invocation")
        self.assert_no_staging()

    def test_output_symlinks_are_refused(self):
        actual = self.root / "actual"
        actual.mkdir()
        try:
            self.output.symlink_to(actual, target_is_directory=True)
        except OSError:
            self.skipTest("This host cannot create unprivileged symlinks")
        with self.assertRaises(auth.PreparationError):
            self.prepare()
        self.assertEqual(list(actual.iterdir()), [])

    @unittest.skipIf(os.name == "nt", "POSIX file-mode assertion")
    def test_private_output_permissions(self):
        self.prepare()
        self.assertEqual(stat.S_IMODE(self.output.stat().st_mode), 0o700)
        for path in self.output.iterdir():
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_download_verifies_and_atomically_caches_the_fixed_https_source(self):
        data = archive_bytes()
        expected = hashlib.sha256(data).hexdigest()
        cache = self.root / "cached.apk"
        opener = Mock(side_effect=lambda *args, **kwargs: Response(data, len(data)))
        self.assertEqual(auth.download_apk(cache, expected_sha256=expected, opener=opener), cache)
        request = opener.call_args.args[0]
        self.assertEqual(request.full_url, auth.APK_URL)
        self.assertEqual(opener.call_args.kwargs["timeout"], auth.DOWNLOAD_TIMEOUT)
        self.assertEqual(cache.read_bytes(), data)
        self.assert_no_staging()
        opener.reset_mock()
        auth.download_apk(cache, expected_sha256=expected, opener=opener)
        opener.assert_not_called()

    def test_bad_existing_cache_is_not_overwritten_or_redownloaded(self):
        cache = self.root / "cached.apk"
        cache.write_bytes(b"keep invalid cache")
        opener = Mock()
        with self.assertRaises(auth.PreparationError):
            auth.download_apk(cache, opener=opener)
        opener.assert_not_called()
        self.assertEqual(cache.read_bytes(), b"keep invalid cache")

    def test_declared_and_streaming_download_size_limits(self):
        data = b"x" * 100
        for declared in (100, None, -1, "invalid"):
            with self.subTest(content_length=declared):
                with self.assertRaises(auth.PreparationError):
                    auth.download_apk(self.root / "cached.apk", max_bytes=64,
                                      opener=lambda *args, **kwargs: Response(data, declared))
                self.assertFalse((self.root / "cached.apk").exists())
                self.assert_no_staging()

    def test_download_digest_mismatch_redirect_and_socket_timeout_cleanup(self):
        for opener in (
            lambda *args, **kwargs: Response(b"not the official APK"),
            lambda *args, **kwargs: Response(b"x", url="http://example.invalid/apk"),
            Mock(side_effect=TimeoutError("simulated timeout")),
        ):
            with self.subTest(opener=type(opener).__name__):
                with self.assertRaises((auth.PreparationError, TimeoutError)):
                    auth.download_apk(self.root / "cached.apk", opener=opener)
                self.assertFalse((self.root / "cached.apk").exists())
                self.assert_no_staging()

    def test_download_has_total_deadline(self):
        with patch.object(auth.time, "monotonic", side_effect=[0, auth.DOWNLOAD_DEADLINE + 1]):
            with self.assertRaises(auth.PreparationError):
                auth.download_apk(self.root / "cached.apk",
                                  opener=lambda *args, **kwargs: Response(b"x"))
        self.assert_no_staging()

    def test_cli_rejects_synthetic_apk_without_printing_its_contents(self):
        self.apk.write_bytes(archive_bytes())
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            result = auth.main(["--apk", str(self.apk), "--output", str(self.output)])
        self.assertEqual(result, 1)
        self.assertNotIn(KEY.decode(), output.getvalue())
        self.assertNotIn(CERT.decode(), output.getvalue())
        self.assertNotIn("Traceback", output.getvalue())
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
