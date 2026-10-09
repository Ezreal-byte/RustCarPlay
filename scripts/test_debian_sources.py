"""Offline Debian source descriptor regressions, including legacy PGP armor."""
from pathlib import Path
import tempfile
import unittest

import native_runtime_sources as sources


def signed(control: str, version: str = "GnuPG v1.4.14 (MirBSD)") -> str:
    # Synthetic envelope: signature verification belongs to APT acquisition,
    # while these fixtures exercise extraction of the authenticated stanza.
    return ("-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA384\n\n" + control
            + "\n-----BEGIN PGP SIGNATURE-----\nVersion: " + version
            + "\n\nsynthetic-signature\n-----END PGP SIGNATURE-----\n")


class DebianSourceTests(unittest.TestCase):
    def fixture(self, directory: Path) -> tuple[Path, str]:
        payload = directory / "db5.3_5.3.28+dfsg2.orig.tar.xz"
        payload.write_bytes(b"synthetic corresponding source")
        control = ("Format: 3.0 (quilt)\nSource: db5.3\nVersion: 5.3.28+dfsg2-7\n"
                   "Checksums-Sha256:\n " + sources.sha256(payload) + " "
                   + str(payload.stat().st_size) + " " + payload.name + "\n")
        return payload, control

    def test_db53_legacy_signature_version_cannot_override_source_version(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            payload, control = self.fixture(directory)
            descriptor = directory / "db5.3_5.3.28+dfsg2-7.dsc"
            for text in (control, signed(control), signed(control).replace("\n", "\r\n")):
                with self.subTest(signed="BEGIN" in text):
                    descriptor.write_bytes(text.encode("utf-8"))
                    self.assertEqual(sources.dsc_artifacts(directory, ("db5.3", "5.3.28+dfsg2-7")), [descriptor, payload])

    def test_signature_metadata_cannot_make_a_wrong_source_version_match(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            _, control = self.fixture(directory)
            descriptor = directory / "db5.3.dsc"
            descriptor.write_text(signed(control, "5.3.28+dfsg2-8"), encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, r"expected db5.3=5\.3\.28\+dfsg2-8, got db5.3=5\.3\.28\+dfsg2-7"):
                sources.dsc_artifacts(directory, ("db5.3", "5.3.28+dfsg2-8"))
            with self.assertRaisesRegex(RuntimeError, "does not match"):
                sources.dsc_artifacts(directory, ("other-package", "5.3.28+dfsg2-7"))

    def test_signed_source_still_requires_every_file_hash_and_size(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            payload, control = self.fixture(directory)
            descriptor = directory / "db5.3.dsc"
            descriptor.write_text(signed(control), encoding="utf-8")
            # Preserve the size so this specifically exercises the hash gate.
            payload.write_bytes(b"x" * payload.stat().st_size)
            with self.assertRaisesRegex(RuntimeError, "Missing or corrupt corresponding source"):
                sources.dsc_artifacts(directory, ("db5.3", "5.3.28+dfsg2-7"))
            payload.unlink()
            with self.assertRaisesRegex(RuntimeError, "Missing or corrupt corresponding source"):
                sources.dsc_artifacts(directory, ("db5.3", "5.3.28+dfsg2-7"))

    def test_duplicate_body_fields_and_extra_stanzas_are_rejected(self):
        for control, message in (("Source: db5.3\nVersion: 1\nversion: 2\n", "Duplicate.*version"),
                                 ("Source: db5.3\nSource: other\n", "Duplicate.*source"),
                                 ("Source: db5.3\n\nVersion: 1\n", "one control stanza")):
            with self.subTest(control=control), self.assertRaisesRegex(RuntimeError, message):
                sources.dsc_control_fields(signed(control))

    def test_malformed_armor_or_signature_only_checksums_are_rejected(self):
        for text in ("-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA384\n",
                     signed("Source: db5.3\n") + "Version: injected\n"):
            with self.subTest(text=text), self.assertRaises(RuntimeError):
                sources.dsc_control_fields(text)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            _, control = self.fixture(directory)
            descriptor = directory / "db5.3.dsc"
            text = signed("Source: db5.3\nVersion: 5.3.28+dfsg2-7\n")
            text = text.replace("synthetic-signature", control[control.index("Checksums-Sha256:"):])
            descriptor.write_text(text, encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "no SHA-256 source file list"):
                sources.dsc_artifacts(directory, ("db5.3", "5.3.28+dfsg2-7"))


if __name__ == "__main__":
    unittest.main()
