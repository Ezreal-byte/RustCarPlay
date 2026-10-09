# Android 与 HarmonyOS 移植 / Mobile porting

[中文](#中文方案) · [English](#english-plan) · [架构 / Architecture](ARCHITECTURE.md)

## 中文方案

**v0.1.0 尚无 Android APK、HarmonyOS HAP、JNI/N-API 桥或移动端真机验收。** 以下是后续开发方案，不是已经提供的接口。Rust 可以复用协议代码；设备访问、媒体、权限与应用生命周期需要平台实现。

### 先分离可复用边界

| 现有代码 | 移动端延续方式 |
| --- | --- |
| `carplay-core` / `carplay-protocol` | 复用设置、媒体格式、输入映射及有界协议解析；先做目标平台编译和测试向量验证 |
| `carplay-auth` | 复用认证算法与 `AuthProvider` / `PairingStore` 接口；身份材料由应用本地提供，持久化接入平台私有存储 |
| `carplay-wireless` / `carplay-receiver` | 复用 iAP2、AirPlay、加密、元数据和会话逻辑；替换系统传输与网络发现边界 |
| `carplay-media` | 当前为桌面 GStreamer 实现；以 `MediaSink`、`CaptureFactory` 接入移动端解码/播放/采集，并保留配置确认、有限队列及错误传播语义 |
| `carplay-app` | 当前直接创建 GStreamer 和桌面平台资源；先改为注入媒体、认证、存储与传输后端，再供外壳调用 |
| `carplay-desktop` | egui 桌面外壳不直接作为手机 UI；Android 与 HarmonyOS 各自建立原生界面 |

建议新增独立桥接 crate 和原生示例工程；名称可在实际实现时确定。公共 C ABI 只暴露版本化配置、不可伪造的会话句柄、带长度的字节缓冲和有界事件队列。设计 `create/start/poll_event/send_input/stop/destroy` 生命周期，明确每块内存由谁释放；这些函数目前不存在。禁止 Rust panic 跨越 ABI，错误返回可分类状态，回调经各平台主线程分发。取消、断线、后台切换和重复销毁都要有回归测试，不能复用旧会话的帧、计数器或密钥。

### Android：Kotlin 外壳 + JNI

1. 固定 Android SDK/NDK 和最低 API 级别，先以 `arm64-v8a` / Rust `aarch64-linux-android` 为手机目标，`x86_64` 仅作适配/模拟器测试目标。先编译协议与认证 crate，再构建 `cdylib` 和最小 Kotlin/JNI 往返调用。ABI 和 JNI 线程/引用管理按 [Android NDK ABI](https://developer.android.com/ndk/guides/abis) 与 [JNI 指引](https://developer.android.com/ndk/guides/jni-tips)核对。
2. 在 Kotlin 层实现系统配对与经典蓝牙 RFCOMM、当前网络选择、权限请求和前后台生命周期；通过显式能力查询向 Rust 提供结果。移动端不能直接照搬 Windows 的“读取系统 Wi-Fi 密码”路径，局域网/热点引导需要先验证普通应用实际可用的接口。
3. 实现 MediaCodec/原生显示表面的视频适配与音频播放/采集后端，Rust 保持媒体时序、输入坐标及加密。先验证 H.264 720p/30fps、音频和触摸，再加入 HEVC、多路音频及麦克风。音频后端必须在 SETUP 确认前准备完成，避免重现桌面初始音频包阻塞队列的问题。
4. USB 单独实现 Android USB Host 的权限、模式切换与重新枚举、接口管理、USBMUX/Lockdown/CarKit 和 NCM 网络路径。不能假设普通 Android 设备上有系统 usbmuxd、libimobiledevice 或直接可绑定的 NCM 网卡。对照固定 DiPlay 源码的 USB/NCM 实现，同时保留其来源声明。
5. 最后接入音频焦点、设备切换、后台双向音频和中断恢复；BYD/车辆能力使用独立适配器，缺少真实车辆数据时不宣告支持。

第一个可交付里程碑是：原生外壳启动 Rust 会话，完成真实认证，显示实时画面、播放声音、回传触摸，并在断开后释放所有资源。仅在模拟器加载 `.so` 不算达到这一目标。

### HarmonyOS / NEXT：先验证能力，再做完整桥接

Rust 官方提供 `aarch64-unknown-linux-ohos` 等 OpenHarmony 目标，并要求配置对应 SDK 工具链。这证明存在编译路径，不证明某一款 HarmonyOS 设备的普通应用具备 CarPlay 所需权限。[Rust 平台说明](https://doc.rust-lang.org/rustc/platform-support/openharmony.html)。

先建立独立 ArkTS + 薄 C/C++ Native API 模块，再经 C ABI 调用 Rust；OpenHarmony 的 [Node-API 工程](https://github.com/openharmony/arkui_napi)与[跨语言调用流程](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/napi/use-napi-process.md)可用于核对桥接方式。消费级 HarmonyOS 使用目标设备对应的华为 SDK 与签名流程，不能把 OpenHarmony 文档直接当作所有商业设备的能力承诺。

在普通应用权限下分别完成以下小实验，记录系统/API 版本、允许的权限、实际结果和失败原因：

- 经典蓝牙配对、SDP/RFCOMM，以及连接中进入后台后的存活情况。
- USB Host 控制请求、手机重新枚举、复合接口访问和 NCM；拒绝权限后的恢复。
- 选定网络上的 IPv6 link-local/scope、mDNS、TCP/UDP 监听与多接口路由。
- H.264/HEVC 解码与显示表面、低延迟播放/采集、后台双向音频和音频焦点。
- 应用私有身份/配对存储、输入回传、资源回收和进程重启。

只将已通过的能力接入桥接层。若 RFCOMM 或 USB/NCM 需要普通应用无法获得的权限，应明确列出受限模式；不要用“Rust 目标编译通过”宣布完整 CarPlay 可用，也不要把兼容运行 Android APK 当成原生 NEXT 支持。

### 贡献与验收顺序

按“可注入会话边界 → ABI/取消测试 → Android 无线闭环 → Android USB → HarmonyOS 能力实验/可用模式 → 车辆扩展”拆分独立变更。每个后端保留桌面协议测试，增加真实设备断线、权限拒绝、后台、暂停、重连和音频抢占记录；随后按[验收要求](ACCEPTANCE.md)完成 20 次循环与两小时运行。

不把私钥、证书、配对文件、原始抓包、个人地址或通话内容加入提交/示例/发布包。协议移植继续注明 DiPlay `9e244d958afe6b8fd79ade49769ce25a944f397b` 与对应源文件，遵循项目 GPL 及各平台依赖的独立许可证。

## English plan

**v0.1.0 has no Android APK, HarmonyOS HAP, JNI/N-API bridge or mobile device acceptance.** This is a continuation plan, not an existing API. Rust protocol reuse still requires native transport, media, permission and lifecycle adapters.

### Reuse and refactor

Reuse `carplay-core` and `carplay-protocol` for configuration, media formats, input and bounded parsers. Reuse `carplay-auth` algorithms and provider/store interfaces with application-private identity storage. Reuse iAP2/AirPlay session logic from `carplay-wireless` and `carplay-receiver`, replacing platform transports and discovery as needed.

Implement `MediaSink` and `CaptureFactory` for native mobile playback/capture. Keep acknowledged configuration, bounded buffering and error propagation. `carplay-app` currently constructs GStreamer and desktop resources directly; refactor it to accept media, authentication, storage and transport backends before exposing it to mobile shells.

Add a separate bridge crate and platform sample apps. A proposed versioned C ABI should use opaque session handles, length-delimited buffers and a bounded event queue, with explicit create/start/poll/input/stop/destroy ownership. Those exports do not exist yet. Define allocator ownership, catch panics at the boundary, return structured errors and dispatch UI callbacks through the platform thread. Test cancellation, background transitions and repeated cleanup; never carry old frames, keys or counters into a new session.

### Android: Kotlin and JNI

1. Pin the SDK, NDK and minimum API level. Start with `arm64-v8a` / `aarch64-linux-android`; use x86_64 for emulator/adapter tests. Build protocol/authentication crates before a `cdylib` and a minimal Kotlin/JNI round trip. Follow the official [ABI](https://developer.android.com/ndk/guides/abis) and [JNI](https://developer.android.com/ndk/guides/jni-tips) guidance.
2. Implement Classic Bluetooth pairing/RFCOMM, network selection, permissions and application lifecycle in the native shell. Report actual capabilities to Rust. Desktop access to saved Wi-Fi credentials cannot be assumed available to an ordinary mobile app; validate LAN/hotspot bootstrap separately.
3. Add MediaCodec/display-surface and native audio adapters. Prove H.264 720p/30fps, sound and touch first, followed by HEVC, multiple audio streams and microphone return. Complete audio initialization before acknowledging SETUP, without blocking existing media streams.
4. Treat USB as a separate backend: USB Host permission, mode switching/re-enumeration, interface ownership, USBMUX/Lockdown/CarKit and NCM networking. Do not assume Android provides system usbmuxd, libimobiledevice or a directly usable NCM interface. Consult the pinned DiPlay implementation and preserve source attribution.
5. Add audio focus, device changes, background capture/playback and interruption recovery. Keep BYD/vehicle integration separate and report only capabilities backed by real vehicle data.

The first device milestone is a native shell starting a Rust session that authenticates, shows live video, plays sound, returns touch and releases its resources on disconnect. Loading a shared library in an emulator is only an earlier build milestone.

### HarmonyOS / NEXT: prove access first

Rust provides OpenHarmony targets including `aarch64-unknown-linux-ohos`, with SDK toolchain configuration described in the [Rust platform guide](https://doc.rust-lang.org/rustc/platform-support/openharmony.html). Target availability does not establish permissions on a commercial HarmonyOS device.

Use an ArkTS shell and a thin C/C++ Native API module calling the Rust C ABI. Consult the [OpenHarmony Node-API project](https://github.com/openharmony/arkui_napi) and [interop workflow](https://github.com/openharmony/docs/blob/master/zh-cn/application-dev/napi/use-napi-process.md); use Huawei's SDK/signing process for the actual target device.

Before promising a full receiver, run ordinary-app capability experiments for Classic Bluetooth SDP/RFCOMM; USB control, re-enumeration and NCM; scoped IPv6, mDNS and sockets; hardware video decode/display; background duplex audio and audio focus; private identity storage and lifecycle cleanup. Record OS/API versions, requested permissions, results and denial/recovery behavior. Integrate only proven capabilities and explicitly limit unavailable modes. Android APK compatibility is not native NEXT support.

### Delivery and acceptance

Split work into injectable session boundaries, ABI/cancellation tests, Android wireless, Android USB, HarmonyOS capability probes/supported modes, and vehicle extensions. Retain desktop protocol tests and collect mobile evidence for permission denial, disconnect, backgrounding, pause, reconnect and audio interruptions. Then complete the [20-cycle and two-hour acceptance requirements](ACCEPTANCE.md).

Keep private keys, certificates, pairings, raw captures and personal data out of commits, samples and release archives. Retain attribution to DiPlay `9e244d958afe6b8fd79ade49769ce25a944f397b` and individual source files, and preserve GPL and dependency license obligations.
