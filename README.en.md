# RustCarPlay

[中文](README.md) · **English**

A Rust CarPlay receiver based on [DiPlay](https://github.com/shihabal3amri/DiPlay). Shared Rust code implements the protocols and sessions, native adapters connect to each operating system, **egui / wgpu** provides the desktop UI, and **GStreamer** handles media.

**v0.1.0 is an early development preview.** Wireless and USB connections work with one tested iPhone on Windows. Full DiPlay feature parity, long-running stability and broader device compatibility remain unfinished.

[Releases](https://github.com/Ezreal-byte/RustCarPlay/releases) · [Development guide](docs/DEVELOPMENT.md) · [Acceptance record](docs/ACCEPTANCE.md) · [Issues](https://github.com/Ezreal-byte/RustCarPlay/issues)

## Screenshots

![Placeholder for the RustCarPlay connection and player screens](docs/screenshots/placeholder.svg)

*This is a labeled placeholder, not a device screenshot. See the [screenshot contribution guide](docs/screenshots/README.md).*

## Platform status

| Platform | v0.1.0 status |
| --- | --- |
| Windows 11 x64 | One iPhone tested: wireless/USB video, touch and audio; USB pause and reconnect also passed an initial user test |
| Linux x64 / ARM64 | Native CI build targets; USB, BlueZ and NetworkManager backends implemented, with no physical-device acceptance yet |
| macOS Intel / Apple Silicon | GUI/core build preview; native RFCOMM, USB and network configuration adapters are unfinished, so receiver connections are unavailable |
| Android | Shared-core port planned; Kotlin/JNI shell and platform adapters not implemented |
| HarmonyOS / NEXT | ArkTS/native bridge planned; required APIs, permissions and device capabilities remain unverified |

A build target does not prove a successful release or working device connection. Check Releases and workflow results for available artifacts. Twenty connection cycles, a two-hour session, Siri/calls, audio device switching and sleep recovery remain unverified.

## What is implemented

- **Three connection configurations:** LAN, computer hotspot and USB. LAN uses the current system Wi-Fi profile without an in-app password field. Actual hotspot operation and phone connection still need testing.
- **Shared protocols:** iAP2, USBMUX/NCM, Lockdown/CarKit integration, accessory authentication, AirPlay pairing, encrypted control/media, discovery and session cleanup.
- **Media and input:** H.264/HEVC software decoding, AAC/Opus/LPCM playback, touch input and track/navigation metadata. Microphone return has implementation and synthetic tests, but lacks device acceptance.
- **Desktop UI:** connection/settings home, automatic player navigation on the first decoded frame, custom title bar, fullscreen, day/night controls and local diagnostics. The current UI is primarily Chinese.

`carplay-protocol`, `carplay-auth`, `carplay-wireless` and `carplay-receiver` implement protocols; `carplay-platform` owns system adapters, `carplay-media` handles playback, and `carplay-app` shares session lifecycle. See the [architecture](docs/ARCHITECTURE.md). Hardware decoding, complete second-screen support, P2P, AEC, parked video and vehicle extensions are not delivered.

## Getting started

1. Choose your OS/architecture from [Releases](https://github.com/Ezreal-byte/RustCarPlay/releases) and verify `SHA256SUMS`.
2. Prepare the **GStreamer runtime and codec plugins** using the [development guide](docs/DEVELOPMENT.md). Release archives do not bundle GStreamer, USB user-space DLLs or drivers. Included preparation scripts run only when explicitly invoked; they do not automatically request elevation or install drivers.
3. Provide `identity.pk8` and `certificate.p7b` locally. These are an accessory private key and certificate, unrelated to your Apple ID password. **Neither source nor release archives include them.** A local key check does not prove iPhone authentication.
4. For LAN, join the same Wi-Fi on the computer and iPhone, pair Bluetooth in system settings, then connect. For USB, read the [USB prerequisites](docs/USB.md); Windows also needs Apple Mobile Device Service, the USB user-space runtime and [reversible device configuration](docs/WINDOWS_USB_DRIVER.md).

The desktop executable is `carplay-desktop`; the CLI is `rustcarplay` (with `.exe` on Windows). Build, launch, troubleshooting and recovery instructions are in the [development guide](docs/DEVELOPMENT.md), with an English quickstart.

## Development

```sh
git clone https://github.com/Ezreal-byte/RustCarPlay.git
cd RustCarPlay
cargo test --workspace --locked
```

Install Rust stable, the platform toolchain and desktop system libraries. Media-enabled builds also need GStreamer development files. Default tests are not native-media or physical-device acceptance. Start with the [architecture](docs/ARCHITECTURE.md), [remaining acceptance work](docs/ACCEPTANCE.md), or the bilingual [Android / HarmonyOS porting plan](docs/MOBILE_PORTING.md).

## Attribution and license

The fixed reference is DiPlay commit [`9e244d958afe6b8fd79ade49769ce25a944f397b`](https://github.com/shihabal3amri/DiPlay/tree/9e244d958afe6b8fd79ade49769ce25a944f397b), whose receiver originates from [xcertplay](https://github.com/shilapi/xcertplay). This project ports protocol behavior to Rust and retains source attribution; it does not wrap an Android APK for desktop use.

Project code is [GPL-3.0-only](LICENSE). External runtimes, upstream assets and dependencies retain their own licenses; see [third-party notices](docs/THIRD_PARTY_NOTICES.md). The CarPlay name and original icon belong to Apple and are outside the project's GPL source license. This project does not imply Apple certification or endorsement.
