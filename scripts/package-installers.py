"""Build native installers from an existing offline portable archive (Python 3.12+).

Building never installs a driver or changes system libraries. --verify installs
and removes only a temporary Windows smoke instance; macOS/Linux verification
mounts or extracts the package without installing the application system-wide.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("portable_verification", ROOT / "scripts/verify-portable-release.py")
portable = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(portable)
TARGETS = portable.TARGETS
ICON = ROOT / "apps/carplay-desktop/assets/carplay.png"
LINUX_DEPENDS = (
    "libc6 (>= 2.39), libgl1, libegl1, libvulkan1, libx11-6, libxcb1, "
    "libxkbcommon0, libwayland-client0, bluez, network-manager, usbmuxd, "
    "udev, dbus-user-session, hicolor-icon-theme"
)


def run(command: list[str | Path], **kwargs) -> str:
    result = subprocess.run([str(item) for item in command], text=True, capture_output=True, **kwargs)
    if result.returncode:
        raise RuntimeError(f"Installer command failed: {command[0]} ({result.returncode})\n{result.stderr[-4000:]}")
    return result.stdout


def write_text(path: Path, body: str, mode: int | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8", newline="\n")
    if mode is not None:
        path.chmod(mode)


def require_native(target: str) -> str:
    _label, system, architecture = TARGETS[target]
    actual = {"amd64": "x86_64", "arm64": "aarch64"}.get(platform.machine().lower(), platform.machine().lower())
    if platform.system() != system or architecture != actual:
        raise RuntimeError("Build and verify installers on their native operating system and architecture")
    return system


def installed_marker(package: Path, version: str, target: str) -> None:
    write_text(package / "INSTALLATION.json", json.dumps({
        "schema": 1, "mode": "installed", "product": "RustCarPlay",
        "version": version, "target": target,
    }, indent=2) + "\n")
    notice = """RustCarPlay installed application / 安装版

Start RustCarPlay from your application menu. Bundled media libraries and the
experimental DiPlay identity are selected automatically. Personal settings,
pairings, logs and caches stay in your user data directory; uninstalling the
application preserves them. See docs/ and THIRD_PARTY_NOTICES.md for platform
capabilities, source availability and the experimental identity's provenance.

从系统应用菜单启动 RustCarPlay。媒体运行库与实验认证资源已随程序提供。
设置、配对记录、日志与缓存保存在用户数据目录，卸载程序时保留。
系统蓝牙配对、iPhone 信任提示和 USB 驱动准备仍由系统管理。
Linux 真机兼容性尚未验证；macOS 当前为界面预览，连接适配器尚未完成。
"""
    write_text(package / "INSTALLATION.txt", notice)
    write_text(package / "RUNTIME.txt", notice)


def png_icon_container(source: Path, destination: Path) -> None:
    """Wrap the existing PNG in ICO without resampling or changing its pixels."""
    image = source.read_bytes()
    if image[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("Installer icon must be a PNG")
    width, height = struct.unpack_from(">II", image, 16)
    if width != height or not 1 <= width <= 256:
        raise ValueError("The existing Windows icon must be square and at most 256 pixels")
    destination.parent.mkdir(parents=True, exist_ok=True)
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack("<BBBBHHII", width % 256, height % 256, 0, 0, 1, 32, len(image), 22)
    destination.write_bytes(header + entry + image)


def find_iscc() -> Path:
    configured = os.environ.get("ISCC_PATH")
    if configured:
        candidates = [Path(configured)]
    else:
        # Chocolatey's PATH entry can be a forwarding executable, without the
        # compiler's adjacent license and resources. Prefer the actual install.
        candidates = [Path(os.environ.get(name, fallback)) / "Inno Setup 6/ISCC.exe"
                      for name, fallback in (("ProgramFiles(x86)", "C:/Program Files (x86)"),
                                             ("ProgramFiles", "C:/Program Files"))]
        if on_path := shutil.which("ISCC.exe"):
            candidates.append(Path(on_path))
    compiler = next((p for p in candidates if p.is_file() and (p.parent / "License.txt").is_file()), None)
    if compiler is None:
        raise RuntimeError("Inno Setup 6 ISCC.exe and its adjacent License.txt are required; "
                           "set ISCC_PATH to the installed compiler, not a PATH shim. Checked: "
                           + ", ".join(str(path) for path in candidates))
    return compiler


def normalize_public_payload(root: Path) -> None:
    """Installed release resources must be readable by ordinary desktop users.

    This applies to the explicitly public experimental release identity too;
    private pairings and settings are created separately at runtime.
    """
    for path in root.rglob("*"):
        if path.is_symlink():
            continue
        if path.is_dir():
            path.chmod(0o755)
        else:
            path.chmod(0o755 if path.stat().st_mode & 0o111 else 0o644)


def windows_setup(package: Path, temporary: Path, version: str, output: Path) -> Path:
    compiler = find_iscc()
    license_file = compiler.parent / "License.txt"
    if not license_file.is_file():
        raise RuntimeError("The Inno Setup compiler's redistribution license is missing")
    licenses = package / "resources/installer"
    licenses.mkdir(parents=True, exist_ok=True)
    shutil.copy2(license_file, licenses / "INNO-SETUP-LICENSE.txt")
    write_text(licenses / "NOTICE.txt", "Installer built using Inno Setup by Jordan Russell and Martijn Laan.\n"
               "https://jrsoftware.org/\nhttps://github.com/jrsoftware/issrc\n"
               "The installer retains Inno Setup's original copyright and About information.\n")
    png_icon_container(ICON, package / "resources/RustCarPlay.ico")
    language = temporary / "ChineseSimplified.isl"
    # Inno's Unicode parser recognizes this generated resource unambiguously.
    language.write_text((ROOT / "packaging/windows/ChineseSimplified.isl").read_text(encoding="utf-8"), encoding="utf-8-sig")
    name = f"RustCarPlay-{version}-windows-x86_64-setup"
    run([compiler, "/Q", "/DProductVersion=" + version,
         "/DPackageRoot=" + str(package), "/DOutputDirectory=" + str(output),
         "/DOutputName=" + name, "/DChineseMessages=" + str(language),
         ROOT / "packaging/windows/setup.iss"], timeout=600)
    installer = output / (name + ".exe")
    with installer.open("rb") as stream:
        if stream.read(2) != b"MZ":
            raise RuntimeError("Inno Setup did not produce a Windows executable")
    return installer


def macos_app(package: Path, destination: Path, version: str) -> Path:
    app = destination / "RustCarPlay.app"
    contents = app / "Contents"
    resources = contents / "Resources"
    resources.mkdir(parents=True)
    payload = resources / "payload"
    shutil.copytree(package, payload)
    # Contents/MacOS is a code-only location. The offline bundle also contains
    # licenses/settings/scripts, so keep that tree sealed under Resources and
    # preserve the already-relocated app/runtime paths inside it.
    executables = contents / "MacOS"
    executables.mkdir()
    launcher = executables / "RustCarPlay"
    (payload / "RustCarPlay").replace(launcher)
    launcher.chmod(0o755)
    normalize_public_payload(payload)
    iconset = destination / "RustCarPlay.iconset"
    iconset.mkdir()
    for logical in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            name = f"icon_{logical}x{logical}" + ("@2x" if scale == 2 else "") + ".png"
            run(["/usr/bin/sips", "-z", str(logical * scale), str(logical * scale), ICON,
                 "--out", iconset / name])
    run(["/usr/bin/iconutil", "-c", "icns", iconset, "-o", resources / "RustCarPlay.icns"])
    shutil.rmtree(iconset)  # Newly created, fixed child of our temporary build directory.
    info = {"CFBundleName": "RustCarPlay", "CFBundleDisplayName": "RustCarPlay",
            "CFBundleIdentifier": "io.github.ezreal-byte.rustcarplay", "CFBundlePackageType": "APPL",
            "CFBundleExecutable": "RustCarPlay", "CFBundleIconFile": "RustCarPlay.icns",
            "CFBundleShortVersionString": version, "CFBundleVersion": version.split("-")[0].split("+")[0],
            "LSMinimumSystemVersion": "15.0", "NSHighResolutionCapable": True,
            "NSMicrophoneUsageDescription": "Use your microphone for CarPlay voice input when a supported connection is active."}
    (contents / "Info.plist").write_bytes(plistlib.dumps(info))
    # Private dylibs were already relocated and signed by the native bundler.
    # Signing the outer app seals resources/main executable without rewriting
    # their bytes, preserving runtime/NATIVE-MANIFEST.json hashes.
    run(["/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none", app])
    run(["/usr/bin/codesign", "--verify", "--deep", "--strict", app])
    portable.validate_runtime_manifest(payload, json.loads((payload / "INSTALLATION.json").read_text())["target"])
    return app


def macos_dmg(package: Path, temporary: Path, version: str, target: str, output: Path) -> Path:
    image_root = temporary / "dmg-root"
    image_root.mkdir()
    macos_app(package, image_root, version)
    (image_root / "Applications").symlink_to("/Applications", target_is_directory=True)
    destination = output / f"RustCarPlay-{version}-{TARGETS[target][0]}.dmg"
    run(["/usr/bin/hdiutil", "create", "-ov", "-format", "UDZO", "-fs", "HFS+", "-volname", "RustCarPlay",
         "-srcfolder", image_root, destination], timeout=600)
    run(["/usr/bin/hdiutil", "verify", destination], timeout=120)
    return destination


def linux_tree(package: Path, destination: Path, version: str, target: str) -> str:
    architecture = "amd64" if target.startswith("x86_64") else "arm64"
    payload = destination / "opt/rustcarplay"
    shutil.copytree(package, payload)
    wrapper = destination / "usr/bin/rustcarplay"
    write_text(wrapper, (ROOT / "packaging/linux/rustcarplay").read_text(encoding="utf-8"), 0o755)
    desktop = destination / "usr/share/applications/rustcarplay.desktop"
    write_text(desktop, (ROOT / "packaging/linux/rustcarplay.desktop").read_text(encoding="utf-8"), 0o644)
    icon = destination / "usr/share/icons/hicolor/192x192/apps/rustcarplay.png"
    icon.parent.mkdir(parents=True)
    shutil.copy2(ICON, icon)
    # All payload directories must be traversable by desktop users after dpkg
    # assigns root ownership. Preserve executable bits, removing writable bits.
    normalize_public_payload(destination)
    installed_size = (sum(p.stat().st_size for p in destination.rglob("*") if p.is_file()) + 1023) // 1024
    control = f"""Package: rustcarplay
Version: {version}
Section: sound
Priority: optional
Architecture: {architecture}
Maintainer: RustCarPlay contributors <noreply@github.com>
Installed-Size: {installed_size}
Depends: {LINUX_DEPENDS}
Recommends: pipewire-pulse | pulseaudio
Homepage: https://github.com/Ezreal-byte/RustCarPlay
Description: Cross-platform CarPlay receiver
 Offline application with bundled media libraries and an experimental
 accessory identity. Requires Ubuntu 24.04-compatible system services.
 Linux iPhone interoperability has not yet been validated on real hardware.
"""
    write_text(destination / "DEBIAN/control", control, 0o644)
    hashes = []
    for path in sorted(destination.rglob("*")):
        if path.is_file() and "DEBIAN" not in path.relative_to(destination).parts:
            with path.open("rb") as stream:
                digest = hashlib.file_digest(stream, "md5").hexdigest()
            hashes.append(digest + "  " + path.relative_to(destination).as_posix())
    write_text(destination / "DEBIAN/md5sums", "\n".join(hashes) + "\n", 0o644)
    return architecture


def linux_deb(package: Path, temporary: Path, version: str, target: str, output: Path) -> Path:
    tree = temporary / "deb-root"
    architecture = linux_tree(package, tree, version, target)
    destination = output / f"rustcarplay_{version}_{architecture}.deb"
    run(["dpkg-deb", "--root-owner-group", "--build", tree, destination], timeout=600)
    if inspect_deb(destination, target) != version:
        raise RuntimeError("DEB control version does not match the packaged release")
    return destination


def build(archive: Path, target: str, output: Path) -> Path:
    system = require_native(target)
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="RustCarPlay installer build ") as temporary:
        temporary = Path(temporary)
        extracted = temporary / "portable"
        extracted.mkdir()
        portable.safe_extract(archive, extracted)
        package, version = portable.validate_layout(extracted, target)
        portable.validate_runtime_manifest(package, target)
        installed_marker(package, version, target)
        if system == "Windows":
            if (package / "RustCarPlay.exe").read_bytes() == (package / "app/rustcarplay.exe").read_bytes():
                raise RuntimeError("Launcher and CLI collide in the portable input")
            result = windows_setup(package, temporary, version, output)
        elif system == "Darwin":
            result = macos_dmg(package, temporary, version, target, output)
        else:
            result = linux_deb(package, temporary, version, target, output)
    print(f"Built installer: {result.name} ({result.stat().st_size} bytes)")
    return result


def validate_installed(root: Path, target: str, temporary: Path, launcher: Path | None = None) -> Path:
    marker = portable.read_json(root / "INSTALLATION.json")
    if (marker.get("schema"), marker.get("mode"), marker.get("product"), marker.get("target")) != (1, "installed", "RustCarPlay", target):
        raise RuntimeError("Installer marker does not match installed application")
    version = marker.get("version", "")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", version):
        raise RuntimeError("Installer has an invalid application version")
    portable.validate_runtime_manifest(root, target)
    system = TARGETS[target][1]
    environment = portable.clean_environment(temporary, system)
    if launcher is None:
        launcher = root / ("RustCarPlay.exe" if system == "Windows" else "RustCarPlay")
    working = temporary / "unrelated working directory"
    working.mkdir(exist_ok=True)
    output = portable.run_captured([str(launcher), "--cli", "--version"], working, environment, "Installed CLI version")
    if not portable.matches_cli_version(output, version):
        raise RuntimeError("Installed application version check failed")
    output = portable.run_captured([str(launcher), "--cli", "auth-check"], working, environment, "Installed local identity check")
    if b"key_matches_certificate: true" not in output or b"iphone_trust_verified: false" not in output:
        raise RuntimeError("Installed local identity check failed")
    del output
    if (root / ".local").exists() or (launcher.parent / ".local").exists():
        raise RuntimeError("Installed application wrote state into its installation directory")
    if system == "Windows":
        data = Path(environment["LOCALAPPDATA"]) / "RustCarPlay"
        portable.run_captured([str(launcher), "--smoke-test"], working, environment,
                              "Installed Windows GUI startup")
    elif system == "Darwin":
        data = Path(environment["HOME"]) / "Library/Application Support/RustCarPlay"
        portable.verify_macos_dependencies(root, environment)
        if launcher.parent != root:
            portable.verify_macos_dependencies(launcher.parent, environment, minimum_binaries=1)
    else:
        data = Path(environment["XDG_DATA_HOME"]) / "rustcarplay"
    if not (data / ".local/logs/launcher.log").is_file():
        raise RuntimeError("Installed application did not use the isolated user data directory")
    return data


def inspect_deb(archive: Path, target: str) -> str:
    fields = dict(line.split(": ", 1) for line in run(["dpkg-deb", "--field", archive]).splitlines() if ": " in line and not line.startswith(" "))
    architecture = "amd64" if target.startswith("x86_64") else "arm64"
    if fields.get("Package") != "rustcarplay" or fields.get("Architecture") != architecture:
        raise RuntimeError("DEB control metadata has the wrong package or architecture")
    version = fields.get("Version", "")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", version):
        raise RuntimeError("DEB has an invalid release version")
    for required in ("bluez", "network-manager", "usbmuxd", "libc6 (>= 2.39)"):
        if required not in fields.get("Depends", "").split(", "):
            raise RuntimeError("DEB is missing a required system dependency")
    with tempfile.TemporaryFile() as stream:
        subprocess.run(["dpkg-deb", "--fsys-tarfile", str(archive)], stdout=stream, check=True)
        stream.seek(0)
        with tarfile.open(fileobj=stream) as payload:
            public_identity = set()
            for member in payload:
                if member.uid != 0 or member.gid != 0:
                    raise RuntimeError("DEB payload ownership is not root:root")
                if member.isdir() and member.mode != 0o755:
                    raise RuntimeError("DEB directories must be traversable by ordinary users")
                if member.isfile() and member.mode not in (0o644, 0o755):
                    raise RuntimeError("DEB files must be readable by ordinary users and not writable by other users")
                name = member.name.removeprefix("./")
                if name.startswith("opt/rustcarplay/resources/auth/") and member.isfile():
                    if member.mode != 0o644:
                        raise RuntimeError("Bundled public identity files must have mode 0644")
                    public_identity.add(Path(name).name)
            if public_identity != {"identity.pk8", "certificate.p7b", "provenance.json"}:
                raise RuntimeError("DEB is missing the public release identity resources")
    return version


def verify(archive: Path, target: str) -> None:
    system = require_native(target)
    with tempfile.TemporaryDirectory(prefix="RustCarPlay installer verification ") as temporary:
        temporary = Path(temporary)
        if system == "Windows":
            root = temporary / "installed application"
            smoke_id = uuid.uuid4().hex
            uninstaller = root / "unins000.exe"
            data = None
            sentinel = None
            driver_receipt = root / ".local/windows-usb/test-restore-record.json"
            shared_driver_receipt = None
            try:
                run([archive, "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", "/NOICONS", "/TASKS=",
                     "/DIR=" + str(root), "/SmokeTestId=" + smoke_id, "/LOG=" + str(temporary / "setup.log")], timeout=600)
                data = validate_installed(root, target, temporary)
                sentinel = data / "keep-personal-data.txt"
                sentinel.write_text("installer verification fixture", encoding="ascii")
                # Preserve both old installation-local and current shared-user
                # rollback receipts when removing tracked application files.
                write_text(driver_receipt, '{"fixture": "no device or driver operation"}\n')
                shared_driver_receipt = data / ".local/windows-usb/test-restore-record.json"
                write_text(shared_driver_receipt, '{"fixture": "shared rollback record"}\n')
            finally:
                if uninstaller.is_file():
                    run([uninstaller, "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"], timeout=180)
            if sentinel is None or not sentinel.is_file():
                raise RuntimeError("Uninstallation removed personal data")
            if not driver_receipt.is_file():
                raise RuntimeError("Uninstallation removed USB driver rollback records")
            if shared_driver_receipt is None or not shared_driver_receipt.is_file():
                raise RuntimeError("Uninstallation removed shared USB driver rollback records")
            if (root / "RustCarPlay.exe").exists():
                raise RuntimeError("Temporary Windows application was not uninstalled")
        elif system == "Darwin":
            mount = temporary / "mounted image"
            mount.mkdir()
            run(["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", mount, archive], timeout=120)
            try:
                app = mount / "RustCarPlay.app"
                info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
                if info.get("CFBundleExecutable") != "RustCarPlay" or info.get("CFBundlePackageType") != "APPL":
                    raise RuntimeError("DMG application metadata is invalid")
                run(["/usr/bin/codesign", "--verify", "--deep", "--strict", app])
                executables = app / "Contents/MacOS"
                if {path.name for path in executables.iterdir()} != {"RustCarPlay"}:
                    raise RuntimeError("DMG executable directory contains unexpected resources")
                validate_installed(app / "Contents/Resources/payload", target, temporary,
                                   executables / "RustCarPlay")
            finally:
                run(["/usr/bin/hdiutil", "detach", mount], timeout=120)
        else:
            version = inspect_deb(archive, target)
            extracted = temporary / "deb payload"
            run(["dpkg-deb", "--extract", archive, extracted], timeout=120)
            if (extracted / "usr/bin/rustcarplay").read_text(encoding="utf-8") != (ROOT / "packaging/linux/rustcarplay").read_text(encoding="utf-8"):
                raise RuntimeError("DEB command wrapper differs from the reviewed launcher")
            for name in ("usr/share/applications/rustcarplay.desktop", "usr/share/icons/hicolor/192x192/apps/rustcarplay.png"):
                portable.require_file(extracted, name)
            if portable.read_json(extracted / "opt/rustcarplay/INSTALLATION.json").get("version") != version:
                raise RuntimeError("DEB control and installed application versions differ")
            validate_installed(extracted / "opt/rustcarplay", target, temporary)
    print("Installer layout, isolated application launch and personal-data preservation checks passed.")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--archive", type=Path, help="Existing portable binary archive to wrap")
    action.add_argument("--verify", type=Path, help="Verify a previously built native installer")
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    if args.verify:
        verify(args.verify.resolve(strict=True), args.target)
    else:
        build(args.archive.resolve(strict=True), args.target, args.output.resolve())


if __name__ == "__main__":
    main()
