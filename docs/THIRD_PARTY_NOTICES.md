# 来源与第三方声明

本工程基于 [DiPlay](https://github.com/shihabal3amri/DiPlay) 提交 `9e244d958afe6b8fd79ade49769ce25a944f397b`。DiPlay 接收端源自 [xcertplay by shilapi](https://github.com/shilapi/xcertplay)，GNU GPL v3；本工程保留 GPL-3.0-only，完整文本见根目录 LICENSE。协议研究的上游鸣谢还包括 [LIVI](https://github.com/f-io/LIVI) 与 [Showcase](https://github.com/amineross/showcase)。

Rust 源文件及各 crate README 标注对应 Kotlin 来源，主要包括 `airplay/` 的认证、控制、媒体、输入模块，`transport/` 的 iAP2/USBMUX/NCM 模块，`network/CarPlayBonjour.kt` 及 `hud/` 元数据解析。固定上游副本 `DiPlay/` 未被本次实现修改。

桌面 UI 为新实现，没有移植 DiAuto 的 AGPL UI、下载网站或 BYDMate 的非商业 HUD 图片。

桌面使用的 `apps/carplay-desktop/assets/carplay.png` 原样复制自固定版本 DiPlay 的 `common/src/main/res/drawable/ic_carplay.png`，上游注明其来自 [Apple 开发者网站](https://developer.apple.com/assets/elements/icons/carplay/carplay-96x96_2x.png)。CarPlay 名称及图标属于 Apple Inc. 的商标/素材，图标不适用本工程的 GPL 源代码许可；使用该素材不表示 Apple 批准或认证。

## v0.1.1 实验认证材料 / Experimental authentication material

v0.1.1 二进制预览包按本次选择携带 `resources/auth/identity.pk8`、`certificate.p7b` 和不含秘密的 `provenance.json`。前两个文件不属于本项目源代码，不进入 Git 或 Rust 源码包。来源为 [DiPlay 官方 v0.2.15 预览 APK](https://github.com/shihabal3amri/DiPlay/releases/tag/v0.2.15) 的 `assets/offline-mfi/`；[上游声明](https://github.com/shihabal3amri/DiPlay/blob/9e244d958afe6b8fd79ade49769ce25a944f397b/docs/THIRD_PARTY_NOTICES.md) 将身份归于 **Carlinkit C2Air / Allwinner V821 公开固件**。

这不是新签发的身份。上游标明这些数据为实验用途，其持续有效性与分发适用性尚未确定；未来 iOS 是否继续接受没有保证。公开可下载、能够提取或本次选择预览分发，都不等于权利人授予再分发许可，也不能把这些材料纳入 GPL 源代码许可。Rust 自检只确认本地密钥匹配，不确认证书链或 iPhone 信任。

准备脚本固定 APK 的 SHA-256 为 `4bf45f16d6b1ab0a61462b831014081f07240f5596c90ca6bf38fb43f9890511`。来源记录保存 APK 摘要、版本、下载位置、固件来源和实验说明；它不包含私钥字节或私钥摘要。发布包不包含开发者或其他用户的个人配对、网络凭据及 `.local/` 状态。

The v0.1.1 binary preview includes the fixed accessory identity from DiPlay's v0.2.15 preview APK, attributed upstream to public Carlinkit C2Air / Allwinner V821 firmware. This is not a newly issued identity. Public availability, extraction and the choice to distribute this preview do not establish permission from the rights holder. Redistribution suitability and future iOS acceptance remain unresolved; the source-code GPL does not license this material. Git and Rust source archives exclude the private key. The non-secret `provenance.json` records the upstream artifact and limitations, never a private-key digest. A local consistency check does not prove iPhone trust.

## 运行时与对应源码 / Runtimes and corresponding source

Rust 依赖及精确版本见 Cargo.lock。GStreamer 与各原生依赖保留各自的 LGPL/GPL/其他许可，不因随包分发改为本项目许可。v0.1.1 二进制包包含 GStreamer 库、插件、必要的可分发用户态依赖及其许可材料；Windows 包另含 USB 用户态 DLL。运行时通过动态库加载，根启动器仅为子进程配置包内路径。SpeexDSP 尚未集成本工程，未来复用时需保留其 BSD 许可和组件声明。

v0.1.1 的发布资产约定包含以下源码附件；本轮 CI 与打包验证尚未完成，实际发布以完整资产及校验和为准：

| 附件 | 内容 |
| --- | --- |
| `RustCarPlay-0.1.1-source.tar.gz` | 项目源码、构建与准备脚本、Cargo.lock，以及 `cargo vendor --locked --versioned-dirs` 保存的 Rust 依赖源码/许可证；不含认证私钥 |
| `cerbero-1.28.7.tar.xz` | 共享的 GStreamer 官方对应源码归档 |
| `RustCarPlay-0.1.1-native-source-windows-x86_64.tar.gz` | Windows 包其余原生依赖的对应源码、许可证和构建来源记录 |
| `RustCarPlay-0.1.1-native-source-linux-x86_64.tar.gz` | Linux x64 包所需的发行版原生依赖对应源码及来源记录 |
| `RustCarPlay-0.1.1-native-source-linux-aarch64.tar.gz` | Linux ARM64 包所需的发行版原生依赖对应源码及来源记录 |

二进制包的 `runtime/NATIVE-MANIFEST.json`、`runtime/licenses/` 应与同一 Release 的源码附件配套阅读；仅有包名、下载链接或许可元数据不能代替适用许可证要求的对应源码。Rust 源码归档提供相对路径的离线 Cargo vendor 配置；重建 native 部分仍需对应平台的编译环境和所附原生源码/构建说明。Linux 的 GStreamer 与依赖来自 Ubuntu，其对应源码包含在各 Linux native-source 附件；共享 Cerbero 归档用于 Windows/macOS 官方构建。Linux 包以 Ubuntu 24.04 / glibc 2.39 为基线，系统桌面、图形/音频驱动、BlueZ、NetworkManager 和 usbmuxd 服务不随应用安装。

Windows 媒体运行时所需的 Microsoft Visual C++ runtime 保留 Microsoft 的专有再分发条款，相关材料放在 `runtime/licenses/microsoft/`；它不是开源组件，不包含在 GPL 对应源码承诺中。Apple 软件、系统驱动和 Windows UsbNcm 不随包分发。

v0.1.1 bundles native media libraries/plugins and selected user-space dependencies. Each component retains its upstream license. The matching Rust source asset contains vendored Rust sources/licenses and build scripts, without authentication private keys. The shared `cerbero-1.28.7.tar.xz` and three platform-specific native-source assets provide the native source material listed above. Read the binary package's provenance/license inventory together with those source assets. Package names or license metadata alone do not replace corresponding-source requirements. Validation of the new package set is still in progress.

## v0.1.0 历史分发方式 / Previous distribution

v0.1.0 的二进制发布包不捆绑上述原生运行时或驱动。每个二进制包附带 `DEPENDENCIES-SOURCE.txt`，指向同一 Release 的 `RustCarPlay-0.1.0-source.tar.gz`：其中包含该版本工程源码、Cargo.lock、构建脚本及 `cargo vendor --locked --versioned-dirs` 保存的全部 Rust 依赖源码与上游许可证。`DEPENDENCIES.json` 列出依赖版本和许可元数据；使用包内相对路径 Cargo 配置可在已安装原生 SDK 的环境中离线重建。此安排针对随程序编译的 Rust 依赖，不把另行安装的 Apple/GStreamer/USB 组件改为本项目许可。

The older v0.1.0 binary archives do not bundle native runtimes or drivers. Their matching source asset includes this project, build scripts, Cargo.lock, and all vendored Rust dependency sources and licenses. See `DEPENDENCIES-SOURCE.txt` and `DEPENDENCIES.json` in those assets. This historical arrangement does not describe the v0.1.1 offline packages.

## USB 与系统组件 / USB and system components

Linux USB 后端动态调用 [libimobiledevice](https://github.com/libimobiledevice/libimobiledevice)（LGPL-2.1-or-later）并连接系统 usbmuxd 服务；离线包中的用户态库不替代系统服务。Windows 后端也动态调用 libimobiledevice，由用户已安装的 Apple Mobile Device Service 提供 USBMUX；不复制 Apple 服务、Apple 驱动或 Windows UsbNcm。热点控制使用系统 Windows WinRT 或 NetworkManager/nmcli，不复制这些系统组件。

Windows 用户态 USB 运行时由 `scripts/setup-usb-runtime.ps1` 从 [MSYS2 官方 UCRT64 仓库](https://packages.msys2.org/packages/mingw-w64-ucrt-x86_64-libimobiledevice) 准备。准确版本、完整依赖闭包、发布 SHA-256 和 MSYS2 源码包链接在 `scripts/usb-runtime-packages.json`；开发时提取到被 Git 忽略的 `.local/usb-runtime/`，v0.1.1 打包到 `runtime/usb/`。主要组件包括 libimobiledevice、libplist、libusbmuxd、libimobiledevice-glue（LGPL 系列；部分工具为 GPL）、OpenSSL（Apache-2.0）、GCC runtime（GPL 加 GCC Runtime Library Exception，libquadmath 为 LGPL）、winpthreads（MIT/BSD）和 tzdata（公有领域）。脚本保留软件包 `.PKGINFO`、所含许可材料及标准 GNU 许可文本到 `licenses/`，不运行包安装脚本。对应源码随本轮 Windows native-source 附件提供；系统驱动准备保持独立，不由根启动器自动执行。

Windows 可选 USB 控制过滤器来自 [libusb-win32 1.4.0.2](https://github.com/mcuee/libusb-win32/releases/tag/release_1.4.0.2)。该发布包的 `installer_license.txt` 将内核驱动标为 GPL，将用户态库、测试文件和安装器标为 LGPL；包内 `COPYING_GPL.txt`、`COPYING_LGPL.txt` 分别为 GPL v3 和 LGPL v3，完整材料保留在下载包。准备工具校验固定包摘要、Microsoft WHCP 目录签名和内核目录成员；本项目未修改或重新签名其内核二进制。Windows USB 复合配置研究参考 [0xbaksa 的反向 USB 网络实验](https://github.com/0xbaksa/iphone-usb-reverse-tethering-windows)（MIT）和 Microsoft 官方文档；其 mode 3 网络结果不能视为本项目 mode 4 CarPlay 已验收。
