# 开发与运行 / Development and operation

[中文首页](../README.md) · [English overview](../README.en.md) · [English quickstart](#english-quickstart)

本文保留完整开发与连接说明，命令默认在仓库根目录执行。v0.1.0 发布包提供程序与准备脚本，不包含 GStreamer、USB 用户态 DLL、驱动、配件认证身份或本地配对记录；源码归档和 `SHA256SUMS` 以 [Releases](https://github.com/Ezreal-byte/RustCarPlay/releases) 为准。准备脚本需要主动执行，系统 USB 配置是单独的管理员操作。

基于 [DiPlay](https://github.com/shihabal3amri/DiPlay) `9e244d958afe6b8fd79ade49769ce25a944f397b` 的 Rust 跨平台接收端移植。协议核心不依赖 Android，桌面使用 egui/wgpu 和 GStreamer。

**当前是正在进行真机验证的开发原型，尚未完成 DiPlay 的全部功能，也没有通过 Windows/Linux 完整 CarPlay 验收。** 编译、协议测试、localhost 加密会话测试和合成媒体解码，不等同于 iPhone 互操作验证。详见 [验收与缺口](ACCEPTANCE.md)。如需对照，可将固定版本的上游源码另行检出至被忽略的 `DiPlay/` 目录；公开仓库与本 Cargo workspace 不包含该副本。

## 已实现的代码路径

- 有限长度的 iAP2 Link/TLV、USBMUX/NCM 编解码；ACK、重传、会话取消；曲目、导航和通话元数据解析。
- 外部 MFi P-256 身份、MFiSAP、SRP Pair Setup、Ed25519/X25519 Pair Verify、加密控制通道及配对保存。Windows 配对数据由当前用户 DPAPI 保护，Unix 文件权限为 0600。
- Windows/Linux 原生 RFCOMM 客户端；iAP2 身份/认证/订阅/Wi-Fi/启动会话；发布 `_airplay`，发现所选 iPhone 的 `_carplay-ctrl` 并发送连接请求。
- AirPlay 控制监听、事件通道、加密视频/UDP 音频、type 130 iAP2 隧道、部分流停止和断线资源清理。
- GStreamer H.264/HEVC 软件解码至 RGBA、AAC/Opus/LPCM 音频输出；有界输入队列、最新帧窗口和异步错误报告。硬件解码尚未交付：本机 HEVC D3D12 测试失败后选择软件路径。
- 可选麦克风：认证并协商成功后才启用 LPCM/Opus 采集，以独立 Input 密钥加密 UDP 回传；默认关闭。合成音源到加密 UDP 的本地端到端测试已通过；真机 Siri/通话与 AEC 尚未验收。
- 中文桌面界面、绿色 CarPlay 图标、自定义窗口顶栏、连接/设置首页与独立画面页；Windows 本机/已配对设备选择、触控、显示设置、诊断；共享 CLI 可检查身份、检查平台或启动接收端。

连接方式已分为局域网、电脑热点和 USB。局域网使用系统已保存的当前 Wi-Fi 配置，无需在软件输入密码；电脑热点使用 Windows Mobile Hotspot 或 Linux NetworkManager，支持复用、启停和资源清理。Windows/Linux USB 均已接入系统 USBMUX、Lockdown、CarKit 和 NCM 后端。2026-10-09，本机 Windows USB 已在一台 iPhone 上完成 iAP2/AirPlay 认证，经 UsbNcm 接收视频，并在桌面自动进入画面页、显示 1280×720 的 CarPlay 主页；用户确认点击、返回正常，音频启动修复后声音、暂停和重连也正常。无线声音同样经用户确认正常。完整输入验收、20 次连接循环、两小时稳定性以及 Linux 真机仍待验收，见 [USB 说明](USB.md)。P2P、稳定的音频时钟同步、AEC、停车视频和移动/车辆后端仍在待实现范围。

## Windows 开发与运行

使用 Rust stable、Visual Studio C++ Build Tools、PowerShell 7。`Cargo.lock` 固定依赖。仅做协议测试无需安装媒体 SDK：

```powershell
cargo test --workspace --locked
cargo run -p carplay-cli -- doctor
cargo run -p carplay-cli -- auth-check --assets .local/auth
```

本工作区提供可复现的 GStreamer 本地准备脚本。它从 GStreamer 官方 PyPI 发布下载固定版本并核对发布 SHA-256，仅提取 native 文件，不运行 Python 安装脚本，不修改系统 PATH、注册表或驱动。Python 需为真实可用的 Python 3，不能是空的 Windows Store 执行别名；另需可用的 `pkg-config`（如 Strawberry Perl 的 `pkg-config.bat`）：

```powershell
./scripts/prepare-gstreamer.ps1
python ./scripts/gstreamer-import-libs.py
./scripts/with-gstreamer.ps1 -Command @('cargo','build','-p','carplay-desktop','-p','carplay-cli','--features','gstreamer','--locked')
./scripts/with-gstreamer.ps1 -Command @('./target/debug/carplay-desktop.exe')
```

如果已配置标准 GStreamer MSVC SDK，可直接使用其 PATH 和 pkg-config 环境运行 Cargo，无需上述本地准备过程。运行文件名为 `carplay-desktop.exe` 和 `rustcarplay.exe`。

局域网连接步骤：

1. 电脑和 iPhone 加入同一个 Wi-Fi，并通过 Windows 设置完成蓝牙配对。
2. 首页选择“局域网”和已配对 iPhone。系统当前 Wi-Fi 名称与地址自动显示，直接点击连接，无密码输入框。为兼容 iPhone 的 iAP2 握手，程序只在连接时从所选接口的当前系统配置读取凭据，保存在内存中；不会读其他已保存网络或将密钥写入设置/日志。如果系统拒绝读取，显示权限原因，不把加密网络冒充开放网络。网卡、蓝牙适配器和手动地址在“高级连接设置”中调整。
3. 保持默认 H.264、1280×720、30 fps。点击连接，在 iPhone 上处理实际出现的 CarPlay 提示。
4. 如果发现日志提示 `PhoneSelectionRequired`，填入该 iPhone Wi-Fi 设置中的 IP 后重试；程序不会任意选择局域网其他手机。
5. 收到实际解码的视频首帧后自动进入 CarPlay 画面页。画面保持原比例并居中，可在工具栏切换日夜模式或全屏；`F11` 切换全屏，`Esc` 先退出全屏，再返回首页。“返回首页”保持连接，可随时点击“打开 CarPlay”回到画面，后续视频帧不会强制切页。断开时自动回首页。
6. 首页可调整显示与声音，连接期间设置锁定；在“连接诊断”查看状态。本地会话日志在 `.local/logs/`，原始认证报文、密码、曲目和通话正文不会写入。

自定义顶栏支持拖动、双击最大化、最小化、还原与关闭，窗口边缘可调整大小。首页在较窄窗口中改为单列并可滚动。交互分组参考 [AutoKit 官方设置说明](https://www.carlinkit.com/faq_detail/119.html) 与 [Apple CarPlay 操作指南](https://support.apple.com/en-lamr/guide/iphone/iph5f6b2beb5/ios)；首帧触发和保持首页是本项目的实现选择。

“电脑热点”模式默认沿用系统热点配置；也可只在该模式下自定义名称和密码。选择已配对 iPhone 后连接，系统会启动热点并通过蓝牙引导手机加入。复用原本已开启的热点时，退出不会关闭它；本程序启动的热点在取消、失败或断开时清理，自定义 Windows 配置会恢复原值。系统不支持或禁止控制时，可打开“系统热点设置”处理；当前已验证本机能力查询，实际热点启停与手机连接仍需实测。

“USB”模式不需要蓝牙和 Wi-Fi 参数。插入数据线后刷新并选择 USB iPhone。Linux 按 [USB 前置条件](USB.md) 配置系统组件。Windows 需要 Apple Mobile Device Service、本地 libimobiledevice 运行时，以及所选手机的 NCM 复合配置；软件使用 Apple 的 USBMUX 服务和 Windows UsbNcm 驱动。普通连接不会安装驱动或回退到无线。

Windows USB 的用户态依赖在普通 PowerShell 7 中准备：

```powershell
./scripts/setup-usb-runtime.ps1
```

该脚本从官方 MSYS2 下载 `scripts/usb-runtime-packages.json` 固定的 UCRT64 软件包并验证 SHA-256，只放入 `.local/usb-runtime/`，不修改系统 PATH。USB 配置准备和可选过滤驱动属于单独的管理员操作，执行步骤与恢复方式见 [Windows USB 驱动说明](WINDOWS_USB_DRIVER.md)。本机驱动组合需要手动重插后使用 `start-windows-usb.ps1 -ResumeAfterReplug` 继续准备，具体条件见 [USB 连接后端](USB.md)；首次联调应保持 iPhone 解锁并处理信任提示。

可用 `--smoke-test --screenshot .local/ui-home.png` 启动桌面视觉检查，保存本程序绘制的窗口并自动退出；截图可能包含本机设备名称，请勿直接作为公开诊断附件。

不要把防火墙关闭作为连接步骤；若 Windows 提示网络访问，选择当前用于测试的可信专用网络。网络地址必须是本机实际接口，不能填 `0.0.0.0`。

命令行连接使用 `connect --mode lan|hotspot|usb`，具体参数见 `connect --help`。LAN 的 `--bind` 可省略，自动选择当前 Wi-Fi 地址；LAN/hotspot 仍需 `--local-bluetooth` 与 `--iphone`。USB 使用可选 `--usb-device`，不依赖无线参数。旧 `--ssid`、`--password-env` 参数已移除。`serve` 是供独立 iAP2 引导方使用的低层服务入口，不会单独完成整个连接流程。

## 认证文件

`identity.pk8` 是配件认证私钥，`certificate.p7b` 是配套证书，二者与 Apple ID 密码无关。`LocalIdentity` 按 DiPlay 的 P-256 MFi v3 路径加载它们；发布包和 Git 中不携带这些文件。

当前本地实验从用户指定的 DiPlay 官方 v0.2.15 APK 的 `assets/offline-mfi/` 读取这两个文件，保存在忽略目录 `.local/auth/`。原 APK SHA-256 为 `4bf45f16d6b1ab0a61462b831014081f07240f5596c90ca6bf38fb43f9890511`。上游将其标为实验认证数据，不能把公开 APK 中存在这些数据理解为它们受 GPL 许可或适合随新项目分发。身份一致性检查不验证 iPhone 信任。

## Ubuntu 24.04

参见 [CI](../.github/workflows/ci.yml) 中的实际 apt 包列表。至少需要 GStreamer core/base 开发包、good/bad/ugly/libav 插件、BlueZ 系统库和桌面 X11/Wayland 开发库：

```sh
cargo test --workspace --locked
cargo build --workspace --features gstreamer --locked
cargo test -p carplay-media --features gstreamer --locked -- --ignored
cargo run -p carplay-desktop --features gstreamer
```

Linux RFCOMM 使用 BlueZ SDP/套接字；图形界面的已配对列表自动枚举目前仅 Windows 提供，Linux 需手工提供已配对地址。Windows 上对 Linux target 的 `cargo check` 只是编译检查；Linux 真机、设备权限和休眠恢复仍需在 Ubuntu 实际验收。

## 工程结构与测试

见 [架构](ARCHITECTURE.md)、[验收记录](ACCEPTANCE.md)、[第三方声明](THIRD_PARTY_NOTICES.md)。`fuzz/` 是独立 cargo-fuzz workspace，不随常规 workspace 构建。各协议/平台 crate README 描述它们的边界。

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

GStreamer feature 的编译/测试需要上述 native 环境；不能用 `DOCS_RS=1` 替代 native 链接或解码验收。Android Kotlin/JNI、HarmonyOS ArkTS/NAPI、BYD 适配器尚未实现，不从 Rust target 存在推断产品可用。

本项目代码采用 GPL-3.0-only。桌面界面为新实现，未复制 DiAuto AGPL UI 或 BYD 图标素材。绿色 CarPlay 图标为 Apple 原始素材，不适用代码的 GPL 许可，来源见 [第三方声明](THIRD_PARTY_NOTICES.md)。CarPlay 是 Apple 的商标；项目不表示 Apple 认证。

## English quickstart

Run the commands from the repository root. This project needs Rust stable and the native toolchain for the selected platform. Release binaries also require external GStreamer runtime libraries and codec plugins; source builds additionally need development headers and pkg-config metadata. Release archives do not include accessory identity files, Apple software, USB DLLs or drivers. See the platform sections above for the complete connection and recovery procedure.

### Windows

Install Visual Studio C++ Build Tools, PowerShell 7, a working Python 3, and pkg-config. The workspace scripts obtain pinned official GStreamer files, verify their hashes and prepare a local build/runtime environment. They do not install a system driver or change the global PATH.

```powershell
./scripts/prepare-gstreamer.ps1
python ./scripts/gstreamer-import-libs.py
./scripts/with-gstreamer.ps1 -Command @('cargo','build','-p','carplay-desktop','-p','carplay-cli','--features','gstreamer','--locked')
./scripts/with-gstreamer.ps1 -Command @('./target/debug/carplay-desktop.exe')
```

For USB, first install Apple's mobile-device support and validate trust with the unlocked phone. Prepare the local user-space library with `./scripts/setup-usb-runtime.ps1`, then follow [Windows USB configuration and recovery](WINDOWS_USB_DRIVER.md). Driver preparation is separate from ordinary connection and may require elevation and a physical cable replug. Do not replace a driver or select a configuration merely from another phone's example values.

### Linux and macOS build previews

Linux x64 and ARM64 use native runners and distribution libraries; check the package list in [CI](../.github/workflows/ci.yml). USB requires libimobiledevice/usbmuxd and the correct CDC-NCM interface; wireless requires BlueZ and the appropriate system network configuration. Linux physical-device operation remains unverified.

macOS Intel and Apple Silicon are GUI/core build previews. The build uses the official GStreamer universal runtime and development packages. Make their tools and pkg-config files available to Cargo using the environment from the native CI job. RFCOMM, USB and system network adapters are not implemented for macOS; compiling or opening the interface does not enable a receiver connection.

After native dependencies are installed:

```sh
cargo test --workspace --locked
cargo build --workspace --features gstreamer --locked
cargo test -p carplay-media --features gstreamer --locked -- --include-ignored
cargo run -p carplay-desktop --features gstreamer --locked
```

Synthetic tests decode into appsinks; they do not prove physical audio playback or open a microphone. Read the [acceptance record](ACCEPTANCE.md) before interpreting a passing build as platform support.

### Identity and connection modes

Provide `identity.pk8` and `certificate.p7b` in a private local directory and select it in the app. These are accessory authentication files, not Apple ID credentials. They are not distributed with this project. `rustcarplay auth-check --assets <directory>` validates local consistency, not iPhone trust. Keep credentials, pairings and local diagnostic/state directories out of commits and release archives.

- **LAN:** join the same Wi-Fi and pair Classic Bluetooth in the OS first. The app uses the current system Wi-Fi profile without an in-app password field. Its credentials are read for the iAP2 handshake only and are not saved in settings or logs.
- **Hotspot:** Windows Mobile Hotspot and Linux NetworkManager adapters are implemented; actual phone connection still needs acceptance. System configuration is reused unless customized in this mode.
- **USB:** no Wi-Fi or Bluetooth parameters. Select the phone after completing the [platform prerequisites](USB.md). Windows video, touch, sound, pause and reconnect passed initial user tests with one phone; other platforms and broader recovery scenarios remain unverified.

The player opens after the first decoded frame. `F11` toggles fullscreen; `Esc` first exits fullscreen, then returns home. Returning home keeps the connection active. Use `rustcarplay connect --help` for CLI options. `serve` is a low-level listener for an independent iAP2 bootstrapper, not a complete connection command.

### Continuing the port

Use [architecture](ARCHITECTURE.md) for crate responsibilities and [mobile porting](MOBILE_PORTING.md) for the proposed Android/HarmonyOS bridges. The upstream reference is DiPlay commit `9e244d958afe6b8fd79ade49769ce25a944f397b`; an optional checkout in `DiPlay/` is ignored and not part of release artifacts. Preserve source attribution and the [third-party notices](THIRD_PARTY_NOTICES.md) when contributing.
