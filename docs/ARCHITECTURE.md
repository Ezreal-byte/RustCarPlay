# 架构与移植边界

固定参考：DiPlay `9e244d958afe6b8fd79ade49769ce25a944f397b`。所有端口、密钥和媒体任务由当前连接拥有；停止连接需停止发现、关闭 transport、销毁媒体管线并 join 工作线程。UI 不拥有协议实现。

| Crate | 职责 |
| --- | --- |
| carplay-core | 设置、输入映射、车辆停车门控、会话代次、RTSP/media framing、媒体接口 |
| carplay-protocol | iAP2 link/control TLV、身份识别、USBMUX/NCM、有界元数据解析 |
| carplay-auth | MFi 身份和挑战签名、MFiSAP、SRP、Pair Verify、控制通道 AEAD |
| carplay-platform | Windows/Linux RFCOMM、当前 Wi-Fi 凭据、热点 actor、USB 模式/配置检测、系统 Lockdown/CarKit/NCM；可选 Windows 控制过滤驱动接口 |
| carplay-wireless | 无线/有线 iAP2 身份、认证、订阅与 0x4301 启动会话；有线不发 Wi-Fi 配置 |
| carplay-discovery | 发现并选择 iPhone 的控制服务、HTTP connect probe、取消与限次重试 |
| carplay-receiver | AirPlay RTSP 服务、配对持久化、加密事件/媒体/iAP 隧道、流生命周期 |
| carplay-media | 可选 GStreamer 解码与播放，有限队列，RGBA 最新帧 |
| carplay-app | CLI/GUI 共享启动顺序、事件汇聚、当前解码帧证明、蓝牙退避重连 |
| carplay-cli / carplay-desktop | 本地诊断/运行命令；egui/wgpu 窗口与触控 |

协议路径：系统蓝牙配对 → RFCOMM → iAP2 link/identification/MFi → Wi-Fi 参数/启动会话 → mDNS phone connect probe → AirPlay Pair Setup/Verify → 加密控制/事件/媒体 → 解码与触控回传。手机主动连接与 connect probe 可同时发生，因此发现成功、MFi 成功、控制连接、解码首帧是不同的观察事件。

`SOURCE_VERSION` 统一为上游运行配置 `950.7.1`。无线与运行时 type 130 隧道共用同一身份信息及真实网络配置。不以已发送 0x4301 或缓存中曾经存在一张图像判断连接成功。

数据与资源约束：RTSP header/body、iAP2 framing/TLV、证书、metadata、配对文件和媒体输入均有上限；无效认证标签、重复/截断控制数据和未知媒体格式返回错误。不同会话不共享控制加密计数器。密码用 Zeroizing 存放，私钥不提供 Debug；元数据事件的 Debug 只输出类型或消息 ID/长度。

Windows 配对数据使用当前用户 DPAPI，Unix 使用 0600；认证提供者为可替换接口，不强制把材料编译进二进制。当前只支持单个活动 iPhone。车辆能力以真实数据可用性为依据：未知/过期/未来时间戳的档位不能允许停车视频。

平台 native 代码位于边界 crate，核心不直接依赖 JNI/Android Framework。移动端未来需要稳定的 FFI 会话句柄、主线程事件投递、权限和音频生命周期适配；当前没有以空壳 FFI 宣称移动端完成。


`TransportOptions` 将 LAN（系统当前 Wi-Fi）、Hotspot（系统热点配置或专用自定义配置）和 USB（选定物理设备）分开，不共享无关参数。热点由专属线程持有系统对象；UI 接收可跨线程的控制句柄，取消和失败需要等待清理。Windows 的 WinRT apartment 覆盖 actor 生命周期，启动/恢复错误保留给界面。

USB 路径为选定物理设备 → 模式/配置 → 系统 USBMUX → Lockdown/TLS → CarKit iAP2 → 该手机 NCM 接口的 IPv6 AirPlay。Linux 使用 usbmuxd/libimobiledevice 与 `cdc_ncm`；Windows 使用 Apple Mobile Device Service、本地 libimobiledevice DLL 和系统 UsbNcm。两端都按所选 USB 序列号精确匹配系统 USBMUX 设备，不回退到网络配对设备。USB 的 endpoint 没有 Wi-Fi 凭据，所有派生媒体、事件、计时与麦克风 socket 保留 IPv6 scope；有线认证后以保守供电声明发送 `0xAE03`。

Windows 的模式请求优先尝试现有 WinUSB 接口；接口不可访问时，单独准备的 libusb-win32 父设备过滤驱动仅承担控制请求，USBMUX bulk 仍由 Apple 服务拥有。nusb 的描述符层接受已激活的非首配置；选择该配置由 usbccgp 和管理员准备工具负责，不能向 WinUSB 子接口发送 `SET_CONFIGURATION` 假装完成。准备工具按实例保存复合配置、CDC 枚举与过滤器原值，运行时不自行安装驱动。

准备流程在切换成功或失败后执行 `Disarm`，恢复配置选择注册表原值，以免下次插线留下初始 USB 模式不支持的索引；当前已激活的配置和该实例的 CDC/过滤器设置仍保留，完整恢复另走 `Restore`。本机 PnP restart 曾返回 3010/50，实际通过手动重插和 `-ResumeAfterReplug` 继续准备；续跑要求同一手机的近期描述符及初始 USB 模式。本轮复用原有 `libusb0.sys`，没有全局升级驱动。

Windows NCM 发现同时验证精确 PnP 父链、NCM 控制接口号、UsbNcm 驱动和本地 IPv6 可绑定性。iPhone 在 `StartCarPlaySession` 前可能不给 NCM carrier，因此不能把 carrier 已连通作为发送 iAP2 启动消息的前置条件；也不能用其他网卡地址补位。若系统未分配本地 link-local 地址，连接会报告这一实际缺口。

2026-10-09，这条 Windows USB 路径已在一台 iPhone 上实际完成 iAP2 认证、AirPlay 验证、NCM 视频接收及桌面 1280×720 可见画面，用户确认点击和返回正常；音频启动修复后，声音、暂停继续和软件重连也经用户确认正常。此次观测为配置 6、UsbNcm MI03、IPv6 scope 63，运行时仍从所选设备描述符和系统网卡读取，不能将这些值写死。完整输入验收、稳定性与 Linux 真机仍待验证，详见 [验收记录](ACCEPTANCE.md)。

音频在 SETUP 阶段以 `MediaEvent::AudioConfig` 完成准备：调用线程打开本机播放设备，再将管线移交给媒体线程并等待确认，之后才向 iPhone 返回 UDP 端口。设备慢启动不会阻塞已有媒体线程，首包不再触发播放设备初始化。数据队列仍有界，准备失败和断开均释放本次流的资源。

macOS 构建启用 Metal 并复用协议、媒体和桌面代码；v0.1.0 尚无 macOS RFCOMM、USB 或网络配置适配。桌面明确显示构建预览且禁用连接入口，底层平台操作返回不支持；构建通过不代表原生连接已完成。Android/HarmonyOS 的后续边界见 [移动端移植](MOBILE_PORTING.md)。

CarKit 流保留创建服务的 Lockdown 会话，按依赖顺序关闭；原生调用只在工作线程运行。libimobiledevice 1.3.x 的 TLS `receive_timeout` 会按请求长度读满，超时可能丢失部分返回，因此适配层在 TLS 模式使用单字节有界读取，保持 Rust `Read` 的短读语义。视频和音频走 NCM socket，不经过这条低带宽控制流。Windows DLL 由绝对目录加载，新增依赖搜索限制到该目录与 System32，不从当前目录或 PATH 寻找替代 DLL。
