"""Independently verify one portable binary archive without connecting a device.

Requires Python 3.12+. Child output is captured and never printed: auth-check
includes a certificate digest that does not belong in CI logs.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile

TARGETS = {
    "x86_64-pc-windows-msvc": ("windows-x86_64", "Windows", "x86_64"),
    "x86_64-unknown-linux-gnu": ("linux-x86_64", "Linux", "x86_64"),
    "aarch64-unknown-linux-gnu": ("linux-aarch64", "Linux", "aarch64"),
    "x86_64-apple-darwin": ("macos-x86_64", "Darwin", "x86_64"),
    "aarch64-apple-darwin": ("macos-aarch64", "Darwin", "aarch64"),
}
MAX_FILES = 100_000
MAX_EXTRACTED_BYTES = 8 * 1024**3
TIMEOUT_SECONDS = 30
MACHO_MAGICS = {
    b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca",
}


class VerificationError(ValueError):
    """Diagnostics must not include captured authentication output."""


def archive_path(name: str) -> Path:
    if not name or "\\" in name or "\0" in name or ":" in name:
        raise VerificationError("Archive contains an unsafe path.")
    components = name.removesuffix("/").split("/")
    if any(component in ("", ".", "..") for component in components):
        raise VerificationError("Archive contains a traversing or ambiguous path.")
    if any(component.rstrip(" .") != component or re.fullmatch(r"(?:CON|PRN|AUX|NUL|COM[1-9¹²³]|LPT[1-9¹²³])(?:\..*)?", component, re.IGNORECASE) for component in components):
        raise VerificationError("Archive contains an ambiguous or reserved filename.")
    pure = PurePosixPath(name)
    if pure.is_absolute():
        raise VerificationError("Archive contains an absolute path.")
    return Path(*components)


def inside(path: Path, root: Path) -> Path:
    resolved = path.resolve()
    if not resolved.is_relative_to(root.resolve()):
        raise VerificationError("Archive link or path escapes its extraction directory.")
    return resolved


def safe_extract(archive: Path, destination: Path) -> None:
    seen = set()
    total = 0

    def check(name: str, size: int) -> Path:
        nonlocal total
        relative = archive_path(name)
        key = str(relative).casefold() if os.name == "nt" else str(relative)
        if key in seen:
            raise VerificationError("Archive contains duplicate paths.")
        seen.add(key)
        total += size
        if size < 0 or len(seen) > MAX_FILES or total > MAX_EXTRACTED_BYTES:
            raise VerificationError("Archive exceeds the verification extraction limits.")
        return relative

    if zipfile.is_zipfile(archive):
        with zipfile.ZipFile(archive) as zipped:
            for member in zipped.infolist():
                relative = check(member.filename, member.file_size)
                mode = member.external_attr >> 16
                if member.flag_bits & 1 or stat.S_ISLNK(mode):
                    raise VerificationError("Encrypted files and ZIP symlinks are not permitted.")
                if stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR):
                    raise VerificationError("Archive contains a special file.")
                output = destination / relative
                inside(output, destination)
                if member.is_dir():
                    output.mkdir(parents=True, exist_ok=True)
                    continue
                output.parent.mkdir(parents=True, exist_ok=True)
                with zipped.open(member) as source, output.open("xb") as target:
                    shutil.copyfileobj(source, target)
                if output.stat().st_size != member.file_size:
                    raise VerificationError("Archive file size changed during extraction.")
                if os.name != "nt" and mode:
                    output.chmod(mode & 0o777 & ~0o022)
    else:
        with tarfile.open(archive, "r:*") as tar:
            members = tar.getmembers()
            for member in members:
                relative = check(member.name, member.size)
                if not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
                    raise VerificationError("Archive contains a special file.")
                if member.issym() or member.islnk():
                    link = member.linkname
                    if not link or "\\" in link or ":" in link or PurePosixPath(link).is_absolute():
                        raise VerificationError("Archive contains an unsafe link.")
                    base = destination / relative.parent if member.issym() else destination
                    inside(base / link, destination)
            tar.extractall(destination, members=members, filter="data")
    # The data filter checks links at extraction time; validate the final link
    # graph too, including links whose targets appeared later in the archive.
    for path in destination.rglob("*"):
        inside(path, destination)
        if path.is_symlink() and not path.exists():
            raise VerificationError("Archive contains a broken symbolic link.")


def read_json(path: Path, limit: int = 32 * 1024**2):
    if not path.is_file() or path.stat().st_size > limit:
        raise VerificationError("Required release metadata is missing or too large.")
    return json.loads(path.read_text(encoding="utf-8"))


def require_file(root: Path, relative: str, limit: int | None = None) -> Path:
    path = root / relative
    inside(path, root)
    if not path.is_file() or not path.stat().st_size:
        raise VerificationError(f"Required portable file is missing or empty: {relative}")
    if limit is not None and path.stat().st_size > limit:
        raise VerificationError(f"Portable file exceeds its size limit: {relative}")
    return path


def validate_layout(destination: Path, target: str) -> tuple[Path, str]:
    roots = list(destination.iterdir())
    if len(roots) != 1 or not roots[0].is_dir() or roots[0].is_symlink():
        raise VerificationError("A binary archive must have exactly one ordinary root directory.")
    root = roots[0]
    label, system, _ = TARGETS[target]
    match = re.fullmatch(r"RustCarPlay-(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?)-" + re.escape(label), root.name)
    if not match:
        raise VerificationError("Archive root name does not match its version and target.")
    version = match.group(1)
    manifest = Path(__file__).resolve().parents[1] / "Cargo.toml"
    if manifest.is_file():
        with manifest.open("rb") as stream:
            expected = tomllib.load(stream)["workspace"]["package"]["version"]
        if version != expected:
            raise VerificationError("Archive version does not match the current workspace release version.")
    suffix = ".exe" if system == "Windows" else ""
    for executable in ["RustCarPlay", "app/carplay-desktop", "app/rustcarplay"]:
        path = require_file(root, executable + suffix)
        if os.name != "nt" and not os.access(path, os.X_OK):
            raise VerificationError("A packaged application is not executable.")
    if system == "Windows":
        for helper in ["usb_probe", "usb_mode", "usb_runtime_check"]:
            require_file(root, f"tools/{helper}.exe")
    for forbidden in [".local", ".git", "target", "INSTALLATION.json"]:
        if (root / forbidden).exists():
            raise VerificationError("The portable archive contains development or user state.")
    require_file(root, "resources/auth/identity.pk8", 16 * 1024)
    require_file(root, "resources/auth/certificate.p7b", 1024**2)
    provenance = read_json(require_file(root, "resources/auth/provenance.json"), 64 * 1024)
    if not isinstance(provenance, dict) or provenance.get("schema_version") != 1 or provenance.get("upstream_project") != "DiPlay":
        raise VerificationError("Authentication provenance is missing its expected schema/source.")
    if not re.fullmatch(r"[0-9a-f]{64}", provenance.get("apk_sha256", "")) or not provenance.get("upstream_url", "").startswith("https://github.com/"):
        raise VerificationError("Authentication provenance is incomplete.")
    if set(provenance.get("apk_entries", [])) != {"assets/offline-mfi/identity.pk8", "assets/offline-mfi/certificate.p7b"}:
        raise VerificationError("Authentication provenance names unexpected APK entries.")
    return root, version


def validate_runtime_manifest(root: Path, target: str) -> None:
    record = read_json(require_file(root, "runtime/NATIVE-MANIFEST.json"))
    if not isinstance(record, dict) or record.get("schema") != 1 or record.get("target") != target:
        raise VerificationError("Native manifest schema or target does not match the archive.")
    if record.get("codec_preflight") is not True or not isinstance(record.get("required_elements"), list) or not record["required_elements"]:
        raise VerificationError("Native manifest lacks codec verification evidence.")
    files = record.get("files")
    if not isinstance(files, list) or not files:
        raise VerificationError("Native manifest has no runtime files.")
    seen = set()
    for entry in files:
        relative = archive_path(entry["path"])
        if relative.parts[0] != "runtime" or str(relative) in seen:
            raise VerificationError("Native manifest contains an unexpected or duplicate file path.")
        seen.add(str(relative))
        path = root / relative
        inside(path, root)
        if not path.is_file():
            raise VerificationError("A native manifest file is missing.")
        if path.stat().st_size != entry["bytes"]:
            raise VerificationError("A bundled native file has an incorrect size.")
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        if digest != entry["sha256"]:
            raise VerificationError("A bundled native file failed integrity verification.")
    licenses = root / archive_path(record.get("license_directory", ""))
    inside(licenses, root)
    if not licenses.is_dir() or not any(path.is_file() for path in licenses.rglob("*")):
        raise VerificationError("Bundled native license notices are missing.")
    if not isinstance(record.get("source_assets"), list) or not record["source_assets"]:
        raise VerificationError("Native manifest lacks corresponding-source asset information.")


def clean_environment(temporary: Path, system: str) -> dict[str, str]:
    prefixes = ("GST", "GSTREAMER", "RUSTCARPLAY", "ALSA_", "LD_", "DYLD_", "GI_", "GIO_", "GOBJECT_", "PKG_CONFIG", "CARGO", "RUSTUP", "VCPKG", "CMAKE", "CONDA")
    removed = {"PATH", "LIB", "LIBPATH", "INCLUDE", "RUSTFLAGS", "RUSTDOCFLAGS", "VIRTUAL_ENV", "PYTHONPATH", "PYTHONHOME", "DEVELOPER_DIR", "PSMODULEPATH", "HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"}
    environment = {key: value for key, value in os.environ.items()
                   if key.upper() not in removed and not key.upper().startswith(prefixes)}
    if system == "Windows":
        windows = Path(os.environ.get("SystemRoot", r"C:\Windows"))
        environment["PATH"] = os.pathsep.join(str(path) for path in [windows / "System32", windows, windows / "System32/Wbem"])
    else:
        environment["PATH"] = "/usr/bin:/bin:/usr/sbin:/sbin"
    scratch = temporary / "process temporary files"
    scratch.mkdir()
    home = temporary / "isolated user profile"
    home.mkdir()
    for name, path in {
        "HOME": home,
        "USERPROFILE": home,
        "LOCALAPPDATA": home / "AppData/Local",
        "APPDATA": home / "AppData/Roaming",
        "XDG_DATA_HOME": home / ".local/share",
        "XDG_CONFIG_HOME": home / ".config",
        "XDG_CACHE_HOME": home / ".cache",
    }.items():
        path.mkdir(parents=True, exist_ok=True)
        environment[name] = str(path)
    for name in ["TEMP", "TMP", "TMPDIR"]:
        environment[name] = str(scratch)
    return environment


def run_captured(command: list[str], cwd: Path, environment: dict[str, str], label: str) -> bytes:
    options = {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == "nt" else {"start_new_session": True}
    with subprocess.Popen(command, cwd=cwd, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **options) as process:
        try:
            stdout, _stderr = process.communicate(timeout=TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            # Only terminate this verifier's newly created process group/tree;
            # never select by executable name or touch a running developer app.
            try:
                if os.name == "nt":
                    taskkill = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32/taskkill.exe"
                    subprocess.run([str(taskkill), "/PID", str(process.pid), "/T", "/F"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=False)
                else:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            finally:
                process.kill()
                process.communicate(timeout=10)
            raise VerificationError(f"{label} exceeded the 30-second timeout; its own process tree was stopped.") from None
        if process.returncode:
            raise VerificationError(f"{label} failed with exit code {process.returncode}; child output was withheld.")
        return stdout


def verify_windows_standalone_imports(executable: Path) -> None:
    """Launchers and USB helpers must load without the application's DLL path."""
    label = f"Windows executable {executable.name}"
    data = executable.read_bytes()
    if data[:2] != b"MZ":
        raise VerificationError(f"{label} is not a PE executable.")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise VerificationError(f"{label} has an invalid PE signature.")
    machine, sections = struct.unpack_from("<HH", data, pe + 4)
    optional_size = struct.unpack_from("<H", data, pe + 20)[0]
    optional = pe + 24
    if machine != 0x8664 or struct.unpack_from("<H", data, optional)[0] != 0x20B:
        raise VerificationError(f"{label} is not a 64-bit executable.")

    def offset(rva: int) -> int:
        for index in range(sections):
            header = optional + optional_size + index * 40
            size, virtual, raw_size, raw = struct.unpack_from("<IIII", data, header + 8)
            if virtual <= rva < virtual + max(size, raw_size):
                location = raw + rva - virtual
                if location < len(data):
                    return location
        raise VerificationError(f"{label} has an invalid import address.")

    imports_rva = struct.unpack_from("<I", data, optional + 112 + 8)[0]
    descriptor = offset(imports_rva)
    for _ in range(512):
        entry = struct.unpack_from("<IIIII", data, descriptor)
        if not any(entry):
            return
        start = offset(entry[3])
        end = data.find(b"\0", start, start + 256)
        if end < 0:
            raise VerificationError(f"{label} has an invalid import name.")
        name = data[start:end].decode("ascii").lower()
        if name.startswith(("vcruntime", "msvcp", "msvcr", "ucrtbase", "api-ms-win-crt", "libgst", "gstreamer", "glib", "libglib", "gobject", "libgobject", "gio", "libgio")):
            raise VerificationError(f"{label} depends on an external CRT or media runtime: {name}")
        descriptor += 20
    raise VerificationError(f"{label} has too many import descriptors.")


def verify_macos_dependencies(root: Path, environment: dict[str, str], *, minimum_binaries: int = 3) -> None:
    binaries = set()
    for candidate in [root / "RustCarPlay", *(root / "app").rglob("*"), *(root / "runtime").rglob("*")]:
        if candidate.is_file():
            with candidate.open("rb") as stream:
                if stream.read(4) in MACHO_MAGICS:
                    binaries.add(inside(candidate, root))
    if len(binaries) < minimum_binaries:
        raise VerificationError("The macOS archive does not contain all expected Mach-O applications.")
    paths = sorted(binaries)
    for start in range(0, len(paths), 32):
        output = run_captured(["/usr/bin/otool", "-L", *map(str, paths[start:start + 32])], root, environment, "macOS dependency inspection")
        if b"/Library/Frameworks/GStreamer.framework" in output:
            raise VerificationError("A Mach-O binary still depends on the build machine's GStreamer framework.")


def matches_cli_version(output: bytes, version: str) -> bool:
    # Clap currently reports the Cargo package name; accept the public command
    # name too, while still requiring the complete, exact release version.
    return output.decode("utf-8", errors="replace").strip() in {
        f"carplay-cli {version}", f"rustcarplay {version}",
    }


def verify_installed_launch(root: Path, launcher: Path, working: Path, environment: dict[str, str], system: str, version: str) -> None:
    # Modify only our temporary extraction. Installer builds add this same marker
    # beside the launcher; actual user data must never be touched by verification.
    original_log = (root / ".local/logs/launcher.log").read_bytes()
    with (root / "INSTALLATION.json").open("x", encoding="utf-8") as stream:
        json.dump({"schema": 1, "mode": "installed", "product": "RustCarPlay"}, stream)
    output = run_captured([str(launcher), "--cli", "--version"], working, environment, "Installed CLI version check")
    if not matches_cli_version(output, version):
        raise VerificationError("The installed CLI did not report the expected release version.")
    output = run_captured([str(launcher), "--cli", "auth-check"], working, environment, "Installed identity self-check")
    if b"key_matches_certificate: true" not in output or b"iphone_trust_verified: false" not in output or b"Local consistency passed" not in output:
        raise VerificationError("Installed identity self-check did not report the expected local-only result.")
    del output
    if system == "Windows":
        data = Path(environment["LOCALAPPDATA"]) / "RustCarPlay"
    elif system == "Darwin":
        data = Path(environment["HOME"]) / "Library/Application Support/RustCarPlay"
    else:
        data = Path(environment["XDG_DATA_HOME"]) / "rustcarplay"
    if not (data / ".local/logs/launcher.log").is_file() or not (data / ".local/gstreamer").is_dir():
        raise VerificationError("Installed mode did not create state in the isolated user data directory.")
    if (root / ".local/logs/launcher.log").read_bytes() != original_log:
        raise VerificationError("Installed mode still writes to the application installation directory.")
    print("Installed launch checks passed using isolated user data and bundled identity paths.")


def verify(archive: Path, target: str, gui_smoke: bool) -> None:
    _label, system, architecture = TARGETS[target]
    host_arch = {"amd64": "x86_64", "arm64": "aarch64"}.get(platform.machine().lower(), platform.machine().lower())
    if platform.system() != system or host_arch != architecture:
        raise VerificationError("Run the portable verifier on the archive's operating system and CPU architecture.")
    if gui_smoke and system != "Windows":
        raise VerificationError("--gui-smoke is currently supported only on Windows.")
    with tempfile.TemporaryDirectory(prefix="RustCarPlay portable verification ") as temporary:
        temporary = Path(temporary)
        destination = temporary / "extracted bundle"
        destination.mkdir()
        safe_extract(archive, destination)
        root, version = validate_layout(destination, target)
        validate_runtime_manifest(root, target)
        environment = clean_environment(temporary, system)
        working = temporary / "unrelated current directory"
        working.mkdir()
        launcher = root / ("RustCarPlay.exe" if system == "Windows" else "RustCarPlay")
        if system == "Windows":
            for executable in [launcher, *(root / f"tools/{name}.exe" for name in ["usb_probe", "usb_mode", "usb_runtime_check"])]:
                verify_windows_standalone_imports(executable)
            if launcher.read_bytes() == (root / "app/rustcarplay.exe").read_bytes():
                raise VerificationError("The Windows launcher and CLI are identical; their build output names must not collide.")
        if system == "Darwin":
            verify_macos_dependencies(root, environment)
        output = run_captured([str(launcher), "--cli", "--version"], working, environment, "CLI version check")
        if not matches_cli_version(output, version):
            raise VerificationError("The bundled CLI did not report the expected release version.")
        print("Portable CLI version check passed in the isolated runtime environment.")
        output = run_captured([str(launcher), "--cli", "auth-check"], working, environment, "Bundled identity self-check")
        if b"key_matches_certificate: true" not in output or b"iphone_trust_verified: false" not in output or b"Local consistency passed" not in output:
            raise VerificationError("Bundled identity self-check did not report the expected local-only result.")
        del output
        print("Bundled identity self-check passed; no device connection was attempted.")
        if gui_smoke:
            run_captured([str(launcher), "--smoke-test"], working, environment, "Windows GUI smoke test")
            print("Windows GUI smoke test passed and the test application closed normally.")
        verify_installed_launch(root, launcher, working, environment, system, version)
        print("Portable archive layout, native manifest and launch verification passed.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--gui-smoke", action="store_true", help="Windows only: briefly launch and automatically close a new test GUI.")
    arguments = parser.parse_args()
    try:
        verify(arguments.archive.resolve(strict=True), arguments.target, arguments.gui_smoke)
        return 0
    except VerificationError as error:
        print(f"Portable verification failed: {error}", file=sys.stderr)
    except (OSError, ValueError, KeyError, TypeError, struct.error, tarfile.TarError, zipfile.BadZipFile, subprocess.SubprocessError):
        # Never dump child output, authentication metadata or a traceback.
        print("Portable verification failed while reading the archive, metadata or executable.", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
