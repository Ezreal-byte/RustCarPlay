# 来源与第三方声明

本工程基于 [DiPlay](https://github.com/shihabal3amri/DiPlay) 提交 `9e244d958afe6b8fd79ade49769ce25a944f397b`。DiPlay 接收端源自 [xcertplay by shilapi](https://github.com/shilapi/xcertplay)，GNU GPL v3；本工程保留 GPL-3.0-only，完整文本见根目录 LICENSE。协议研究的上游鸣谢还包括 [LIVI](https://github.com/f-io/LIVI) 与 [Showcase](https://github.com/amineross/showcase)。

Rust 源文件及各 crate README 标注对应 Kotlin 来源，主要包括 `airplay/` 的认证、控制、媒体、输入模块，`transport/` 的 iAP2/USBMUX/NCM 模块，`network/CarPlayBonjour.kt` 及 `hud/` 元数据解析。固定上游副本 `DiPlay/` 未被本次实现修改。

桌面 UI 为新实现，没有移植 DiAuto 的 AGPL UI、下载网站或 BYDMate 的非商业 HUD 图片。

桌面使用的 `apps/carplay-desktop/assets/carplay.png` 原样复制自固定版本 DiPlay 的 `common/src/main/res/drawable/ic_carplay.png`，上游注明其来自 [Apple 开发者网站](https://developer.apple.com/assets/elements/icons/carplay/carplay-96x96_2x.png)。CarPlay 名称及图标属于 Apple Inc. 的商标/素材，图标不适用本工程的 GPL 源代码许可；使用该素材不表示 Apple 批准或认证。

本地实验的配件私钥/证书不属于项目源代码，不随 Git、程序或文档发布。上游 THIRD_PARTY_NOTICES 将公开预览 APK 中的认证数据标为来自公开固件的实验数据，并说明其持续有效性与分发适用性尚未确定。Rust 自检只确认密钥匹配，不确认证书链或 iPhone 信任。

Rust 依赖及精确版本见 Cargo.lock。GStreamer 本地运行环境由官方发行文件组成，具有各组件自身的 LGPL/GPL/其他许可；打包分发 native DLL 和插件时应随附对应源码获取方式与许可材料。当前准备脚本用于本地构建，尚不是完成许可清单的发布打包器。SpeexDSP 尚未集成本工程，未来复用时需保留其 BSD 许可和组件声明。

v0.1.0 的二进制发布包不捆绑上述原生运行时或驱动。每个二进制包附带 `DEPENDENCIES-SOURCE.txt`，指向同一 Release 的 `RustCarPlay-0.1.0-source.tar.gz`：其中包含该版本工程源码、Cargo.lock、构建脚本及 `cargo vendor --locked --versioned-dirs` 保存的全部 Rust 依赖源码与上游许可证。`DEPENDENCIES.json` 列出依赖版本和许可元数据；使用包内相对路径 Cargo 配置可在已安装原生 SDK 的环境中离线重建。此安排针对随程序编译的 Rust 依赖，不把另行安装的 Apple/GStreamer/USB 组件改为本项目许可。

Release binary archives do not bundle native runtimes or drivers. The matching source asset includes this project, build scripts, Cargo.lock, and all vendored Rust dependency sources and licenses. See `DEPENDENCIES-SOURCE.txt` and `DEPENDENCIES.json` in those assets. Separately installed native components retain their upstream licenses.

Linux USB 后端动态调用发行版安装的 [libimobiledevice](https://github.com/libimobiledevice/libimobiledevice)（LGPL-2.1-or-later）和系统 usbmuxd 服务。Windows 后端也动态调用 libimobiledevice，由用户已安装的 Apple Mobile Device Service 提供 USBMUX；不复制 Apple 服务、Apple 驱动或 Windows UsbNcm。热点控制使用系统 Windows WinRT 或 NetworkManager/nmcli，不复制这些系统组件。

Windows 本地运行时由 `scripts/setup-usb-runtime.ps1` 从 [MSYS2 官方 UCRT64 仓库](https://packages.msys2.org/packages/mingw-w64-ucrt-x86_64-libimobiledevice) 准备。准确版本、完整依赖闭包、发布 SHA-256 和 MSYS2 源码包链接在 `scripts/usb-runtime-packages.json`；提取文件位于被 Git 忽略的 `.local/usb-runtime/`。主要组件包括 libimobiledevice、libplist、libusbmuxd、libimobiledevice-glue（LGPL 系列；部分工具为 GPL）、OpenSSL（Apache-2.0）、GCC runtime（GPL 加 GCC Runtime Library Exception，libquadmath 为 LGPL）、winpthreads（MIT/BSD）和 tzdata（公有领域）。脚本保留软件包 `.PKGINFO`、所含许可材料及标准 GNU 许可文本到 `licenses/`，不运行包安装脚本。此为本地开发准备工具；对外分发这些 DLL 时仍需提供对应许可证、版权声明及适用的完整对应源码/获取安排，不能仅以运行时清单替代发布义务。

Windows 可选 USB 控制过滤器来自 [libusb-win32 1.4.0.2](https://github.com/mcuee/libusb-win32/releases/tag/release_1.4.0.2)。该发布包的 `installer_license.txt` 将内核驱动标为 GPL，将用户态库、测试文件和安装器标为 LGPL；包内 `COPYING_GPL.txt`、`COPYING_LGPL.txt` 分别为 GPL v3 和 LGPL v3，完整材料保留在下载包。准备工具校验固定包摘要、Microsoft WHCP 目录签名和内核目录成员；本项目未修改或重新签名其内核二进制。Windows USB 复合配置研究参考 [0xbaksa 的反向 USB 网络实验](https://github.com/0xbaksa/iphone-usb-reverse-tethering-windows)（MIT）和 Microsoft 官方文档；其 mode 3 网络结果不能视为本项目 mode 4 CarPlay 已验收。
