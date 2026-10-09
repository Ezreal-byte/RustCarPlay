# 验收与缺口

以下状态按实际证据记录，未勾选的内容仍属于原实施方案。测试成功只能证明所覆盖场景，不能把软件单测替代真实手机或操作系统接口验收。

| 目标 | 当前证据 / 尚需完成 |
| --- | --- |
| 认证 | Rust 算法向量、独立 SRP 对照、完整本地 Pair Setup/Verify 测试通过；用户现有 iPhone 已在 Windows 无线与 USB 路径通过认证及配对并显示画面，其他设备/版本未验证 |
| iAP2 | 编解码、ACK/重传、握手取消/超时、认证/身份/订阅/Wi-Fi、type130 真 TCP 测试；同一台 iPhone 的 Windows 无线与有线认证/会话启动已实测，其他设备及完整异常恢复待验证 |
| 局域网配置 | 当前系统WiFi接口/profile自动读取，无密码输入框；本机普通用户权限安全读取已验证，凭据只在内存用于上游一致的iAP2握手；不读其他保存网络，权限失败明确提示 |
| 电脑热点 | Windows WinRT与Linux NetworkManager后端、专属actor生命周期、取消/恢复/自有热点清理已实现；本机能力查询通过且热点原为关闭，实际启停和iPhone热点连接待实测 |
| Windows 无线 | 原生RFCOMM引导→认证→AirPlay配对→视频已在用户iPhone和Windows实际打通；补齐与DiPlay一致的Start/Stop声明后不再遇到首次Identification拒绝。20次连接循环待实测 |
| Windows USB | 一台 iPhone 已完成 USBMUX／Lockdown／CarKit、iAP2 认证和 AirPlay 验证，经原生 UsbNcm 收到首帧；桌面实际显示 1280×720 CarPlay 主页并自动进入画面页，用户确认点击和返回正常。音频启动修复后，用户确认声音、暂停和重连正常。复用原过滤驱动，手动重插后续跑准备已成功；完整输入、20 次循环、两小时运行、休眠及异常拔插恢复仍待验收 |
| Linux | 核心及 CLI 的 Linux target 编译检查；新增usbmuxd/libimobiledevice Lockdown/CarKit→NCM IPv6→有线iAP2/AirPlay代码；Ubuntu native连接、carrier时序、声音/显示与USB拔插仍待实测 |
| 视频 | 合成 H264/H265 真实解码验证；用户已确认无线 iPhone 画面正常显示，Windows USB 也已实际观察到 1280×720 可见画面。动态分辨率、延迟、其他设备待实测；硬解未交付 |
| 音频 | 合成 AAC/Opus 解码及 LPCM 大端转码测试、解密 UDP localhost 验证通过；用户确认 Windows 无线声音正常，USB 音频启动修复后声音、暂停和重连正常。时钟偏移估计、重传、优先级/ducking、设备切换、两小时稳定性仍待实现或验证 |
| Siri/通话 | LPCM/Opus麦克风与加密回传已实现，合成采集→UDP独立解密测试通过；真实Siri/通话/AEC及抢占恢复尚未验收 |
| 输入 | 已修复抬起被提前发送到(0,0)造成返回/打开App不响应的问题；快速完整点击、拖动合并和失焦释放回归通过。2026-10-09 用户确认无线修复后的功能初测正常，随后明确确认 USB 点击和返回正常；旋转/分辨率变化等完整输入验收仍待完成，系统媒体键未完成 |
| 导航/第二屏 | 元数据 parser 和第二屏协议声明；完整地图第二窗口/转向卡、车辆仪表尚未完成 |
| 停车视频 | 可信新鲜 Park 状态门控已有测试；HLS/视频播放器和车辆状态提供者尚未完成，因此不提供可用入口 |
| 应用配套 | 原版绿色 CarPlay 图标、自定义窗口顶栏、连接/设置首页、首帧自动进入独立画面页、全屏与日夜切换、Windows设备选择和诊断；完整多语言、开机启动、签名安装、更新机制尚未完成 |
| Android/BYD | 仅共享核心设计与上游参考，原生外壳/车辆扩展尚未实现 |
| HarmonyOS NEXT | 尚未建立原生权限/接口验证工程，不能承诺RFCOMM、USB、后台音频权限可得 |

## 本地检查命令

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
./scripts/with-gstreamer.ps1 -Command @('cargo','test','--workspace','--all-features','--locked')
./scripts/with-gstreamer.ps1 -Command @('cargo','test','-p','carplay-media','--features','gstreamer','--locked','--','--ignored')
./scripts/with-gstreamer.ps1 -Command @('./target/debug/carplay-desktop.exe','--smoke-test')
```

合成媒体测试以 appsink 接收音频，不打开麦克风或扬声器。Windows 软件解码已通过；自动 D3D12 HEVC 解码曾出现 `0x887a0005` / `0x887a0020`，因此默认管线显式使用 avdec 软件解码。

2026-10-08 本地记录：整合全workspace native features与ignored合成用例143项通过（日志`.local/checks/workspace-native-tests.log`）。桌面Windows启动与两秒自动退出测试通过。全workspace/all-features/all-targets Clippy通过。Linux CLI完整生产依赖链交叉check通过。GitHub CI尚未运行。

2026-10-09 输入修复：三项桌面回归验证按下/抬起保留相同非零坐标、失焦/离开面板仅释放一次、大量运动合并但不丢点击边沿；均通过。接收器还验证真实加密事件中的同坐标按下/抬起，以及128条队列满载时明确断开并清除旧输入。此次桌面/接收器含原生合成音源的22项测试与Clippy均通过。用户随后确认“目前功能正常了，初步测试完”；该反馈记录为当前设备的初测通过，不代替20次连接循环、两小时稳定性或跨平台验收。

2026-10-09 桌面交互更新：17 项桌面测试通过，覆盖首个实际解码帧自动切页、返回首页后持续来帧不反跳、旧帧和未认证帧不能重开画面、切页当帧输入隔离、失焦/返回释放触控、窗口按钮/标题拖动与双击。Windows native 构建、Clippy 和首页实际渲染截图检查通过。保留原比例与 4px 窗口边缘隔离，避免窗口缩放手势传给 iPhone；此轮新界面仍需用户在已连接的真机上确认操作体验。

`fuzz/` 包含 link/TLV/USBMUX/NCM/RTSP/media 等输入入口。构建 fuzz target 或有限 deterministic corpus 运行不等于覆盖率 fuzz 已充分完成，应另在 Linux nightly + ASan 环境持续运行。

## 真机记录要求

每轮记录程序 revision、iOS版本、电脑系统/无线适配器、连接路径、协商格式、成功到达的最后阶段、可复现操作和脱敏日志。每个平台分别完成：

- 有线/无线各20次连接断开，覆盖拔插、网络丢失、重启与休眠。
- 两小时音乐/导航/Siri/来电叠加测试，检查声音恢复、丢帧、音频欠载、延迟、资源增长。
- 触控/旋转/分辨率和第二屏测试，断线和失焦无卡键。
- 权限拒绝与恢复；Windows驱动绑定前后原有iPhone功能及配置还原。
- 与同一部iPhone、同配置DiPlay对照。未经测试的平台/车型继续标记未验证。

若同局域网受 AP 隔离/mDNS 阻挡，记录网络环境，不通过关闭防火墙或伪造发现结果制造成功。未完成真实连接前，此文档不记录虚构延迟或兼容设备数量。


2026-10-09 三模式配置更新：Windows全workspace/all-features（含ignored合成用例）178项测试通过，日志 `.local/checks/connection-modes-tests.log`；全目标Clippy通过。新增模式参数隔离、当前系统WiFi明文权限与敏感信息遮罩、有线0x4301/0xAE03与禁发WiFi、有线媒体IPv6 scope保留、热点actor跨线程/取消竞态/清理失败测试。本机LAN系统profile读取以普通权限成功（仅记录secret_available=true）；热点只读能力/状态查询反复通过（未启停）；本轮接线前的 USB 只读枚举为 []。这些单测和预检结果不作为 USB 或热点真机连接证据，后续 USB 实测另记如下。

2026-10-09 Windows USB 首次贯通：本机 MSYS2 libimobiledevice 运行时与已有 USB 配对验证成功；USB 会话日志 `.local/checks/windows-usb-session-2.log` 依次记录 `USB: Authenticated`、`Verified`、`StreamStarted(110)`、`FirstVideoFrame(110)`。原生 UsbNcm 的 MI03 接口为 Up，激活 USB 配置 6，IPv6 scope 63；这些是本次观测值，程序不将其用作固定配置。

桌面开发程序 `.local/run/carplay-desktop-usb.exe` 随后实际显示 1280×720 CarPlay 地图/媒体主页，并自动进入画面页；对应 `.local/logs/session-1791512463.log` 记录 USB 消息和 AirPlay 验证/视频事件。可见画面由实际窗口观察确认，不能仅用 `FirstVideoFrame` 接收事件推断桌面渲染。窗口正常退出，退出码为 0；退出本身不等于触控、音频或异常恢复验证。

准备过程中，系统 PnP restart 曾返回 3010/50，需要手动拔线、等待五秒、重新插入并解锁同一手机；`start-windows-usb.ps1 -ResumeAfterReplug` 续跑已成功。本轮复用原有 `libusb0.sys`，没有全局升级；完成后已通过 Disarm 恢复配置选择注册表原值，但当前激活配置和该手机实例的 CDC/过滤器设置仍保留。Disarm 不代表完整恢复或卸载。

用户随后明确反馈 USB“点击和返回正常，声音待测”，记录为当时的触控初测通过；音频后续发现的问题及修复结果另记如下。只验证了这一台 iPhone；旋转/分辨率变化等完整输入、20 次连接循环、两小时运行、休眠、异常拔插及完整驱动恢复仍待验收。

同轮 Windows 回归：workspace `--all-features` 并包含 ignored 用例，共 194 项通过、0 失败，日志 `.local/checks/windows-usb-final-tests.log`；全 workspace/all-targets/all-features Clippy `-D warnings` 通过，日志 `.local/checks/windows-usb-final-clippy.log`。USB 注册表模拟回归与 PowerShell 脚本解析检查通过；这些检查不代替上述尚未完成的真机验收。

2026-10-09 USB 音频启动修复：用户先确认无线声音正常，但 USB 播放声音时断开，继续播放状态下重连也再次断开。旧日志 `.local/logs/session-1791512883.log` 共记录 7 次 type 100 音频启动后 `audio sink 100: media event queue is full`，随后会话断开。原因是首个音频包在共用媒体工作线程中同步初始化音频设备，高频 PCM 包在此期间堆满事件队列。

修复引入 `AudioConfig`：SETUP 调用线程先初始化音频管线，再交给媒体工作线程安装并等待确认，最后返回音频端口；音频首包不再创建管线，也不让初始化阻塞已有视频或音频流。本轮没有修改 USB 驱动。新日志 `.local/logs/session-1791513484.log` 记录两次监听及视频首帧，分别在 35.565 秒和 64.902 秒启动 type 100 音频，未再出现队列满或 `Disconnected` 事件。用户明确确认“声音正常，暂停和重连也正常”，记录为当前设备的声音、暂停和重连初测通过；不据此推断 20 次循环、两小时稳定性或其他设备已通过。

修复回归：media/receiver 全 features、包含 ignored 的 38 项测试通过，日志 `.local/checks/usb-audio-startup-tests.log`，覆盖音频配置确认前不返回 SETUP、配置失败清理、首包顺序与冷启动并发场景；全 workspace/all-targets/all-features Clippy `-D warnings` 通过，日志 `.local/checks/usb-audio-startup-clippy.log`；格式检查通过。音频混音、Siri/来电抢占、设备切换与长时间时钟同步仍需单独验收。
