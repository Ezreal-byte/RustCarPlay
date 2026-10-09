# Installer packaging

`scripts/package-installers.py` wraps an already verified portable archive.
Build and verify each installer on its target operating system and architecture:

```text
python scripts/package-installers.py --archive <portable archive> --target <Rust target> --output dist
python scripts/package-installers.py --verify <installer> --target <Rust target>
```

Windows uses Inno Setup 6, installed in the GitHub `windows-2022` image. Setup is
per-user, offers English and simplified Chinese, and does not install drivers or
system services. Verification uses a temporary application directory and unique
uninstall registration, suppresses shortcuts and automatic launch, then removes
the test installation. User data remain outside the installation directory.

macOS uses the system `sips`, `iconutil`, `codesign`, and `hdiutil` tools. The DMG
contains `RustCarPlay.app` and an Applications shortcut. It is ad-hoc signed,
not notarized. Verification mounts the image read-only. Native macOS connection
adapters remain incomplete; this packaging does not change that feature boundary.

Linux uses `dpkg-deb` on Ubuntu 24.04 and installs under `/opt/rustcarplay` with a
menu entry and `/usr/bin/rustcarplay`. DEB metadata declares the required system
services and desktop libraries. Verification extracts into a temporary directory;
it does not invoke dpkg installation or start services.

Every installed layout places `INSTALLATION.json` in the payload root. On macOS, only the signed launcher is in `Contents/MacOS`; the payload is in `Contents/Resources/payload`. Other platforms keep the payload beside the launcher.
The launcher reads bundled resources from the installation and writes state to
the platform's user data directory. Uninstallers never delete that personal data.
Matching source and license attachments are the same as for the portable build.
