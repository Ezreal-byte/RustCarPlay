# Build and release workflows

`ci.yml` runs on branch pushes, pull requests, manual dispatch, and reusable-workflow calls. Windows Server 2022 and Ubuntu 24.04 run formatting, packaging tests, default-feature Clippy and workspace tests.

The native GStreamer matrix uses Windows x64, Ubuntu 24.04 x64/ARM64, and macOS 15 Intel/Apple Silicon. Each target checks native codecs, runs Clippy and links/runs workspace tests. Windows and Linux x64 additionally run the ignored synthetic media tests. Those tests use software decoding, appsink and audiotestsrc; they do not open speakers, microphones or connect an iPhone.

The Windows SDK is prepared from verified official GStreamer native archives. macOS uses pinned universal framework packages. Linux uses distro packages. These native runtimes are build dependencies; they are not included in release archives.

`release.yml` runs on `v*` tag pushes. Manual dispatch must also select an existing version tag. It verifies the tag against every workspace package version, calls the complete CI matrix, builds optimized desktop/CLI executables, and uploads five platform archives. A separate job produces the source archive with locked vendored Rust dependency sources/licenses and relative offline Cargo configuration.

The final publish job is the only job with `contents: write`. It waits for all builds and the source archive, requires the complete asset set, creates SHA-256 checksums, uploads to a draft, then publishes it. Failed uploads remain drafts; an existing public release is never overwritten. Release notes come from `docs/releases/<tag>.md` when available.

To publish a new version, update all workspace package versions and Cargo.lock, add release notes, pass branch CI, then push the matching `v<version>` tag. Do not move published version tags.

CI proves the tested compilation/linking and synthetic cases only. Linux device interoperability is unverified; macOS native Bluetooth/USB/network adapters remain incomplete. No hosted job validates real Bluetooth pairing, USB drivers, iPhone authentication, audio devices, latency, sleep/resume, or vehicle integration. See [acceptance](../../docs/ACCEPTANCE.md).
