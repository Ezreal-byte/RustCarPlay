"""Align only bundled native dependencies with available exact Ubuntu sources.

Run after installing native SDKs and enabling/updating deb-src, before Rust
compilation. Checking is read-only; --apply permits targeted package upgrades
only in GitHub Actions on the matching native Ubuntu 24.04 runner. Packaging
still resolves every binary's actual dpkg source version independently.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess

from native_runtime_sources import apt_source_locations, write_json

SPEC = importlib.util.spec_from_file_location("native_bundle", Path(__file__).with_name("bundle-native-runtime.py"))
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)

TARGETS = {"x86_64-unknown-linux-gnu": "x86_64", "aarch64-unknown-linux-gnu": "aarch64"}


def run(command: list[str]) -> str:
    # setup-python adds its own LD_LIBRARY_PATH. Inspect the distribution's
    # libraries, not unrelated copies shipped in the hosted tool cache.
    environment = {key: value for key, value in os.environ.items() if key not in ("LD_LIBRARY_PATH", "LD_PRELOAD")}
    environment["LC_ALL"] = "C"
    return subprocess.check_output(command, text=True, env=environment, timeout=120).rstrip("\r\n")


def native_inputs(target: str) -> tuple[list[Path], list[Path]]:
    """Use the bundler's plugin set and include its explicit dlopen/data inputs."""
    lib = Path("/usr/lib") / (TARGETS[target] + "-linux-gnu")
    binaries = [lib / "gstreamer-1.0" / ("libgst" + name + ".so")
                for name in bundle.PLUGINS + ("alsa", "pulseaudio")]
    binaries.extend(Path(shutil.which(name) or "/missing/" + name) for name in ("gst-inspect-1.0", "gst-launch-1.0"))
    scanners = [lib / "gstreamer1.0/gstreamer-1.0/gst-plugin-scanner", lib / "gstreamer-1.0/gst-plugin-scanner"]
    scanner = next((path for path in scanners if path.is_file()), None)
    if scanner is None:
        raise RuntimeError("Ubuntu GStreamer plugin scanner is missing")
    binaries.append(scanner)
    binaries.extend(lib / name for name in ("libbluetooth.so.3", "libimobiledevice-1.0.so.6"))
    binaries.extend(sorted((lib / "alsa-lib").glob("*.so")))
    data = [path for path in sorted(Path("/usr/share/alsa").rglob("*")) if path.is_file()]
    if not data:
        raise RuntimeError("Ubuntu ALSA configuration is missing")
    return binaries, data


def installed_packages(target: str) -> list[dict]:
    binaries, data = native_inputs(target)
    owners = {bundle.dpkg_owner(path) for path in data}
    queue = list(binaries)
    visited = set()
    while queue:
        path = queue.pop()
        resolved = path.resolve()
        if resolved in visited:
            continue
        if not path.is_file():
            raise RuntimeError("Missing native runtime input: " + str(path))
        visited.add(resolved)
        owners.add(bundle.dpkg_owner(path))
        for name, dependency in bundle.parse_ldd(run(["ldd", str(path)])).items():
            if not bundle.LINUX_SYSTEM.fullmatch(name):
                queue.append(dependency)
    packages = []
    for owner in sorted(owners):
        fields = run(["dpkg-query", "-W", "-f=${binary:Package}\t${Version}\t${source:Package}\t${source:Version}", owner]).split("\t")
        if len(fields) != 4:
            raise RuntimeError("Incomplete dpkg source provenance for " + owner)
        binary, version, source, source_version = fields
        packages.append({"binary_package": binary, "binary_version": version,
                         "source_package": source or binary.split(":", 1)[0], "source_version": source_version or version})
    return packages


def candidate_package(package: dict) -> dict:
    binary = package["binary_package"]
    policy = run(["apt-cache", "policy", binary])
    match = re.search(r"^\s*Candidate: (\S+)\s*$", policy, re.MULTILINE)
    if match is None or match[1] == "(none)":
        raise RuntimeError("No candidate binary with corresponding source: " + binary)
    version = match[1]
    newer = subprocess.run(["dpkg", "--compare-versions", version, "gt", package["binary_version"]], check=False)
    if newer.returncode != 0:
        raise RuntimeError("No newer candidate for missing exact source: " + binary + "=" + package["binary_version"])
    records = run(["apt-cache", "show", "--no-all-versions", binary + "=" + version])
    for paragraph in records.split("\n\n"):
        fields = dict(re.findall(r"^([A-Za-z][A-Za-z0-9-]*): (.*)$", paragraph, re.MULTILINE))
        if fields.get("Package") != binary.split(":", 1)[0] or fields.get("Version") != version:
            continue
        source = re.fullmatch(r"([a-z0-9][a-z0-9+.-]*)(?: \(([^)]+)\))?", fields.get("Source", fields["Package"]))
        if source is None:
            raise RuntimeError("Invalid candidate source provenance for " + binary)
        return {"binary_package": binary, "binary_version": version,
                "source_package": source[1], "source_version": source[2] or version}
    raise RuntimeError("Candidate binary metadata is missing: " + binary + "=" + version)


def upgrade_packages(packages: list[dict]) -> None:
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("Package upgrades are restricted to GitHub Actions")
    requests = sorted({package["binary_package"] + "=" + package["binary_version"] for package in packages})
    if not requests:
        return
    # Pin each reviewed candidate; no upgrade/dist-upgrade and no removals.
    command = ["sudo", "-n", "apt-get", "install", "--only-upgrade", "--no-install-recommends", "--no-remove", "-y", *requests]
    print("Upgrading native runtime owners with unavailable old source: " + ", ".join(requests), flush=True)
    subprocess.run(command, check=True)


def align_sources(target: str, apply: bool) -> dict:
    history = []
    checked = {}
    for attempt in range(4):
        packages = installed_packages(target)
        unavailable = set()
        for key in sorted({(p["source_package"], p["source_version"]) for p in packages}):
            if key in checked:
                continue
            try:
                checked[key] = apt_source_locations(*key)
            except subprocess.CalledProcessError:
                unavailable.add(key)
        if not unavailable:
            return {"schema": 1, "target": target, "binary_packages": packages, "upgrades": history,
                    "sources": [{"name": name, "version": version, "uris": checked[(name, version)]}
                                for name, version in sorted({(p["source_package"], p["source_version"]) for p in packages})]}
        missing = ", ".join(name + "=" + version for name, version in sorted(unavailable))
        if not apply:
            raise RuntimeError("Exact source versions unavailable: " + missing + "; run --apply in CI before compiling")
        if attempt == 3:
            raise RuntimeError("Exact source versions still unavailable after targeted upgrades: " + missing)
        candidates = [candidate_package(package) for package in packages
                      if (package["source_package"], package["source_version"]) in unavailable]
        # Check every candidate source before changing any package. Missing
        # deb-src, broken indexes, or an unavailable new source cannot lead to
        # unverified upgrades or a latest-source substitution.
        for key in sorted({(p["source_package"], p["source_version"]) for p in candidates}):
            if key not in checked:
                checked[key] = apt_source_locations(*key)
        upgrade_packages(candidates)
        history.append(candidates)
        # Recompute the ELF closure because upgrading can change dependencies.
    raise AssertionError("unreachable")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--report", type=Path, default=Path(".local/linux-native-sources.json"))
    args = parser.parse_args()
    if platform.system() != "Linux" or platform.machine() != TARGETS[args.target]:
        parser.error("Run on the matching native Linux architecture")
    release = dict(line.split("=", 1) for line in Path("/etc/os-release").read_text(encoding="utf-8").splitlines() if "=" in line)
    if release.get("ID", "").strip('"') != "ubuntu" or release.get("VERSION_ID", "").strip('"') != "24.04":
        parser.error("The release baseline is Ubuntu 24.04")
    if args.apply and os.environ.get("GITHUB_ACTIONS") != "true":
        parser.error("--apply is restricted to GitHub Actions")
    report = align_sources(args.target, args.apply)
    write_json(args.report, report)
    print(json.dumps({"binary_packages": len(report["binary_packages"]), "exact_sources": len(report["sources"]),
                      "upgrade_batches": len(report["upgrades"]), "report": str(args.report)}))


if __name__ == "__main__":
    main()
