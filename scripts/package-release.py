"""Build explicit runtime-free binary/source release assets (Python 3.12+)."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "x86_64-pc-windows-msvc": "windows-x86_64",
    "x86_64-unknown-linux-gnu": "linux-x86_64",
    "aarch64-unknown-linux-gnu": "linux-aarch64",
    "x86_64-apple-darwin": "macos-x86_64",
    "aarch64-apple-darwin": "macos-aarch64",
}
USB_HELPERS = ("usb_probe", "usb_mode", "usb_runtime_check")
WINDOWS_SCRIPTS = (
    "start-release-windows.ps1", "prepare-gstreamer.ps1", "with-gstreamer.ps1",
    "setup-usb-runtime.ps1", "usb-runtime-packages.json", "start-windows-usb.ps1",
    "windows-usb-config.ps1", "windows-usb-filter.ps1",
)


def version() -> str:
    with (ROOT / "Cargo.toml").open("rb") as stream:
        return tomllib.load(stream)["workspace"]["package"]["version"]


def validate_tag(tag: str) -> str:
    current = version()
    if tag != f"v{current}":
        raise ValueError(f"Tag {tag!r} does not match Cargo workspace version v{current}")
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version=1"], cwd=ROOT
    ))
    for package in metadata["packages"]:
        if package["id"] in metadata["workspace_members"] and package["version"] != current:
            raise ValueError(f"Workspace package {package['name']} has another version")
    return current


def write_text(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8", newline="\n")


def source_notice(current: str, repository: str) -> str:
    return f"""RustCarPlay {current}: Rust dependency licenses and corresponding source

Download RustCarPlay-{current}-source.tar.gz from this same release:
https://github.com/{repository}/releases/tag/v{current}

The source archive contains this exact Git revision, Cargo.lock, build scripts,
and the complete `cargo vendor --locked --versioned-dirs` Rust dependency tree.
Each vendor crate retains its source and upstream license/notice files.
DEPENDENCIES.json records the dependency names, versions and license metadata.
The included .cargo/config.toml selects that relative vendor directory, allowing
cargo --offline --locked rebuilds after installing the documented native SDKs.

GStreamer, USB native libraries, Apple software, device drivers, authentication
keys and certificates are NOT included in this binary archive. Those separately
installed components remain subject to their own licenses and requirements.
See LICENSE and THIRD_PARTY_NOTICES.md for this application's source notices.
"""


def runtime_notice(target: str) -> str:
    common = """RUNTIME REQUIREMENTS / 运行依赖

This package contains the GStreamer-enabled Rust desktop and CLI executables.
It is not a self-contained installer. Native codecs and USB libraries are not
bundled. Start from this directory so relative settings/auth paths are stable.
No accessory identity/private key/certificate is included.

Windows x64 has limited real iPhone USB/wireless testing. Linux and macOS builds
are compile/test artifacts, not a claim of full device interoperability. Check
README.md and the release notes for the current platform feature boundaries.

GStreamer upstream installation: https://gstreamer.freedesktop.org/download/
Required decoding plugins include H.264/HEVC, AAC, Opus, appsrc/appsink and
audio/video conversion. Plugin availability depends on the installed runtime.

"""
    if "windows" in target:
        return common + """Windows 11 x64:
Recommended preparation in PowerShell 7 from this extracted directory:
  ./scripts/prepare-gstreamer.ps1
  ./scripts/start-release-windows.ps1
For the CLI: ./scripts/start-release-windows.ps1 -Cli -Arguments @('--help')
Preparation downloads verified official GStreamer 1.28.7 x64 native archives
into .local/gstreamer. No Python, Rust or compiler installation is needed.
Alternatively install the official MSVC x86_64 GStreamer runtime, including
libav codecs, put its bin on PATH, and launch the application executable.
The Visual C++ 2015-2022 x64 runtime may also be required.
USB additionally requires Apple Devices/iTunes and the separately prepared USB
runtime/selected-device configuration described in docs/WINDOWS_USB_DRIVER.md.
USB preparation helpers are already built in tools/; skip cargo build commands
in source-development instructions. Run scripts/setup-usb-runtime.ps1 before
scripts/start-windows-usb.ps1 (the latter requires administrator PowerShell 7).
This ZIP does not install, replace or configure any driver automatically.
"""
    if "linux" in target:
        return common + """Linux (built on Ubuntu 24.04, glibc 2.39 or newer):
Install the matching-architecture distro GStreamer runtime and desktop libraries.
Ubuntu example:
  sudo apt install libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav \
    libxkbcommon0 libwayland-client0 libx11-6 libvulkan1
Bluetooth requires libbluetooth3. USB also requires libimobiledevice6, usbmuxd,
appropriate device permissions,
and the correct cdc_ncm interface; see docs/USB.md.
Run ./carplay-desktop in your desktop session, or ./rustcarplay --help.
"""
    return common + """macOS (experimental platform build, built on macOS 15):
Install the official GStreamer 1.28.7 universal runtime framework from upstream.
The binary links to /Library/Frameworks/GStreamer.framework/Versions/1.0.
Do not substitute a Homebrew build with different library install paths.
This release is not notarized; no signed .app or DMG is provided.
Launch ./carplay-desktop from Terminal, or ./rustcarplay --help.
macOS Bluetooth/USB/hotspot native connection adapters remain limited; a built
executable does not mean complete CarPlay receiver support on macOS.
"""


def binary(current: str, target: str, output: Path, repository: str) -> None:
    label = TARGETS[target]
    name = f"RustCarPlay-{current}-{label}"
    suffix = ".exe" if "windows" in target else ""
    with tempfile.TemporaryDirectory(prefix="rustcarplay-package-") as temp:
        package = Path(temp) / name
        package.mkdir()
        for executable in ("carplay-desktop", "rustcarplay"):
            source = ROOT / "target" / target / "release" / (executable + suffix)
            if not source.is_file():
                raise FileNotFoundError(source)
            shutil.copy2(source, package / source.name)
        if suffix:
            (package / "tools").mkdir()
            for helper in USB_HELPERS:
                source = ROOT / "target" / target / "release/examples" / (helper + suffix)
                shutil.copy2(source, package / "tools" / source.name)
            (package / "scripts").mkdir()
            for script in WINDOWS_SCRIPTS:
                shutil.copy2(ROOT / "scripts" / script, package / "scripts" / script)
        for document in ("LICENSE", "README.md", "README.en.md"):
            shutil.copy2(ROOT / document, package / document)
        shutil.copy2(ROOT / "docs/THIRD_PARTY_NOTICES.md", package / "THIRD_PARTY_NOTICES.md")
        shutil.copytree(ROOT / "docs", package / "docs", ignore=shutil.ignore_patterns("*.png", "*.jpg", "*.gif"))
        write_text(package / "DEPENDENCIES-SOURCE.txt", source_notice(current, repository))
        write_text(package / "RUNTIME.txt", runtime_notice(target))
        if suffix:
            destination = output / (name + ".zip")
            with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
                for path in sorted(package.rglob("*")):
                    if path.is_file():
                        archive.write(path, path.relative_to(package.parent))
        else:
            destination = output / (name + ".tar.gz")
            with tarfile.open(destination, "w:gz") as archive:
                archive.add(package, arcname=name)
        print(destination)


def source(current: str, output: Path, repository: str) -> None:
    name = f"RustCarPlay-{current}-source"
    with tempfile.TemporaryDirectory(prefix="rustcarplay-source-") as temp:
        stage = Path(temp)
        archive_path = stage / "tracked-source.tar"
        with archive_path.open("wb") as stream:
            subprocess.run(["git", "archive", "--format=tar", f"--prefix={name}/", "HEAD"], cwd=ROOT, stdout=stream, check=True)
        with tarfile.open(archive_path) as archive:
            archive.extractall(stage, filter="data")
        package = stage / name
        config = subprocess.check_output(
            ["cargo", "vendor", "--locked", "--versioned-dirs", "vendor"], cwd=package, text=True
        )
        cargo_dir = package / ".cargo"
        cargo_dir.mkdir(exist_ok=True)
        if (cargo_dir / "config.toml").exists():
            raise RuntimeError("Source has .cargo/config.toml; merge vendor configuration explicitly before releasing")
        write_text(cargo_dir / "config.toml", config)
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--offline", "--locked", "--format-version=1"], cwd=package
        ))
        dependencies = [
            {key: item.get(key) for key in ("name", "version", "license", "repository", "source")}
            for item in metadata["packages"] if item["id"] not in metadata["workspace_members"]
        ]
        write_text(package / "DEPENDENCIES.json", json.dumps(dependencies, indent=2, ensure_ascii=False) + "\n")
        write_text(package / "DEPENDENCIES-SOURCE.txt", source_notice(current, repository))
        write_text(package / "SOURCE-REVISION.txt", subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True))
        destination = output / (name + ".tar.gz")
        with tarfile.open(destination, "w:gz") as archive:
            archive.add(package, arcname=name)
        print(destination)


def checksums(current: str, output: Path) -> None:
    names = [f"RustCarPlay-{current}-{label}" + (".zip" if "windows" in label else ".tar.gz") for label in TARGETS.values()]
    names.append(f"RustCarPlay-{current}-source.tar.gz")
    actual = sorted(path.name for path in output.iterdir() if path.is_file() and path.name != "SHA256SUMS")
    if actual != sorted(names):
        raise ValueError(f"Release assets must match the complete target matrix: {actual!r}")
    lines = []
    for name in sorted(names):
        with (output / name).open("rb") as stream:
            lines.append(f"{hashlib.file_digest(stream, 'sha256').hexdigest()}  {name}\n")
    write_text(output / "SHA256SUMS", "".join(lines))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate", "binary", "source", "checksums"))
    parser.add_argument("--tag", required=True)
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY", "Ezreal-byte/RustCarPlay"))
    args = parser.parse_args()
    current = validate_tag(args.tag)
    if args.command == "validate":
        print(current)
        return
    args.output.mkdir(parents=True, exist_ok=True)
    if args.command == "binary":
        if not args.target:
            parser.error("binary requires --target")
        binary(current, args.target, args.output, args.repository)
    elif args.command == "source":
        source(current, args.output, args.repository)
    else:
        checksums(current, args.output)


if __name__ == "__main__":
    main()
