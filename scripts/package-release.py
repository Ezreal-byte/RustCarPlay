"""Build explicit portable binary/source release assets (Python 3.12+)."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
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
USB_HELPERS = ("usb_probe", "usb_mode", "usb_runtime_check", "usb_driver_verify")
WINDOWS_SCRIPTS = (
    "start-release-windows.ps1", "prepare-gstreamer.ps1", "with-gstreamer.ps1",
    "setup-usb-runtime.ps1", "extract-usb-runtime.py", "native_runtime_sources.py", "usb-runtime-packages.json", "start-windows-usb.ps1",
    "windows-usb-config.ps1", "windows-usb-filter.ps1", "windows-usb-paths.ps1",
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

Portable binary archives also contain native GStreamer libraries and an
experimental accessory identity extracted from the pinned DiPlay preview APK.
Those authentication data are not relicensed under this project's GPL license
and are not included in the source archive. See resources/auth/provenance.json.
runtime/NATIVE-MANIFEST.json and runtime/licenses identify native components.
The same release supplies cerbero-1.28.7.tar.xz (the upstream complete native
source bundle) and platform-specific native-source archives for USB/Ubuntu
libraries and the pinned libusb-win32 filter-driver source. Its original signed
installation archive is included for manual USB preparation; Apple software
and Windows system drivers are not bundled.
See LICENSE and THIRD_PARTY_NOTICES.md for this application's source notices.
"""


def runtime_notice(target: str) -> str:
    common = """PORTABLE RELEASE / 离线便携包

Extract the whole archive to a writable directory. Keep its folders together.
GStreamer playback/decoding libraries and the pinned DiPlay experimental
accessory identity are included; no certificate directory or runtime download
is needed for the application to start. Do not launch app/carplay-desktop
directly: the root launcher selects the bundled libraries and identity.

解压完整压缩包到可写目录，从根目录的 RustCarPlay 启动。
无需手动配置媒体库或认证目录；个人设置和配对记录保存在 .local/。
认证身份来自 DiPlay v0.2.15 公开预览 APK 的实验资源，并非为本项目签发。
其持续有效性未经保证；详见 resources/auth/provenance.json 和第三方声明。

System Bluetooth pairing, iPhone trust prompts, USB permissions/drivers,
graphics/audio drivers and system services remain operating-system tasks.
Packaging is not an interoperability guarantee on untested hardware.

"""
    if "windows" in target:
        return common + """Windows 11 x64: double-click RustCarPlay.exe.
Command line: RustCarPlay.exe --cli --help
The launcher needs no PowerShell, Rust, Python or separately installed
GStreamer. LAN still requires the PC/iPhone on the same Wi-Fi and Bluetooth
pairing in system settings. USB user-mode libraries/helpers and the pinned
libusb-win32 filter installation archive are bundled;
Apple Mobile Device Service and a compatible USBMUX/NCM device/driver setup
are still required. See docs/WINDOWS_USB_DRIVER.md for USB preparation.
No bundled launcher installs drivers or requests administrator rights.
"""
    if "linux" in target:
        return common + """Linux x64/ARM64 (Ubuntu 24.04 or compatible, glibc >= 2.39):
Run ./RustCarPlay in your desktop session, or ./RustCarPlay --cli --help.
The desktop, glibc, graphics/audio drivers, BlueZ/NetworkManager and usbmuxd
services/device permissions are provided by the operating system. Bundled
libraries do not make this archive compatible with every Linux distribution.
Real iPhone interoperability has not yet been validated on Linux.
"""
    return common + """macOS Intel/Apple Silicon (macOS 15 build preview):
Run ./RustCarPlay, or ./RustCarPlay --cli --help.
This build is not notarized. macOS native Bluetooth/USB/network connection
adapters remain incomplete; bundled resources enable the UI/core preview,
not a functional macOS CarPlay connection.
"""


def prepare_auth(package: Path) -> None:
    prepared = ROOT / ".local/release-auth"
    subprocess.run([sys.executable, str(ROOT / "scripts/prepare-release-auth.py"),
                    "--output", str(prepared)], cwd=ROOT, check=True)
    destination = package / "resources/auth"
    destination.mkdir(parents=True)
    # The preparer has verified the complete APK and these exact entries. Never
    # copy the developer's .local/auth directory or arbitrary files beside it.
    for name in ("identity.pk8", "certificate.p7b", "provenance.json"):
        shutil.copy2(prepared / name, destination / name)


def bundle_runtime(package: Path, target: str, output: Path) -> None:
    subprocess.run([sys.executable, str(ROOT / "scripts/bundle-native-runtime.py"),
                    "--app-dir", str(package), "--target", target,
                    "--source-output", str(output),
                    "--cache-dir", str(ROOT / ".local/native-cache")], cwd=ROOT, check=True)


def binary(current: str, target: str, output: Path, repository: str, local_build: bool = False) -> None:
    label = TARGETS[target]
    name = f"RustCarPlay-{current}-{label}"
    suffix = ".exe" if "windows" in target else ""
    with tempfile.TemporaryDirectory(prefix="rustcarplay-package-") as temp:
        package = Path(temp) / name
        package.mkdir()
        (package / "app").mkdir()
        launcher = ROOT / "target" / target / "release" / ("carplay-launcher" + suffix)
        shutil.copy2(launcher, package / ("RustCarPlay" + suffix))
        for executable in ("carplay-desktop", "rustcarplay"):
            source = ROOT / "target" / target / "release" / (executable + suffix)
            if not source.is_file():
                raise FileNotFoundError(source)
            shutil.copy2(source, package / "app" / source.name)
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
        notice = source_notice(current, repository)
        if local_build:
            revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
            notice = notice.replace(
                f"Download RustCarPlay-{current}-source.tar.gz from this same release:\nhttps://github.com/{repository}/releases/tag/v{current}",
                f"LOCAL TEST BUILD: not published on GitHub.\nSource: the current RustCarPlay workspace, based on {revision} plus local changes.\nThere is no v{current} release source archive on GitHub for this test build."
            ).replace("The source archive contains this exact Git revision, Cargo.lock, build scripts,",
                      "Published release source archives (not generated by this local build) contain their Git revision, Cargo.lock, build scripts,")
            write_text(package / "LOCAL-BUILD.txt", f"RustCarPlay {current} local test build\nBase revision: {revision}\nIncludes local workspace changes. This package was not published on GitHub.\nSee the included docs/ for device checks.\n")
        write_text(package / "DEPENDENCIES-SOURCE.txt", notice)
        write_text(package / "RUNTIME.txt", runtime_notice(target))
        prepare_auth(package)
        bundle_runtime(package, target, output)
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
    names.append("cerbero-1.28.7.tar.xz")
    names.extend(f"RustCarPlay-{current}-native-source-{label}.tar.gz"
                 for label in ("windows-x86_64", "linux-x86_64", "linux-aarch64"))
    names.extend((f"RustCarPlay-{current}-windows-x86_64-setup.exe",
                  f"RustCarPlay-{current}-macos-x86_64.dmg",
                  f"RustCarPlay-{current}-macos-aarch64.dmg",
                  f"rustcarplay_{current}_amd64.deb", f"rustcarplay_{current}_arm64.deb"))
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
    parser.add_argument("--local-build", action="store_true", help="Mark a local binary test build without a published GitHub source link")
    args = parser.parse_args()
    current = validate_tag(args.tag)
    if args.command == "validate":
        print(current)
        return
    args.output.mkdir(parents=True, exist_ok=True)
    if args.command == "binary":
        if not args.target:
            parser.error("binary requires --target")
        binary(current, args.target, args.output, args.repository, args.local_build)
    elif args.command == "source":
        source(current, args.output, args.repository)
    else:
        checksums(current, args.output)


if __name__ == "__main__":
    main()
