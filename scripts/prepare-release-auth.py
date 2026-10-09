"""Prepare the explicitly selected DiPlay experimental release identity.

The CLI accepts only the pinned upstream APK; test helpers accept synthetic APK
digests. Never print identity bytes or an identity digest. Outputs belong in an
ignored local directory and authorized binary packages, not source archives.
"""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import hmac
import io
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import time
import urllib.request
import zipfile
import zlib

ROOT = Path(__file__).resolve().parents[1]
APK_VERSION = "v0.2.15"
APK_URL = "https://github.com/shihabal3amri/DiPlay/releases/download/v0.2.15/DiPlay-0.2.15.apk"
APK_SHA256 = "4bf45f16d6b1ab0a61462b831014081f07240f5596c90ca6bf38fb43f9890511"
DEFAULT_CACHE = ROOT / ".local/downloads/DiPlay-0.2.15.apk"
DEFAULT_OUTPUT = ROOT / ".local/release-auth"
MAX_APK_BYTES = 64 * 1024 * 1024
MAX_ZIP_ENTRIES = 50_000
KEY_ENTRY = "assets/offline-mfi/identity.pk8"
CERT_ENTRY = "assets/offline-mfi/certificate.p7b"
ENTRY_LIMITS = {KEY_ENTRY: 16 * 1024, CERT_ENTRY: 1024 * 1024}
DOWNLOAD_TIMEOUT = 30
DOWNLOAD_DEADLINE = 180
CHUNK_BYTES = 64 * 1024


class PreparationError(ValueError):
    """Only fixed, non-secret diagnostic messages may be stored here."""


def _reject_link(path: Path) -> None:
    if path.is_symlink() or path.is_junction():
        raise PreparationError("Symbolic links and junctions are not permitted.")


def _destination(path: Path) -> Path:
    path = Path(os.path.abspath(path))
    for part in (*reversed(path.parents), path):
        _reject_link(part)
    return path


def _read_file(path: Path, limit: int) -> bytes:
    _reject_link(path)
    if not stat.S_ISREG(path.stat().st_mode):
        raise PreparationError("A regular file is required.")
    with path.open("rb") as source:
        data = source.read(limit + 1)
    if len(data) > limit:
        raise PreparationError("File exceeds its configured size limit.")
    return data


def verified_apk(path: Path, expected_sha256: str = APK_SHA256,
                 max_bytes: int = MAX_APK_BYTES) -> bytes:
    # Parse exactly the snapshot that was hashed, not a reopened mutable file.
    data = _read_file(path, max_bytes)
    if not hmac.compare_digest(hashlib.sha256(data).hexdigest(), expected_sha256):
        raise PreparationError("APK SHA-256 does not match the pinned release.")
    return data


def _rename_exclusive(source: Path, destination: Path) -> None:
    """Atomically publish a file/directory without replacing even an empty one."""
    if os.name == "nt":
        os.rename(source, destination)  # Windows rename refuses an existing target.
        return
    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform.startswith("linux"):
        # Linux UAPI linux/fs.h: RENAME_NOREPLACE = 1; AT_FDCWD = -100.
        rename = getattr(libc, "renameat2", None)
        if rename is None:
            raise PreparationError("Atomic no-replace rename is unavailable.")
        rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int,
                           ctypes.c_char_p, ctypes.c_uint]
        rename.restype = ctypes.c_int
        result = rename(-100, os.fsencode(source), -100, os.fsencode(destination), 1)
    elif sys.platform == "darwin":
        # Apple xnu bsd/sys/stdio.h: renamex_np(..., RENAME_EXCL = 4).
        rename = libc.renamex_np
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        rename.restype = ctypes.c_int
        result = rename(os.fsencode(source), os.fsencode(destination), 4)
    else:
        raise PreparationError("Atomic publication is unsupported on this platform.")
    if result != 0:
        number = ctypes.get_errno()
        raise OSError(number, os.strerror(number))


def download_apk(cache: Path = DEFAULT_CACHE, *, expected_sha256: str = APK_SHA256,
                 max_bytes: int = MAX_APK_BYTES, opener=None) -> Path:
    """Use the fixed URL, bounded HTTPS reads and a verified, immutable cache."""
    cache = _destination(cache)
    if cache.exists():
        verified_apk(cache, expected_sha256, max_bytes)
        return cache
    cache.parent.mkdir(parents=True, exist_ok=True)
    opener = opener or urllib.request.urlopen
    deadline = time.monotonic() + DOWNLOAD_DEADLINE
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(prefix=".diplay-apk-", suffix=".part",
                                         dir=cache.parent, delete=False) as target:
            temporary = Path(target.name)
            request = urllib.request.Request(APK_URL, headers={"User-Agent": "RustCarPlay-release"})
            with opener(request, timeout=DOWNLOAD_TIMEOUT) as response:
                if not response.geturl().lower().startswith("https://"):
                    raise PreparationError("The APK download must remain HTTPS.")
                declared = response.headers.get("Content-Length")
                if declared is not None:
                    try:
                        size = int(declared)
                    except (TypeError, ValueError):
                        raise PreparationError("Invalid APK Content-Length.") from None
                    if size < 1 or size > max_bytes:
                        raise PreparationError("APK download exceeds its size limit.")
                size = 0
                digest = hashlib.sha256()
                while True:
                    if time.monotonic() >= deadline:
                        raise PreparationError("APK download exceeded its time limit.")
                    chunk = response.read(min(CHUNK_BYTES, max_bytes - size + 1))
                    if not chunk:
                        break
                    size += len(chunk)
                    if size > max_bytes:
                        raise PreparationError("APK download exceeds its size limit.")
                    digest.update(chunk)
                    target.write(chunk)
                if not hmac.compare_digest(digest.hexdigest(), expected_sha256):
                    raise PreparationError("APK SHA-256 does not match the pinned release.")
            target.flush()
            os.fsync(target.fileno())
        try:
            _rename_exclusive(temporary, cache)
        except FileExistsError:
            # Another successful invocation may have populated the cache first.
            verified_apk(cache, expected_sha256, max_bytes)
        return cache
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _entry_name(info: zipfile.ZipInfo) -> str:
    raw = info.orig_filename
    name = raw[:-1] if raw.endswith("/") else raw
    if (not name or "\\" in raw or ":" in raw or "\0" in raw
            or raw.startswith("/") or any(p in ("", ".", "..") for p in name.split("/"))):
        raise PreparationError("APK contains an unsafe ZIP path.")
    mode = stat.S_IFMT(info.external_attr >> 16)
    if mode not in (0, stat.S_IFREG, stat.S_IFDIR):
        raise PreparationError("APK contains a non-regular ZIP entry.")
    if mode == stat.S_IFDIR and not info.is_dir():
        raise PreparationError("APK contains an inconsistent directory entry.")
    return raw


def extract_identity(apk_bytes: bytes, *, entry_limits=None) -> dict[str, bytes]:
    limits = ENTRY_LIMITS if entry_limits is None else entry_limits
    if set(limits) != {KEY_ENTRY, CERT_ENTRY} or any(n < 1 for n in limits.values()):
        raise PreparationError("Exactly the two bounded identity entries are required.")
    try:
        with zipfile.ZipFile(io.BytesIO(apk_bytes)) as apk:
            entries = apk.infolist()
            if len(entries) > MAX_ZIP_ENTRIES:
                raise PreparationError("APK contains too many ZIP entries.")
            selected = {}
            seen = set()
            for info in entries:
                name = _entry_name(info)
                # Trailing slash aliases must not make a file and directory collide.
                canonical = name.rstrip("/")
                if canonical in seen:
                    raise PreparationError("APK contains duplicate ZIP entries.")
                seen.add(canonical)
                if canonical in limits:
                    if info.is_dir() or info.flag_bits & 1:
                        raise PreparationError("Identity ZIP entries must be ordinary unencrypted files.")
                    if not 0 < info.file_size <= limits[canonical]:
                        raise PreparationError("Identity ZIP entry exceeds its size limit or is empty.")
                    selected[canonical] = info
            if set(selected) != set(limits):
                raise PreparationError("APK is missing an identity entry.")
            result = {}
            for name, info in selected.items():
                with apk.open(info) as source:
                    data = source.read(limits[name] + 1)
                if not 0 < len(data) <= limits[name] or len(data) != info.file_size:
                    raise PreparationError("Identity ZIP entry has an invalid decoded size.")
                result[Path(name).name] = data
            return result
    except (zipfile.BadZipFile, NotImplementedError, RuntimeError, EOFError, zlib.error):
        raise PreparationError("APK ZIP contents could not be validated.") from None


def provenance(apk_sha256: str = APK_SHA256) -> bytes:
    record = {
        "schema_version": 1,
        "upstream_project": "DiPlay",
        "upstream_version": APK_VERSION,
        "upstream_url": APK_URL,
        "apk_sha256": apk_sha256,
        "upstream_notice_url": (
            "https://github.com/shihabal3amri/DiPlay/blob/"
            "9e244d958afe6b8fd79ade49769ce25a944f397b/docs/THIRD_PARTY_NOTICES.md"
        ),
        "source_firmware_origin": (
            "Upstream describes these experimental offline authentication resources as "
            "recovered from public Carlinkit C2Air Allwinner V821 firmware."
        ),
        "experimental_status": (
            "These are not newly issued Apple credentials for RustCarPlay or DiPlay. "
            "Continued device acceptance and suitability for general distribution remain "
            "unresolved. The resources are not relicensed as project source code."
        ),
        "distribution_scope": "Explicitly authorized binary release packages only; exclude from Git and source archives.",
        "apk_entries": [KEY_ENTRY, CERT_ENTRY],
    }
    return (json.dumps(record, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode("utf-8")


def _same_directory(output: Path, files: dict[str, bytes]) -> bool:
    _reject_link(output)
    if not output.is_dir() or {p.name for p in output.iterdir()} != set(files):
        return False
    return all(hmac.compare_digest(_read_file(output / name, len(data)), data)
               for name, data in files.items())


def _write_file(path: Path, data: bytes) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as target:
        target.write(data)
        target.flush()
        os.fsync(target.fileno())


def prepare_auth(apk: Path, output: Path = DEFAULT_OUTPUT, *,
                 expected_sha256: str = APK_SHA256, max_apk_bytes: int = MAX_APK_BYTES,
                 entry_limits=None) -> dict:
    files = extract_identity(verified_apk(apk, expected_sha256, max_apk_bytes),
                             entry_limits=entry_limits)
    files["provenance.json"] = provenance(expected_sha256)
    output = _destination(output)
    if output.exists():
        if not _same_directory(output, files):
            raise PreparationError("Output already exists with different or unexpected contents; nothing was overwritten.")
        return {"ok": True, "unchanged": True, "output": str(output)}
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f".{output.name}.stage-", dir=output.parent) as temporary:
        stage = Path(temporary)
        # TemporaryDirectory owns only this checked sibling, never the final output.
        if stage.resolve().parent != output.parent.resolve():
            raise PreparationError("Unexpected staging directory location.")
        for name, data in files.items():
            _write_file(stage / name, data)
        try:
            _rename_exclusive(stage, output)
        except FileExistsError:
            if not _same_directory(output, files):
                raise PreparationError("Output appeared during preparation; nothing was overwritten.") from None
            return {"ok": True, "unchanged": True, "output": str(output)}
    return {"ok": True, "unchanged": False, "output": str(output)}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apk", type=Path, help="Existing official APK; the pinned SHA-256 is still mandatory.")
    parser.add_argument("--output", "--output-dir", type=Path, default=DEFAULT_OUTPUT,
                        help="New output directory, or an identical previous output (default: .local/release-auth).")
    args = parser.parse_args(argv)
    try:
        apk = args.apk if args.apk is not None else download_apk()
        result = prepare_auth(apk, args.output)
        print(json.dumps(result, ensure_ascii=True))
        return 0
    except PreparationError as error:
        print(f"Authentication resource preparation failed: {error}", file=sys.stderr)
    except (OSError, ValueError):
        # OS/network errors may contain signed redirect URLs or user-specific paths.
        print("Authentication resource preparation failed due to a file or network error.", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
