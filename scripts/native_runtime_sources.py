"""Corresponding-source and license collection for native release bundles.

Never executes an upstream recipe or extracts its source tree into the checkout.
The original archives, their hashes, and their exact URLs travel with the release.
"""
from __future__ import annotations

import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import zipfile

GST_VERSION = "1.28.7"
CERBERO_NAME = f"cerbero-{GST_VERSION}.tar.xz"
CERBERO_URL = f"https://gstreamer.freedesktop.org/data/pkg/src/{GST_VERSION}/{CERBERO_NAME}"
CERBERO_SHA256 = "6c502458f3e0cc1dea824879875b8939c890242091449405513ab7b8904494e5"
VC_LICENSE_URL = "https://visualstudio.microsoft.com/wp-content/uploads/2021/09/Visual-C-Runtime-2015-2022-License-1.docx"


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def download(url: str, destination: Path, expected: str | None = None) -> Path:
    if not url.startswith("https://"):
        raise ValueError("Native source downloads require HTTPS")
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.is_file() and (expected is None or sha256(destination) == expected):
        return destination
    partial = destination.with_name(destination.name + ".partial")
    print(f"Downloading {url}", flush=True)
    for attempt in range(3):
        try:
            with urllib.request.urlopen(url, timeout=120) as response, partial.open("wb") as stream:
                shutil.copyfileobj(response, stream, length=1024 * 1024)
            break
        except (OSError, urllib.error.URLError):
            if attempt == 2:
                raise
            time.sleep(2 ** attempt)
    if expected is not None and sha256(partial) != expected:
        raise RuntimeError(f"Source digest mismatch: {destination.name}")
    partial.replace(destination)
    return destination


def source_record(path: Path, url: str) -> dict:
    return {"file": path.name, "url": url, "sha256": sha256(path), "bytes": path.stat().st_size}


def license_name(name: str) -> bool:
    leaf = PurePosixPath(name).name.lower()
    return (leaf.startswith(("license", "copying", "copyright", "notice"))
            or leaf == "unlicense" or "/licenses/" in name.lower())


def collect_cerbero_licenses(archive: Path, destination: Path) -> dict:
    """Read licenses both from Cerbero and its nested original source archives."""
    destination.mkdir(parents=True, exist_ok=True)
    cached = destination / "INDEX.json"
    if cached.is_file():
        result = json.loads(cached.read_text(encoding="utf-8"))
        if result.get("archive_sha256") == CERBERO_SHA256 and all(
            (destination / item["file"]).is_file() and sha256(destination / item["file"]) == item["sha256"]
            for item in result.get("licenses", [])
        ):
            return result
    licenses = []
    components = set()

    def save(name: str, data: bytes) -> None:
        digest = hashlib.sha256(data).hexdigest()
        filename = digest + ".txt"
        path = destination / filename
        if not path.exists() or sha256(path) != digest:
            path.write_bytes(data)
        licenses.append({"source_path": name, "file": filename, "sha256": digest})

    def scan(stream: tarfile.TarFile, prefix: str = "", nested: bool = False) -> None:
        for member in stream:
            if not member.isfile():
                continue
            name = prefix + member.name
            if not nested and "/sources/" in name:
                components.add(name.split("/sources/", 1)[1].split("/", 1)[0])
            if license_name(member.name) and member.size <= 2 * 1024 * 1024:
                save(name, stream.extractfile(member).read())
            elif not nested and "/sources/" in name and member.name.endswith(
                (".tar.gz", ".tar.xz", ".tar.bz2", ".tgz", ".tar")
            ):
                with tempfile.TemporaryFile() as temporary:
                    shutil.copyfileobj(stream.extractfile(member), temporary)
                    temporary.seek(0)
                    with tarfile.open(fileobj=temporary, mode="r|*") as inner:
                        scan(inner, name + "!", True)
            elif not nested and "/sources/" in name and member.name.endswith(".zip"):
                with tempfile.TemporaryFile() as temporary:
                    shutil.copyfileobj(stream.extractfile(member), temporary)
                    temporary.seek(0)
                    with zipfile.ZipFile(temporary) as inner:
                        for entry in inner.infolist():
                            if not entry.is_dir() and license_name(entry.filename) and entry.file_size <= 2 * 1024 * 1024:
                                save(name + "!" + entry.filename, inner.read(entry))

    with tarfile.open(archive, "r|xz") as stream:
        scan(stream)
    for required in ("ffmpeg-", "x264-", "x265-"):
        if not any(component.startswith(required) for component in components):
            raise RuntimeError(f"Official source bundle is missing {required} corresponding source")
    if not licenses:
        raise RuntimeError("No upstream license files found in corresponding source")
    result = {"archive_sha256": CERBERO_SHA256, "components": sorted(components), "licenses": licenses}
    write_json(cached, result)
    return result


def cerbero_sources(cache: Path, output: Path, license_destination: Path) -> dict:
    archive = download(CERBERO_URL, cache / CERBERO_NAME, CERBERO_SHA256)
    license_cache = cache / "cerbero-licenses"
    index = collect_cerbero_licenses(archive, license_cache)
    shutil.copytree(license_cache, license_destination, dirs_exist_ok=True)
    output.mkdir(parents=True, exist_ok=True)
    target = output / CERBERO_NAME
    if not target.is_file() or sha256(target) != CERBERO_SHA256:
        shutil.copy2(archive, target)
    return {**source_record(target, CERBERO_URL), "components": index["components"],
            "build_recipes": "cerbero-1.28.7/recipes (included, with patches)",
            "upstream_checksum_url": CERBERO_URL + ".sha256sum"}


def microsoft_license(cache: Path, destination: Path) -> dict:
    original = download(VC_LICENSE_URL, cache / "Microsoft-VC-Runtime-License.docx")
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copy2(original, destination / original.name)
    import xml.etree.ElementTree as ET
    with zipfile.ZipFile(original) as archive:
        document = ET.fromstring(archive.read("word/document.xml"))
    ns = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
    paragraphs = ["".join(p.itertext()) for p in document.iter(ns + "p")]
    (destination / "LICENSE.txt").write_text("\n".join(paragraphs) + "\n", encoding="utf-8")
    return {**source_record(original, VC_LICENSE_URL), "license": "Microsoft Visual C++ Runtime 2015-2022",
            "source_availability": "Microsoft redistributable; not an open-source component",
            "redistribution_terms": "https://learn.microsoft.com/cpp/windows/redistributing-visual-cpp-files"}


def usb_sources(manifest: dict, cache: Path, output: Path, version: str) -> dict:
    files = {}
    for package in manifest["packages"]:
        url = package["source"]
        name = url.rsplit("/", 1)[1]
        if name not in files:
            path = download(url, cache / "msys2" / name)
            # MSYS2 allsource packages must include source payloads, not just PKGBUILD.
            listing = subprocess.check_output(["tar", "-tf", str(path)], text=True).splitlines()
            if not any(n.endswith((".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst", ".zip")) for n in listing):
                raise RuntimeError(f"MSYS2 package contains recipes without complete original source: {name}")
            files[name] = source_record(path, url)
    output.mkdir(parents=True, exist_ok=True)
    target = output / f"RustCarPlay-{version}-native-source-windows-x86_64.tar.gz"
    index = {"schema": 1, "packages": manifest["packages"], "source_archives": list(files.values()),
             "build_instructions": "Original MSYS2 allsource archives retain PKGBUILD, upstream sources, and distribution patches."}
    with tarfile.open(target, "w:gz") as archive:
        body = (json.dumps(index, indent=2) + "\n").encode()
        info = tarfile.TarInfo("SOURCE-MANIFEST.json")
        info.size = len(body)
        archive.addfile(info, io.BytesIO(body))
        for name in sorted(files):
            archive.add(cache / "msys2" / name, arcname="sources/" + name)
    return {"file": target.name, "sha256": sha256(target), "bytes": target.stat().st_size,
            "source_archives": list(files.values())}


def dsc_artifacts(directory: Path) -> list[Path]:
    descriptors = list(directory.glob("*.dsc"))
    if len(descriptors) != 1:
        raise RuntimeError(f"Expected exactly one Debian source descriptor in {directory.name}")
    descriptor = descriptors[0]
    text = descriptor.read_text(encoding="utf-8")
    match = re.search(r"^Checksums-Sha256:\n((?: [^\n]+\n)+)", text, re.MULTILINE)
    if match is None:
        raise RuntimeError("Debian .dsc has no SHA-256 source file list")
    result = [descriptor]
    for line in match.group(1).splitlines():
        digest, size, name = line.split()
        if Path(name).name != name or "/" in name or "\\" in name:
            raise RuntimeError("Invalid Debian source filename")
        path = directory / name
        if not path.is_file() or path.stat().st_size != int(size) or sha256(path) != digest:
            raise RuntimeError(f"Missing or corrupt corresponding source: {name}")
        result.append(path)
    return result


def debian_sources(packages: list[dict], cache: Path, output: Path, version: str, label: str) -> dict:
    records = []
    payloads = []
    for name, source_version in sorted({(p["source_package"], p["source_version"]) for p in packages}):
        directory = cache / "ubuntu" / re.sub(r"[^A-Za-z0-9.+_-]", "_", name + "-" + source_version)
        directory.mkdir(parents=True, exist_ok=True)
        command = ["apt-get", "source", "--download-only", "--only-source", name + "=" + source_version]
        # Record the authenticated apt index's exact download locations too.
        # An empty directory prevents apt from eliding URLs for cached files.
        with tempfile.TemporaryDirectory(prefix="rustcarplay-apt-uris-") as temporary:
            locations = subprocess.check_output(command + ["--print-uris"], cwd=temporary, text=True)
        urls = re.findall(r"^'(https?://[^']+)'", locations, re.MULTILINE)
        if not urls:
            raise RuntimeError(f"apt returned no source locations for {name}={source_version}; enable deb-src")
        if not list(directory.glob("*.dsc")):
            subprocess.run(command, cwd=directory, check=True)
        artifacts = dsc_artifacts(directory)
        records.append({"name": name, "version": source_version, "urls": urls,
                        "files": [{"file": p.name, "sha256": sha256(p), "bytes": p.stat().st_size} for p in artifacts]})
        payloads.extend((path, "sources/" + directory.name + "/" + path.name) for path in artifacts)
    target = output / f"RustCarPlay-{version}-native-source-{label}.tar.gz"
    output.mkdir(parents=True, exist_ok=True)
    with tarfile.open(target, "w:gz") as archive:
        content = (json.dumps({"schema": 1, "source_packages": records, "binary_packages": packages}, indent=2) + "\n").encode()
        info = tarfile.TarInfo("SOURCE-MANIFEST.json")
        info.size = len(content)
        archive.addfile(info, io.BytesIO(content))
        for path, name in payloads:
            archive.add(path, arcname=name)
    return {"file": target.name, "sha256": sha256(target), "bytes": target.stat().st_size,
            "source_packages": records}
