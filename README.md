# RustCarPlay

**中文** · [English](README.en.md)

基于 [DiPlay](https://github.com/shihabal3amri/DiPlay) 的 Rust CarPlay 接收端。以共享 Rust 协议实现连接与媒体会话，通过平台原生适配器接入设备，桌面界面使用 **egui / wgpu**，媒体后端使用 **GStreamer**。

**v0.1.0 是早期开发预览。** 已在 Windows 与一台 iPhone 上打通无线和 USB；尚未复刻 DiPlay 的全部功能，也未完成长期稳定性与跨设备验收。

[下载 / Releases](https://github.com/Ezreal-byte/RustCarPlay/releases) · [开发与运行](docs/DEVELOPMENT.md) · [验收记录](docs/ACCEPTANCE.md) · [问题反馈](https://github.com/Ezreal-byte/RustCarPlay/issues)

## 界面预览

![RustCarPlay 首页与画面页截图占位](docs/screenshots/placeholder.svg)

*上图是明确标注的截图占位，不是真机截图。[截图补充指引](docs/screenshots/README.md)。*

## 平台状态

| 平台 | v0.1.0 状态 |
| --- | --- |
| Windows 11 x64 | 一台 iPhone 实测：无线/USB 画面、触控和声音；USB 暂停与重连初测正常 |
| Linux x64 / ARM64 | 原生 CI 构建目标；USB、BlueZ 与 NetworkManager 后端已有实现，尚未真机验收 |
| macOS Intel / Apple Silicon | GUI/核心构建预览；原生 RFCOMM、USB 及网络配置适配尚未完成，不能完成接收端连接 |
| Android | 共享核心移植方向明确，Kotlin/JNI 外壳及系统适配未实现 |
| HarmonyOS / NEXT | 规划 ArkTS + 原生桥接；系统接口、权限和真机能力尚未验证 |

构建目标不代表该平台已有成功发布或真机支持，实际产物以 Releases 和工作流结果为准。20 次连接循环、两小时运行、Siri/通话、音频设备切换和休眠恢复仍待验收。

## 已实现什么

- **三种连接配置：**局域网、电脑热点、USB。局域网使用系统当前 Wi-Fi 配置，无需在应用输入密码；热点实际启停与手机连接仍待验证。
- **共享协议：**iAP2、USBMUX/NCM、Lockdown/CarKit 集成、配件认证、AirPlay 配对/加密控制与媒体、发现与会话清理。
- **媒体与输入：**H.264/HEVC 软件解码，AAC/Opus/LPCM 输出，触控回传和曲目/导航元数据。麦克风回传已有代码及合成测试，真机功能待验收。
- **桌面体验：**连接/设置首页、首帧自动切入画面页、自定义顶栏、全屏、日夜切换和本地诊断。当前界面主要为中文。

协议集中在 `carplay-protocol`、`carplay-auth`、`carplay-wireless`、`carplay-receiver`；`carplay-platform` 负责系统边界，`carplay-media` 负责媒体，`carplay-app` 共享会话生命周期。详见[架构](docs/ARCHITECTURE.md)。硬解、完整第二屏、P2P、AEC、停车视频及车辆扩展尚未交付。

## 开始使用

1. 从 [Releases](https://github.com/Ezreal-byte/RustCarPlay/releases) 选择匹配系统与架构的包，并核对 `SHA256SUMS`。
2. 按[开发与运行](docs/DEVELOPMENT.md)准备 **GStreamer 运行时及编解码插件**。发布包不捆绑 GStreamer、USB 用户态 DLL 或驱动；随包脚本需要主动运行，不会自动申请管理员权限或安装驱动。
3. 在本地提供配件认证文件 `identity.pk8` 与 `certificate.p7b`。它们是配件私钥/证书，与 Apple ID 密码无关，**不包含在源码或发布包中**；本地自检不能代替 iPhone 实际认证。
4. 局域网模式：电脑与 iPhone 自行加入同一 Wi-Fi，在系统中完成蓝牙配对后连接。USB 模式：先阅读 [USB 前置条件](docs/USB.md)；Windows 还需 Apple Mobile Device Service、USB 用户态运行时和[可恢复的设备配置](docs/WINDOWS_USB_DRIVER.md)。

窗口程序为 `carplay-desktop`，命令行为 `rustcarplay`（Windows 带 `.exe`）。完整构建、启动、故障定位与恢复命令见[开发文档](docs/DEVELOPMENT.md)。

## 从源码开发

```sh
git clone https://github.com/Ezreal-byte/RustCarPlay.git
cd RustCarPlay
cargo test --workspace --locked
```

需要 Rust stable、对应平台编译工具和桌面系统库；启用媒体功能还需 GStreamer 开发文件。默认测试不等于 native 媒体或真机验收。开发者可从[架构](docs/ARCHITECTURE.md)、[验收缺口](docs/ACCEPTANCE.md)和 [Android / HarmonyOS 延续方案](docs/MOBILE_PORTING.md)选取工作项。

## 来源与许可证

固定参考为 DiPlay 提交 [`9e244d958afe6b8fd79ade49769ce25a944f397b`](https://github.com/shihabal3amri/DiPlay/tree/9e244d958afe6b8fd79ade49769ce25a944f397b)，其接收端源自 [xcertplay](https://github.com/shilapi/xcertplay)。本项目在 Rust 中移植协议与行为，保留源文件来源声明；不是对 Android APK 的桌面封装。

项目代码使用 [GPL-3.0-only](LICENSE)。外部运行时、上游素材及各依赖保留自己的许可证，详见[第三方声明](docs/THIRD_PARTY_NOTICES.md)。CarPlay 名称与原始图标属于 Apple，不适用本项目代码的 GPL 许可；本项目不表示 Apple 认证或授权。
