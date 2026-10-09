"""Bundle private native runtimes and matching source assets (Python 3.12+).

Run on the target OS after building app/carplay-desktop and app/rustcarplay.
Nothing is installed globally. Missing libraries, licenses, or source archives
are errors; a manifest is written only after private codec probes succeed.
"""
from __future__ import annotations

import argparse
import email
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import urllib.request
import zipfile

from native_runtime_sources import (
    GST_VERSION, cerbero_sources, debian_sources, download, microsoft_license,
    sha256, source_record, usb_sources, write_json,
)

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "x86_64-pc-windows-msvc": "windows-x86_64",
    "x86_64-unknown-linux-gnu": "linux-x86_64",
    "aarch64-unknown-linux-gnu": "linux-aarch64",
    "x86_64-apple-darwin": "macos-x86_64",
    "aarch64-apple-darwin": "macos-aarch64",
}
PLUGINS = (
    "coreelements", "typefindfunctions", "app", "audioconvert", "audioresample",
    "audioparsers", "autodetect", "opus", "playback", "videoparsersbad",
    "videoconvertscale", "libav", "audiomixer", "audiotestsrc", "videotestsrc", "x264", "x265",
)
ELEMENTS = (
    "appsrc", "appsink", "h264parse", "h265parse", "avdec_h264", "avdec_h265",
    "videoconvert", "aacparse", "avdec_aac", "opusdec", "opusenc", "decodebin",
    "audioconvert", "audioresample", "audiomixer", "autoaudiosink", "autoaudiosrc",
    "audiotestsrc", "videotestsrc", "x264enc", "x265enc", "avenc_aac",
)
WINDOWS_SYSTEM_DLLS = set("""
advapi32 avrt bcrypt bcryptprimitives bluetoothapis cabinet cfgmgr32 comctl32
combase comdlg32 crypt32 cryptbase cryptsp d3d9 d3d11 d3d12 d3dcompiler_47 dcomp
devobj dnsapi dsound dwmapi dxgi dxguid dwrite gdi32 glu32 hid imm32 iphlpapi
kernel32 ksuser mf mfplat mfreadwrite mfuuid mmdevapi mpr msacm32 msasn1 msvcrt
ncrypt netapi32 normaliz ntdll ole32 oleaut32 opengl32 powrprof propsys psapi
rpcrt4 secur32 setupapi shell32 shlwapi user32 userenv usp10 uxtheme version
winhttp wininet winmm winusb winspool wintrust wlanapi ws2_32 wtsapi32 wsock32
""".split())
LINUX_SYSTEM = re.compile(
    r"^(?:ld-linux[^/]*|lib(?:c|m|pthread|dl|rt|resolv|util|anl|nss_[^/]+)\.so(?:\..*)?"
    r"|lib(?:GL|EGL|GLX|OpenGL|GLESv[12]|GLdispatch|vulkan)\.so(?:\..*)?)$"
)


def run(args: list[str | Path], **kwargs) -> str:
    result = subprocess.run([str(a) for a in args], check=False, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)
    if result.returncode:
        raise RuntimeError(f"Native command failed ({result.returncode}): {' '.join(str(a) for a in args)}\n"
                           f"{result.stdout[-4000:]}\n{result.stderr[-4000:]}")
    return result.stdout


def copy_file(source: Path, destination: Path) -> None:
    if not source.is_file():
        raise FileNotFoundError(source)
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() and sha256(destination) != sha256(source):
        raise RuntimeError(f"Native basename collision: {destination.name}")
    shutil.copy2(source.resolve(), destination)


def applications(package: Path, windows: bool) -> list[Path]:
    folder = package / "app" if (package / "app").is_dir() else package
    paths = [folder / (name + (".exe" if windows else "")) for name in ("carplay-desktop", "rustcarplay")]
    for path in paths:
        if not path.is_file():
            raise FileNotFoundError(path)
    return paths


def pe_imports(path: Path) -> set[str]:
    """Read normal and delay PE imports without loading or executing the file."""
    data = path.read_bytes()
    if data[:2] != b"MZ":
        raise ValueError(f"Not a PE executable: {path.name}")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("Invalid PE signature")
    machine, count = struct.unpack_from("<HH", data, pe + 4)
    if machine != 0x8664:
        raise ValueError(f"Expected Windows x64 PE: {path.name}")
    optional = pe + 24
    if struct.unpack_from("<H", data, optional)[0] != 0x20B:
        raise ValueError("Expected PE32+")
    section_start = optional + struct.unpack_from("<H", data, pe + 20)[0]
    sections = [struct.unpack_from("<IIII", data, section_start + i * 40 + 8) for i in range(count)]

    def offset(rva: int) -> int:
        for virtual_size, base, raw_size, raw in sections:
            if base <= rva < base + max(virtual_size, raw_size):
                return raw + rva - base
        raise ValueError("PE RVA is outside sections")

    imports = set()
    for directory, entry_size, name_offset in ((1, 20, 12), (13, 32, 4)):
        rva, size = struct.unpack_from("<II", data, optional + 112 + directory * 8)
        if not rva or not size:
            continue
        start = offset(rva)
        for entry in range(start, start + size, entry_size):
            if not any(data[entry:entry + entry_size]):
                break
            if directory == 13 and struct.unpack_from("<I", data, entry)[0] != 1:
                raise ValueError("Unsupported VA-based PE delay import")
            name_rva = struct.unpack_from("<I", data, entry + name_offset)[0]
            name_pos = offset(name_rva)
            imports.add(data[name_pos:data.index(b"\0", name_pos)].decode("ascii").lower())
    return imports


def windows_system_dll(name: str) -> bool:
    return name.startswith(("api-ms-win-", "ext-ms-win-")) or name.removesuffix(".dll") in WINDOWS_SYSTEM_DLLS


def windows_runtime(prefix: Path, runtime: Path, apps: list[Path]) -> list[dict]:
    if os.name != "nt":
        raise RuntimeError("Windows bundling and codec preflight require a Windows host")
    destination = runtime / "gstreamer"
    dlls = {p.name.lower(): p for p in (prefix / "bin").glob("*.dll")}
    queue = list(apps)
    for name in PLUGINS + ("wasapi", "wasapi2"):
        path = prefix / "lib/gstreamer-1.0" / ("gst" + name + ".dll")
        copy_file(path, destination / path.relative_to(prefix))
        queue.append(path)
    for name in ("gst-inspect-1.0.exe", "gst-launch-1.0.exe"):
        path = prefix / "bin" / name
        copy_file(path, destination / "bin" / name)
        queue.append(path)
    scanner = prefix / "libexec/gstreamer-1.0/gst-plugin-scanner.exe"
    if not scanner.is_file():
        scanner = prefix / "bin/gst-plugin-scanner.exe"
    copy_file(scanner, destination / "libexec/gstreamer-1.0/gst-plugin-scanner.exe")
    queue.append(scanner)
    visited = set()
    system = set()
    while queue:
        path = queue.pop()
        if path in visited:
            continue
        visited.add(path)
        for name in pe_imports(path):
            if name in dlls:
                dependency = dlls[name]
                copy_file(dependency, destination / "bin" / dependency.name)
                queue.append(dependency)
            elif windows_system_dll(name):
                system.add(name)
            else:
                raise RuntimeError(f"Unbundled Windows import {name} required by {path.name}")
    return [{"name": name, "provided_by": "Windows 11 x64"} for name in sorted(system)]


def windows_provenance(prefix: Path, runtime: Path, wheel_directory: Path) -> list[dict]:
    """Tie copied bytes back to the verified official wheels and retain metadata."""
    wanted = {p.relative_to(runtime / "gstreamer").as_posix(): p for p in (runtime / "gstreamer").rglob("*") if p.is_file()}
    matched = set()
    result = []
    for wheel in sorted(wheel_directory.glob(f"gstreamer*-{GST_VERSION}-*-win_amd64.whl")):
        with zipfile.ZipFile(wheel) as archive:
            metadata_name = next(n for n in archive.namelist() if n.endswith(".dist-info/METADATA"))
            metadata = archive.read(metadata_name)
            message = email.message_from_bytes(metadata)
            name = message["Name"]
            with urllib.request.urlopen(f"https://pypi.org/pypi/{name}/{GST_VERSION}/json", timeout=60) as response:
                release = json.load(response)
            asset = next((a for a in release["urls"] if a["filename"] == wheel.name), None)
            if asset is None or sha256(wheel) != asset["digests"]["sha256"]:
                raise RuntimeError(f"Unverified official runtime wheel: {wheel.name}")
            used = []
            for entry in archive.infolist():
                match = re.search(r"/(?:purelib|platlib)/[^/]+/((?:bin|lib|libexec|share)/.+)$", entry.filename)
                if match is None:
                    continue
                relative = match.group(1)
                # Some upstream releases put the scanner in bin instead.
                candidate = relative
                if relative == "bin/gst-plugin-scanner.exe":
                    candidate = "libexec/gstreamer-1.0/gst-plugin-scanner.exe"
                if candidate in wanted:
                    import hashlib
                    if hashlib.sha256(archive.read(entry)).hexdigest() != sha256(wanted[candidate]):
                        raise RuntimeError(f"Runtime bytes differ from official wheel: {candidate}")
                    matched.add(candidate)
                    used.append(candidate)
            if used:
                notice = runtime / "licenses/wheels" / (name + "-METADATA.txt")
                notice.parent.mkdir(parents=True, exist_ok=True)
                notice.write_bytes(metadata)
                result.append({"name": name, "version": GST_VERSION, "wheel": wheel.name,
                               "url": asset["url"], "sha256": asset["digests"]["sha256"],
                               "license": message.get("License-Expression") or message.get("License"), "files": used})
    if set(wanted) != matched:
        raise RuntimeError(f"Files without verified upstream wheel provenance: {sorted(set(wanted) - matched)}")
    return result


def bundle_usb_driver(cache: Path, runtime: Path) -> tuple[Path, dict]:
    """Package reviewed installer resources without loading or installing a driver."""
    binary_name = "libusb-win32-bin-1.4.0.2.zip"
    binary_url = "https://github.com/mcuee/libusb-win32/releases/download/release_1.4.0.2/" + binary_name
    binary_sha = "00004c92cdb99be36e17fb2377165eb97e63b48ba895bfc04a642ea9c3e26d94"
    binary = download(binary_url, cache / binary_name, binary_sha)
    copy_file(binary, runtime / "usb-driver" / binary_name)
    with zipfile.ZipFile(binary) as archive:
        # The running receiver needs the matching user-mode control library
        # after the separately authorized per-device driver preparation.
        library = runtime / "usb-filter/libusb0.dll"
        library.parent.mkdir(parents=True, exist_ok=True)
        library.write_bytes(archive.read("libusb-win32-bin-1.4.0.2/bin/amd64/libusb0.dll"))
        pending = [library]
        copied = {library.name.lower()}
        while pending:
            for name in pe_imports(pending.pop()):
                if windows_system_dll(name) or name in copied:
                    continue
                # LoadLibraryEx restricts this library to its own directory and
                # System32, so its verified VC runtime must also live beside it.
                dependency = runtime / "gstreamer/bin" / name
                copy_file(dependency, library.parent / name)
                copied.add(name)
                pending.append(library.parent / name)
        for name in ("COPYING_GPL.txt", "COPYING_LGPL.txt", "installer_license.txt", "README.txt"):
            path = runtime / "licenses/libusb-win32" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(archive.read("libusb-win32-bin-1.4.0.2/" + name))
    commit = "0987f983a89e5e60d3c2db2af46564239c1b0d37"
    source_url = "https://codeload.github.com/mcuee/libusb-win32/tar.gz/" + commit
    source_sha = "b710538a40446ed919f761dc4e91cd91c389c34bd5b35fd929df476425acf110"
    source = download(source_url, cache / ("libusb-win32-" + commit + ".tar.gz"), source_sha)
    with tarfile.open(source) as archive:
        names = set(archive.getnames())
        root = "libusb-win32-" + commit + "/libusb/"
        for name in ("src/driver/libusb_driver.c", "projects/vs2019/libusb-win32.sln", "COPYING_GPL.txt", "COPYING_LGPL.txt"):
            if root + name not in names:
                raise RuntimeError("Incomplete libusb-win32 release source archive")
    record = {**source_record(source, source_url), "release_tag": "release_1.4.0.2", "commit": commit,
              "binary": source_record(binary, binary_url),
              "build_instructions": "Included libusb/README.in, Makefile, ddk_make and projects/vs2019 retain driver and user-mode build definitions; Microsoft WDK/SDK are external system toolchains."}
    write_json(runtime / "usb-driver/PROVENANCE.json", record)
    return source, record


def bundle_usb(prefix: Path, runtime: Path, cache: Path, output: Path, version: str) -> dict:
    manifest = json.loads((ROOT / "scripts/usb-runtime-packages.json").read_text(encoding="utf-8"))
    installed = json.loads((prefix / "packages.json").read_text(encoding="utf-8"))
    if installed != manifest:
        raise RuntimeError("Prepared USB runtime does not match the pinned package manifest")
    expected = {}
    import hashlib
    for package in manifest["packages"]:
        archive = prefix / "downloads" / f"{package['name']}-{package['version']}-any.pkg.tar.zst"
        if not archive.is_file() or sha256(archive) != package["sha256"]:
            raise RuntimeError(f"Unverified USB binary package: {archive.name}")
        for name in run(["tar", "-tf", archive]).splitlines():
            if re.fullmatch(r"ucrt64/bin/[^/]+\.dll", name) or re.fullmatch(r"ucrt64/bin/(?:idevice_id|ideviceinfo|idevicepair)\.exe", name):
                body = subprocess.check_output(["tar", "-xOf", str(archive), name])
                expected[Path(name).name] = hashlib.sha256(body).hexdigest()
    for name, digest in expected.items():
        path = prefix / "bin" / name
        if not path.is_file() or sha256(path) != digest:
            raise RuntimeError(f"USB runtime bytes differ from pinned package: {name}")
        copy_file(path, runtime / "usb/bin" / name)
    shutil.copytree(prefix / "licenses", runtime / "licenses/usb", dirs_exist_ok=True)
    write_json(runtime / "usb/packages.json", manifest)
    driver_source = bundle_usb_driver(cache, runtime)
    return usb_sources(manifest, cache, output, version, [driver_source])


def parse_ldd(text: str) -> dict[str, Path]:
    dependencies = {}
    for line in text.splitlines():
        if "=> not found" in line:
            raise RuntimeError("Unresolved ELF dependency: " + line.strip())
        match = re.match(r"\s*(\S+)\s+=>\s+(/.+?)\s+\(0x[0-9a-f]+\)", line)
        if match:
            dependencies[match.group(1)] = Path(match.group(2))
        else:
            match = re.match(r"\s*(/\S+)\s+\(0x[0-9a-f]+\)", line)
            if match:
                path = Path(match.group(1))
                dependencies[path.name] = path
    return dependencies


def dpkg_owner(path: Path) -> str:
    choices = [str(path), str(path.resolve())]
    choices += [p[4:] for p in choices if p.startswith("/usr/lib/")]
    for choice in dict.fromkeys(choices):
        result = subprocess.run(["dpkg-query", "-S", choice], text=True, capture_output=True)
        if result.returncode == 0:
            for line in result.stdout.splitlines():
                if ": " in line and not line.startswith("diversion"):
                    return line.split(": ", 1)[0]
    raise RuntimeError(f"No distribution source ownership for {path}")


def linux_runtime(runtime: Path, apps: list[Path], target: str) -> tuple[list[dict], list[dict]]:
    if platform.system() != "Linux":
        raise RuntimeError("Linux bundling requires a native Ubuntu host")
    os_release = Path("/etc/os-release").read_text()
    if 'ID=ubuntu' not in os_release or 'VERSION_ID="24.04"' not in os_release:
        raise RuntimeError("Linux release baseline must be Ubuntu 24.04")
    architecture = "x86_64-linux-gnu" if target.startswith("x86_64") else "aarch64-linux-gnu"
    lib = Path("/usr/lib") / architecture
    destination = runtime / "gstreamer"
    queue = list(apps)
    owners = set()
    baseline = set()
    for name in PLUGINS + ("alsa", "pulseaudio"):
        source = lib / "gstreamer-1.0" / ("libgst" + name + ".so")
        copy_file(source, destination / "lib/gstreamer-1.0" / source.name)
        owners.add(dpkg_owner(source))
        queue.append(source)
    for name in ("gst-inspect-1.0", "gst-launch-1.0"):
        source = Path(shutil.which(name) or "/missing/" + name)
        copy_file(source, destination / "bin" / name)
        owners.add(dpkg_owner(source))
        queue.append(source)
    scanners = [lib / "gstreamer1.0/gstreamer-1.0/gst-plugin-scanner", lib / "gstreamer-1.0/gst-plugin-scanner"]
    scanner = next((p for p in scanners if p.is_file()), None)
    if scanner is None:
        raise RuntimeError("Ubuntu GStreamer plugin scanner is missing")
    copy_file(scanner, destination / "libexec/gstreamer-1.0/gst-plugin-scanner")
    owners.add(dpkg_owner(scanner))
    queue.append(scanner)
    # These are dlopen() dependencies and do not appear in the Rust ELF imports.
    for name in ("libbluetooth.so.3", "libimobiledevice-1.0.so.6"):
        source = lib / name
        copy_file(source, destination / "lib" / name)
        owners.add(dpkg_owner(source))
        queue.append(source)
    for source in sorted(Path("/usr/share/alsa").rglob("*")):
        if source.is_file():
            copy_file(source, destination / "share/alsa" / source.relative_to("/usr/share/alsa"))
            owners.add(dpkg_owner(source))
    for source in sorted((lib / "alsa-lib").glob("*.so")):
        copy_file(source, destination / "lib/alsa-lib" / source.name)
        owners.add(dpkg_owner(source))
        queue.append(source)
    visited = set()
    while queue:
        source = queue.pop()
        if source.resolve() in visited:
            continue
        visited.add(source.resolve())
        for name, dependency in parse_ldd(run(["ldd", source])).items():
            if LINUX_SYSTEM.fullmatch(name):
                baseline.add(name)
                continue
            copy_file(dependency, destination / "lib" / name)
            owners.add(dpkg_owner(dependency))
            queue.append(dependency)
    packages = []
    for owner in sorted(owners):
        fields = run(["dpkg-query", "-W", "-f=${binary:Package}\t${Version}\t${source:Package}\t${source:Version}", owner]).split("\t")
        binary, binary_version, source, source_version = fields
        source = source or binary.split(":")[0]
        source_version = source_version or binary_version
        packages.append({"binary_package": binary, "binary_version": binary_version,
                         "source_package": source, "source_version": source_version})
        copyright_file = Path("/usr/share/doc") / binary.split(":")[0] / "copyright"
        copy_file(copyright_file, runtime / "licenses/ubuntu" / (binary.replace(":", "_") + ".copyright"))
    shutil.copytree("/usr/share/common-licenses", runtime / "licenses/ubuntu/common-licenses", dirs_exist_ok=True)
    return packages, [{"name": name, "provided_by": "Ubuntu 24.04 glibc and desktop graphics stack"} for name in sorted(baseline)]


def macho(path: Path) -> bool:
    if not path.is_file():
        return False
    with path.open("rb") as stream:
        return stream.read(4) in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf")


def macho_dependencies(path: Path) -> set[str]:
    return {line.strip().split(" (compatibility version", 1)[0]
            for line in run(["otool", "-L", path]).splitlines() if line.startswith("\t")}


def macho_rpaths(path: Path) -> list[str]:
    # Universal files report the same path once for each architecture.
    return list(dict.fromkeys(re.findall(r"cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (.+?) \(offset \d+\)",
                                        run(["otool", "-l", path]))))


def macho_install_ids(path: Path) -> set[str]:
    # otool -L includes LC_ID_DYLIB before actual dependencies. It must not be
    # resolved as a dependency, particularly for a nested @rpath backend.
    # Universal files may report the same ID once per architecture.
    return {line.strip() for line in run(["otool", "-D", path]).splitlines()
            if line.strip() and not line.rstrip().endswith(":")}


def macho_sdk_roots(prefix: Path) -> tuple[Path, ...]:
    return (prefix, Path("/Library/Frameworks/GStreamer.framework/Versions/1.0"),
            Path("/Library/Frameworks/GStreamer.framework/Versions/Current"))


def private_macho_path(candidate: Path, prefix: Path, destination: Path) -> Path | None:
    """Map an SDK location to its copied counterpart, never to the installed SDK."""
    candidate = candidate.resolve()
    private_root = destination.resolve()
    if candidate.is_relative_to(private_root):
        return candidate
    for original_root in macho_sdk_roots(prefix):
        original_root = original_root.resolve()
        if candidate.is_relative_to(original_root):
            local = (private_root / candidate.relative_to(original_root)).resolve()
            if local.is_relative_to(private_root):
                return local
    return None


def resolve_macho_dependency(dependency: str, image: Path, rpaths: list[str],
                             prefix: Path, destination: Path) -> Path:
    candidates = []
    if dependency.startswith("@rpath/"):
        suffix = dependency.removeprefix("@rpath/")
        # Keep the original LC_RPATH order and subdirectories. libproxy's
        # backend lives in lib/libproxy/, not alongside the top-level dylibs.
        for rpath in rpaths:
            if rpath == "@loader_path" or rpath.startswith("@loader_path/"):
                base = image.parent / rpath.removeprefix("@loader_path").lstrip("/")
            elif rpath.startswith("/") or Path(rpath).is_absolute():
                base = Path(rpath)
            else:
                continue
            candidates.append(base / suffix)
        # Common SDK libraries may inherit the executable's lib/ runpath.
        # This fallback is also private and never scans the host framework.
        candidates.append(destination / "lib" / suffix)
    elif dependency.startswith("@loader_path/"):
        candidates.append(image.parent / dependency.removeprefix("@loader_path/"))
    elif not dependency.startswith("@") and (dependency.startswith("/") or Path(dependency).is_absolute()):
        candidates.append(Path(dependency))
    else:
        raise RuntimeError(f"Unsupported private Mach-O dependency: {dependency}")
    for candidate in candidates:
        local = private_macho_path(candidate, prefix, destination)
        if local is not None and local.is_file():
            return local
    raise RuntimeError(f"Unresolved private Mach-O dependency: {dependency} in {image.name}")


def macos_runtime(prefix: Path, runtime: Path, apps: list[Path]) -> list[dict]:
    if platform.system() != "Darwin":
        raise RuntimeError("macOS relocation and code signing require a macOS host")
    destination = runtime / "gstreamer"
    # Copy dylibs and resources, not headers, pkg-config files, static archives,
    # introspection databases, development tools, or Python modules.
    for source in sorted((prefix / "lib").rglob("*.dylib")):
        if "gstreamer-1.0" in source.parts:
            continue
        copy_file(source, destination / source.relative_to(prefix))
    for name in PLUGINS + ("osxaudio",):
        source = prefix / "lib/gstreamer-1.0" / ("libgst" + name + ".dylib")
        copy_file(source, destination / source.relative_to(prefix))
    for name in ("gst-inspect-1.0", "gst-launch-1.0"):
        copy_file(prefix / "bin" / name, destination / "bin" / name)
    scanner = prefix / "libexec/gstreamer-1.0/gst-plugin-scanner"
    copy_file(scanner, destination / "libexec/gstreamer-1.0/gst-plugin-scanner")
    for source in sorted((prefix / "share").rglob("*")):
        if source.is_file() and not any(p in ("doc", "gtk-doc", "man", "gir-1.0", "aclocal") for p in source.parts):
            copy_file(source, destination / source.relative_to(prefix))
    private = [p for p in destination.rglob("*") if macho(p)]
    system = set()
    for path in private + apps:
        changes = []
        original_rpaths = macho_rpaths(path)
        dependencies = macho_dependencies(path)
        if path.suffix == ".dylib":
            dependencies -= macho_install_ids(path)
        for dependency in dependencies:
            if dependency.startswith(("/System/Library/", "/usr/lib/")):
                system.add(dependency)
            else:
                local = resolve_macho_dependency(dependency, path, original_rpaths, prefix, destination)
                relocated = "@loader_path/" + os.path.relpath(local, path.parent).replace(os.sep, "/")
                changes.extend(["-change", dependency, relocated])
        if path.suffix == ".dylib":
            changes.extend(["-id", "@loader_path/" + path.name])
        # Every private dependency now uses @loader_path. Remove SDK search paths
        # too, so a developer's framework installation cannot mask missing files.
        for rpath in original_rpaths:
            if rpath.startswith("/Library/Frameworks/GStreamer.framework") or rpath.startswith(str(prefix)):
                changes.extend(["-delete_rpath", rpath])
        if changes:
            run(["install_name_tool", *changes, path])
        if any("/Library/Frameworks/GStreamer.framework" in dependency for dependency in macho_dependencies(path)):
            raise RuntimeError(f"Framework relocation failed: {path.name}")
        run(["codesign", "--force", "--sign", "-", "--timestamp=none", path])
        run(["codesign", "--verify", "--strict", path])
    return [{"name": name, "provided_by": "macOS 15 system framework"} for name in sorted(system)]


def codec_preflight(runtime: Path, windows: bool, target: str) -> list[str]:
    gst = runtime / "gstreamer"
    env = dict(os.environ)
    env["PATH"] = str(gst / "bin") + os.pathsep + (os.environ.get("SystemRoot", "C:/Windows") + "/System32" if windows else "/usr/bin:/bin")
    env["GST_PLUGIN_PATH_1_0"] = str(gst / "lib/gstreamer-1.0")
    env["GST_PLUGIN_SYSTEM_PATH_1_0"] = env["GST_PLUGIN_PATH_1_0"]
    env["GST_PLUGIN_SCANNER_1_0"] = str(gst / "libexec/gstreamer-1.0" / ("gst-plugin-scanner.exe" if windows else "gst-plugin-scanner"))
    env["LD_LIBRARY_PATH"] = str(gst / "lib")
    env["DYLD_LIBRARY_PATH"] = str(gst / "lib")
    env["DYLD_FALLBACK_LIBRARY_PATH"] = str(gst / "lib")
    env["ALSA_CONFIG_DIR"] = str(gst / "share/alsa")
    env["ALSA_PLUGIN_DIR"] = str(gst / "lib/alsa-lib")
    elements = list(ELEMENTS)
    elements += ["wasapi2src", "wasapi2sink"] if windows else (["pulsesrc", "pulsesink", "alsasrc", "alsasink"] if "linux" in target else ["osxaudiosrc", "osxaudiosink"])
    with tempfile.TemporaryDirectory(prefix="rustcarplay-native-probe-") as temporary:
        env["GST_REGISTRY_1_0"] = str(Path(temporary) / "registry.bin")
        tool = gst / "bin" / ("gst-inspect-1.0.exe" if windows else "gst-inspect-1.0")
        if "linux" not in target and f"GStreamer {GST_VERSION}\n" not in run([tool, "--version"], env=env, timeout=60):
            raise RuntimeError("Native GStreamer version does not match the complete corresponding-source archive")
        for element in elements:
            run([tool, element], env=env, timeout=60)
        # Exercise codec construction and real buffers without opening a speaker,
        # microphone, display or phone. Factory enumeration alone misses errors
        # in lazy-loaded encoder/decoder implementations.
        launcher = gst / "bin" / ("gst-launch-1.0.exe" if windows else "gst-launch-1.0")
        graphs = (
            "videotestsrc num-buffers=2 ! video/x-raw,format=I420,width=64,height=64,framerate=30/1 ! x264enc tune=zerolatency ! h264parse ! avdec_h264 ! fakesink",
            "videotestsrc num-buffers=2 ! video/x-raw,format=I420,width=64,height=64,framerate=30/1 ! x265enc tune=zerolatency ! h265parse ! avdec_h265 ! fakesink",
            "audiotestsrc num-buffers=8 ! audio/x-raw,rate=48000,channels=2 ! audioconvert ! avenc_aac ! aacparse ! avdec_aac ! fakesink",
            "audiotestsrc num-buffers=8 ! audio/x-raw,rate=48000,channels=1 ! audioconvert ! opusenc ! opusdec ! fakesink",
        )
        for graph in graphs:
            run([launcher, "-q", *graph.split()], env=env, timeout=60)
    return elements


def bundle(args: argparse.Namespace) -> dict:
    package = args.app_dir.resolve()
    runtime = package / "runtime"
    if runtime.exists() and any(runtime.iterdir()):
        raise RuntimeError("Use a fresh staging package; runtime/ already contains files")
    apps = applications(package, "windows" in args.target)
    runtime.mkdir(parents=True, exist_ok=True)
    cache = args.cache_dir.resolve()
    output = args.source_output.resolve()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    sources = []
    metadata = {}
    if "windows" in args.target:
        prefix = (args.gstreamer_prefix or ROOT / ".local/gstreamer").resolve()
        system = windows_runtime(prefix, runtime, apps)
        metadata["wheels"] = windows_provenance(prefix, runtime, args.wheel_directory.resolve())
        elements = codec_preflight(runtime, True, args.target)
        sources.append(cerbero_sources(cache, output, runtime / "licenses/cerbero"))
        metadata["microsoft_runtime"] = microsoft_license(cache, runtime / "licenses/microsoft")
        sources.append(bundle_usb((args.usb_prefix or ROOT / ".local/usb-runtime").resolve(), runtime, cache, output, version))
        baseline = "Windows 11 x64; Apple USBMUX service and selected-device driver preparation remain system requirements"
    elif "linux" in args.target:
        packages, system = linux_runtime(runtime, apps, args.target)
        elements = codec_preflight(runtime, False, args.target)
        metadata["ubuntu_packages"] = packages
        sources.append(debian_sources(packages, cache, output, version, TARGETS[args.target]))
        baseline = "Ubuntu 24.04 desktop, glibc 2.39+, host graphics drivers, audio server and USBMUX daemon; no bundled system services"
    else:
        prefix = (args.gstreamer_prefix or Path("/Library/Frameworks/GStreamer.framework/Versions/1.0")).resolve()
        system = macos_runtime(prefix, runtime, apps)
        elements = codec_preflight(runtime, False, args.target)
        sources.append(cerbero_sources(cache, output, runtime / "licenses/cerbero"))
        baseline = "macOS 15; ad-hoc signed private dylibs; GUI/core preview, native CarPlay connection adapters not implemented"
    manifest = {"schema": 1, "target": args.target, "baseline": baseline, "source_assets": sources,
                "license_directory": "runtime/licenses", "required_elements": elements,
                "codec_preflight": True, "system_dependencies": system, **metadata,
                "files": [{"path": path.relative_to(package).as_posix(), "sha256": sha256(path), "bytes": path.stat().st_size}
                          for path in sorted(runtime.rglob("*")) if path.is_file()]}
    write_json(runtime / "NATIVE-MANIFEST.json", manifest)
    print(json.dumps({"target": args.target, "runtime_files": len(manifest["files"]),
                      "runtime_bytes": sum(item["bytes"] for item in manifest["files"]),
                      "source_assets": [item["file"] for item in sources], "codec_preflight": True}, indent=2))
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app-dir", type=Path, required=True, help="Staged package root containing app/ executables")
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--source-output", type=Path, required=True, help="Release directory for complete corresponding-source attachments")
    parser.add_argument("--gstreamer-prefix", type=Path)
    parser.add_argument("--usb-prefix", type=Path)
    parser.add_argument("--wheel-directory", type=Path, default=ROOT / ".local/downloads")
    parser.add_argument("--cache-dir", type=Path, default=ROOT / ".local/native-cache")
    bundle(parser.parse_args())


if __name__ == "__main__":
    main()
