# Build and release workflows

`ci.yml` runs on branch pushes, pull requests, manual dispatch, and reusable-workflow calls. Windows Server 2022 and Ubuntu 24.04 run formatting, packaging tests, default-feature Clippy and workspace tests. Manual dispatch accepts `package: true` to build and inspect offline package previews from a branch such as `main`; this does not publish a GitHub release.

`rust-toolchain.toml` pins the compiler, formatter and Clippy version for both local development and CI. Upgrade that file explicitly when adopting a newer Rust release.

The native GStreamer matrix uses Windows x64, Ubuntu 24.04 x64/ARM64, and macOS 15 Intel/Apple Silicon. Each target checks native codecs, runs Clippy and links/runs workspace tests. Windows and Linux x64 additionally run the ignored synthetic media tests. Those tests use software decoding, appsink and audiotestsrc; they do not open speakers, microphones or connect an iPhone.

The Windows SDK is prepared from verified official GStreamer native archives. macOS uses pinned universal framework packages. Linux uses distro packages. For v0.1.1, selected native runtime libraries/plugins and dependency closures are also included in binary packages. The root Rust launcher configures them for the child process; ordinary launch does not depend on Rust, Python, PowerShell or an installed GStreamer runtime.

Packages contain `app/`, `runtime/gstreamer/`, platform USB user-space libraries and fixed experimental `resources/auth/` material with provenance. Authentication preparation verifies the complete pinned DiPlay v0.2.15 APK before extracting only the required bounded entries. Private keys never enter Git or Rust source archives. Public availability does not establish redistribution permission; the experimental identity's distribution suitability and continued iOS acceptance remain unresolved. See [third-party notices](../../docs/THIRD_PARTY_NOTICES.md).

`release.yml` runs on `v*` tag pushes. Manual dispatch of **that workflow** must also select an existing version tag. It verifies the tag against every workspace package version, calls the complete CI matrix, builds optimized launcher/desktop/CLI executables, and uploads five platform archives. A separate job produces the source archive with locked vendored Rust dependency sources/licenses and relative offline Cargo configuration. The source includes the launcher and preparation scripts, but excludes authentication private keys.

The v0.1.1 asset contract is five binary archives, one Rust source archive, the shared `cerbero-1.28.7.tar.xz` GStreamer source archive, three native-source archives (Windows x64 and Linux x64/ARM64), and `SHA256SUMS`: **11 attachments total**. Per-platform native source supplements the shared archive with the other bundled native components and licensing/provenance materials. The exact names are listed in the [v0.1.1 notes](../../docs/releases/v0.1.1.md).

The final publish job is the only job with `contents: write`. It waits for all required builds and source artifacts, requires the complete asset set, creates SHA-256 checksums, uploads to a draft, then publishes it. Failed uploads remain drafts; an existing public release is never overwritten. Release notes come from `docs/releases/<tag>.md` when available.

To publish a new version, update all workspace package versions and Cargo.lock, add release notes, pass branch CI, then push the matching `v<version>` tag. Do not move published version tags.

The new v0.1.1 offline package workflow and environment checks are still being validated; this document is not evidence of a successful run. Inspect the specific workflow and its artifacts before claiming a target passed. Packaging checks cannot establish a completely clean-host installation or device interoperability.

Windows USB still needs Apple Mobile Device Service and explicit system preparation; its PowerShell 7/SDK prerequisites are not eliminated by the Rust launcher. Linux targets Ubuntu 24.04 / glibc 2.39 and still relies on OS desktop/graphics/audio drivers, BlueZ, NetworkManager, usbmuxd and permissions. macOS 15 packages are not notarized and are GUI/core previews with incomplete native Bluetooth/USB/network adapters.

CI proves the tested compilation/linking and synthetic cases only. Linux device interoperability is unverified. No hosted job validates real Bluetooth pairing, USB drivers, iPhone authentication, audio devices, latency, sleep/resume, or vehicle integration. See [acceptance](../../docs/ACCEPTANCE.md).
