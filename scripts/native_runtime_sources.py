"""Corresponding-source and license collection for native release bundles.

Never executes an upstream recipe or extracts its source tree into the checkout.
The original archives, their hashes, and their exact URLs travel with the release.
"""
from __future__ import annotations

import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
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


def validate_msys_allsource(path: Path) -> dict:
    """Accept complete upstream tarballs or a complete pinned bare Git source.

    winpthreads uses a Git source in makepkg. Inspect objects only, ignoring the
    downloaded repository's config and hooks, and never execute PKGBUILD.
    """
    listing = subprocess.check_output(["tar", "-tf", str(path)], text=True).splitlines()
    recipes = [name for name in listing if name.endswith("/PKGBUILD")]
    if len(recipes) != 1:
        raise RuntimeError(f"MSYS2 source has no unique build recipe: {path.name}")
    archives = [name for name in listing if name.endswith((".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst", ".zip"))]
    if archives:
        return {"upstream_archives": archives}
    srcinfo = recipes[0].rsplit("/", 1)[0] + "/.SRCINFO"
    metadata = subprocess.check_output(["tar", "-xOf", str(path), srcinfo], text=True)
    commits = set(re.findall(r"#commit=([0-9a-f]{40})", metadata))
    repositories = [name.removesuffix("/HEAD") for name in listing if name.endswith("/HEAD")
                    and name.removesuffix("HEAD") + "objects/" in listing]
    if len(commits) != 1 or len(repositories) != 1:
        raise RuntimeError(f"MSYS2 package has no complete upstream archive or pinned Git source: {path.name}")
    repository, commit = repositories[0], commits.pop()
    with tempfile.TemporaryDirectory(prefix="rustcarplay-source-git-") as temporary:
        target = Path(temporary)
        (target / "config").write_text("[core]\n bare = true\n", encoding="ascii")
        (target / "HEAD").write_text(commit + "\n", encoding="ascii")
        (target / "refs").mkdir()
        (target / "objects").mkdir()
        for name in listing:
            if not name.startswith(repository + "/objects/") or name.endswith("/"):
                continue
            relative = PurePosixPath(name.removeprefix(repository + "/"))
            if any(part in ("..", ".") for part in relative.parts) or relative.is_absolute():
                raise RuntimeError("Invalid Git object path in source archive")
            if not re.fullmatch(r"objects/(?:[0-9a-f]{2}/[0-9a-f]{38}|pack/pack-[0-9a-f]{40}\.(?:pack|idx|rev))", relative.as_posix()):
                continue  # Never consume alternates/promisor/config files.
            destination = target.joinpath(*relative.parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            with destination.open("wb") as output:
                subprocess.run(["tar", "-xOf", str(path), name], stdout=output, check=True)
        # git archive walks every referenced tree and blob; a recipes-only,
        # shallow/missing-object, or wrong-commit archive therefore fails.
        subprocess.run(["git", "--no-replace-objects", "--git-dir=" + temporary,
                        "-c", "core.hooksPath=", "archive", "--format=tar", commit],
                       stdout=subprocess.DEVNULL, check=True)
    return {"git_commit": commit, "git_source_complete": True}


def usb_sources(manifest: dict, cache: Path, output: Path, version: str,
                extra_archives: list[tuple[Path, dict]] | None = None) -> dict:
    files = {}
    payloads = {}
    for package in manifest["packages"]:
        url = package["source"]
        name = url.rsplit("/", 1)[1]
        if name not in files:
            path = download(url, cache / "msys2" / name)
            verification = validate_msys_allsource(path)
            files[name] = {**source_record(path, url), **verification}
            payloads[name] = path
    for path, record in extra_archives or []:
        if path.name in files or record["sha256"] != sha256(path):
            raise RuntimeError("Duplicate or inconsistent additional corresponding source")
        files[path.name] = record
        payloads[path.name] = path
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
            archive.add(payloads[name], arcname="sources/" + name)
    return {"file": target.name, "sha256": sha256(target), "bytes": target.stat().st_size,
            "source_archives": list(files.values())}


def dsc_artifacts(directory: Path, source: tuple[str, str] | None = None) -> list[Path]:
    descriptors = list(directory.glob("*.dsc"))
    if len(descriptors) != 1:
        raise RuntimeError(f"Expected exactly one Debian source descriptor in {directory.name}")
    descriptor = descriptors[0]
    text = descriptor.read_text(encoding="utf-8")
    if source is not None:
        fields = dict(re.findall(r"^(Source|Version): (.+)$", text, re.MULTILINE))
        if (fields.get("Source"), fields.get("Version")) != source:
            raise RuntimeError("Debian source descriptor does not match the installed binary's exact source version")
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


def parse_apt_source_uris(output: str) -> list[str]:
    """Preserve APT transport URIs; mirror+file is not an HTTP download URL."""
    uris = []
    for line in output.splitlines():
        if not line.startswith("'"):
            continue
        match = re.fullmatch(r"'([^'\s]+)'\s+(\S+)\s+\d+(?:\s+\S+)?\s*", line)
        if match is None:
            raise RuntimeError("Malformed apt source URI record")
        uri, filename = match.groups()
        if not re.match(r"(?:https?|file|copy|mirror(?:\+(?:https?|file))?):", uri):
            raise RuntimeError("Unsupported apt source transport: " + uri.split(":", 1)[0])
        if filename in (".", "..") or "/" in filename or "\\" in filename:
            raise RuntimeError("Invalid apt source filename")
        uris.append(uri)
    if not uris:
        raise RuntimeError("apt returned no source locations; enable matching deb-src repositories")
    return uris


def apt_source_locations(name: str, version: str) -> list[str]:
    """Query the authenticated index for this exact source, never the latest one."""
    if not re.fullmatch(r"[a-z0-9][a-z0-9+.-]*", name) or not re.fullmatch(r"[A-Za-z0-9.+:~\-]+", version):
        raise ValueError("Invalid Debian source package or version")
    command = ["apt-get", "source", "--download-only", "--only-source", name + "=" + version, "--print-uris"]
    # An empty directory prevents apt from eliding URLs for already cached files.
    with tempfile.TemporaryDirectory(prefix="rustcarplay-apt-uris-") as temporary:
        output = subprocess.check_output(command, cwd=temporary, text=True, stderr=subprocess.PIPE,
                                         env={**os.environ, "LC_ALL": "C"}, timeout=120)
    return parse_apt_source_uris(output)


def apt_local_mirror_lists(uris: list[str]) -> list[dict]:
    """Keep hosted-runner mirror choices alongside their machine-local URI."""
    lists = {}
    for uri in uris:
        if not uri.startswith("mirror+file:"):
            continue
        path = Path(urllib.parse.unquote(uri.removeprefix("mirror+file:")))
        mirror = next((parent for parent in path.parents if parent.is_file()), None)
        if mirror is None:
            raise RuntimeError("The APT local mirror list is missing: " + uri)
        if mirror.stat().st_size > 64 * 1024:
            raise RuntimeError("The APT local mirror list is unexpectedly large")
        prefix = "mirror+file:" + mirror.as_posix()
        if prefix not in lists:
            # These are mirror definitions, not a claim that every mirror was
            # used. APT selects and authenticates the actual download.
            entries = [line.strip() for line in mirror.read_text(encoding="utf-8").splitlines()
                       if line.strip() and not line.lstrip().startswith("#")]
            lists[prefix] = {"uri_prefix": prefix, "entries": entries}
    return list(lists.values())


def debian_sources(packages: list[dict], cache: Path, output: Path, version: str, label: str) -> dict:
    records = []
    payloads = []
    for name, source_version in sorted({(p["source_package"], p["source_version"]) for p in packages}):
        directory = cache / "ubuntu" / re.sub(r"[^A-Za-z0-9.+_-]", "_", name + "-" + source_version)
        directory.mkdir(parents=True, exist_ok=True)
        command = ["apt-get", "source", "--download-only", "--only-source", name + "=" + source_version]
        urls = apt_source_locations(name, source_version)
        if not list(directory.glob("*.dsc")):
            subprocess.run(command, cwd=directory, check=True)
        artifacts = dsc_artifacts(directory, (name, source_version))
        records.append({"name": name, "version": source_version, "urls": urls,
                        "apt_mirror_lists": apt_local_mirror_lists(urls),
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
