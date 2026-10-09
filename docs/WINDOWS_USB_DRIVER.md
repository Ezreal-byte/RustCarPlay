# Windows USB 配置与驱动准备

Windows 有线接收继续使用系统 `usbccgp` 复合设备父驱动、Apple USBMUX/Lockdown 服务和 Windows 11 内置 `UsbNcm` 网络驱动。WinUSB 用户态接口不能直接选择非首配置，但 `usbccgp` 可以在重新枚举时按设备注册表选择配置；因此不需要先把整个 iPhone 替换成 WinUSB。

本机已经完成 Windows USB 真机首轮连接：父设备的单设备过滤器成功加载，mode 4 实测配置为描述符索引 `5`、`bConfigurationValue=6`；系统 `UsbNcm` 管理 NCM `MI_03/04`，网络接口为 Up，测试时 IPv6 scope ID 为 `63`。CLI 和桌面 GUI 均完成认证并显示 `1280×720` 画面，用户确认点击和返回正常。音频启动修复后，用户又确认声音、暂停和重连正常；本次修复没有修改驱动，仍不能据此宣称全部功能、其他设备或长期稳定性已通过。

原始状态为 PID `12A8`、父设备 `usbccgp`、USBMUX `MI_01` 使用 WinUSB 和 Apple 过滤器；普通 Apple Ethernet 为 `ff/fd/01`，并非 CarPlay CDC NCM。原 `OriginalConfigurationValue=2` 对应活动配置值 `3`。直接 claim Apple WinUSB 接口返回 Windows 错误 50，因此本机复用已有 `libusb0.sys 1.2.6.0` 执行模式控制请求，保留其全局驱动文件和服务配置，USBMUX 仍由 Apple 服务处理。

## 统一准备与恢复入口

v0.1.2 本地修复版在 USB 连接配置中提供“准备 Windows USB”入口。先结束接收会话，连接并解锁 iPhone，选中该设备后点击准备，在 Windows 管理员提示中批准。准备完成后再连接；若提示需要拔插，拔出数据线、等待至少五秒，再接回并解锁同一部手机，然后点击续接。准备在后台执行，普通连接不会自行安装驱动。

安装版和开发版共用 `%LOCALAPPDATA%\RustCarPlay\.local\windows-usb` 保存恢复记录、描述符和驱动准备缓存；不会写入安装目录。更新或卸载应用时应保留这些记录，直到完成恢复和复测。可用 `RUSTCARPLAY_USB_STATE_DIR` 或脚本的 `-StateDirectory` 显式指定绝对目录。

以下管理员终端命令保留用于排查。系统自带 Windows PowerShell 5.1 和 PowerShell 7 均可使用。下例用于本机已验证的旧驱动复用路径；只连接一台手机时自动选定，多台手机时需提供 `usb_probe` 返回的 `-DeviceId`。

```powershell
./scripts/start-windows-usb.ps1 -Action Prepare -UseExistingFilter
```

脚本检查 USB 运行库和 Apple 服务，然后按选定父实例完成过滤器准备、CDC 枚举、模式探测与配置选择。准备成功后，在桌面程序选择 USB 模式连接。没有预先安装 libusb0 的电脑可以省略 `-UseExistingFilter`，但首次签名驱动安装路径尚未在本轮真机验证。

本机准备过程中遇到 PnP 返回 `3010` 和错误 `50`，实际需要物理拔插。管理员命令行的续接入口为：

```powershell
./scripts/start-windows-usb.ps1 -Action Prepare -UseExistingFilter -ResumeAfterReplug
```

续接要求同一物理手机已经回到初始 USB 模式。如果上次失败发生在首次重启、尚无描述符，续接跳过已由拔插替代的首次软件重启，继续 mode 4 探测；后续复位也可能要求再拔插一次。如果已有 **30 分钟内、同一父实例 token** 的报告，则验证后直接执行 Arm 和 mode 4，并核验实际活动配置。缺失或过期的报告会重新探测，不会盲用。早先真机验证过有报告的续接路径；本次新增两次拔插阶段推进已通过模拟回归，尚未重新进行真机准备。

旧开发目录或旧便携包的恢复记录可显式通过 `-LegacyStateDirectory '<旧目录>/.local/windows-usb'` 迁入共享目录。工具仅复制当前手机经完整校验的记录，保留原件；新目录已有不同记录时拒绝覆盖。开发启动路径可自动定位其旧记录，安装程序不会搜索磁盘上的其他开发目录。

准备结束会执行 Disarm，恢复最初的配置注册表值而不重启设备，保留已经活动的 NCM 链路，使以后拔插仍可从安全的普通配置启动。CDC 枚举设置和所选父设备的过滤器关联仍然持久存在；Disarm 不等于完整恢复。当前也不能把一次准备成功当成每次拔插都会自动完成连接。

完整恢复使用同一统一入口；设备已无法枚举或已拔出时，可指定此前 `Inspect` 返回的 `-DeviceToken`：

```powershell
./scripts/start-windows-usb.ps1 -Action Restore
./scripts/start-windows-usb.ps1 -Action Restore -DeviceToken '<token>'
```

这两条是正常枚举与按恢复记录定位的两种替代用法。恢复顺序是先还原配置及 CDC/AppleLowerFilter 状态，再移除本项目附加的手机过滤器关联。若系统要求物理拔插，按提示完成后再验证原有 iPhone 功能；本轮尚未完成这些功能的实际恢复验收。

## 工具和变更边界

下列只读检查无需管理员，兼容 Windows PowerShell 5.1 与 PowerShell 7：

```powershell
./scripts/windows-usb-config.ps1 -Action Inspect
./scripts/windows-usb-filter.ps1 -Action Verify
```

`Inspect` 只输出散列后的 `device_token`、PID、配置索引和过滤器/CDC 枚举状态，不打印设备序列号。`Verify` 优先读取随包的 `runtime/usb-driver/libusb-win32-bin-1.4.0.2.zip`，缺少随包资源时才使用固定版本下载来源。两条路径都校验固定 SHA-256、文件 Authenticode、Microsoft WHCP 目录签名和实际驱动文件归属，不修改驱动或设备。

v0.1.2 使用随包静态工具 `tools/usb_driver_verify.exe`，通过 Windows `DRIVER_ACTION_VERIFY` 内核签名策略及 catalog 成员校验验证标准 1.4.0.2 驱动，并使用系统 catalog API 注册签名目录。常规新安装无需另装 PowerShell 7 或 Windows SDK。源码开发可回退使用 SDK SignTool；复用下述已知旧 1.2.6.0 交叉签名驱动时仍使用可选 SDK `/kp` 兼容验证，缺少工具会明确停止，不替换共享驱动或降低签名要求。

该过滤驱动精确 release 的对应源码包含于相应版本的 Windows native-source 归档，随包保留上游许可与来源记录。Apple Mobile Device Service、Apple 驱动和系统 UsbNcm 不随包分发；需要电脑已经安装并能使用 Apple Devices 或 iTunes 的 USB 服务。提供 ZIP 不改变下文单设备选择、签名验证、管理员安装及恢复流程，也不代表全新系统首次安装已验收。

固定下载来源：[libusb-win32 官方 release](https://github.com/mcuee/libusb-win32/releases/tag/release_1.4.0.2)。归档 SHA-256 为 `00004c92cdb99be36e17fb2377165eb97e63b48ba895bfc04a642ea9c3e26d94`。验证已经在开发机通过。本轮真机实际复用原有 1.2.6.0 驱动，没有安装该包的全局驱动；因此首次安装 1.4.0.2 的路径仍待验证。签名有效不等于其他系统一定允许加载；如系统策略拒绝，返回错误，不关闭 Secure Boot、内存完整性或签名验证。

### 复用电脑中原有的 libusb0

开发机已有其他软件安装的 `libusb0.sys 1.2.6.0`，准备前服务为手动、已停止。本轮已通过显式 `-UseExistingFilter` 路径复用并成功加载，没有覆盖升级。以下低层命令用于检查与排查，日常准备优先使用前述统一入口：

```powershell
./scripts/windows-usb-filter.ps1 -Action Verify -UseExistingFilter
./scripts/windows-usb-filter.ps1 -Action Install -DeviceToken '<token>' -UseExistingFilter
```

仅接受已审查的文件版本/哈希：现有 1.2.6.0 的 SHA-256 为 `8058f2afe6ef96a7d2ded432997fd8655970c9ea75a938ee4557d6a2cb4cc989`，或固定 1.4.0.2 包的对应文件。每次复用校验 Authenticode、对应内核签名策略、服务路径、内核驱动类型、手动启动类型和稳定服务状态，并在关联前复查；已有恢复记录也必须与当前驱动哈希相符。1.4.0.2 使用固定 catalog 的成员验证，旧 1.2.6.0 使用 SDK 的嵌入式内核签名验证。现有 1.2.6.0 已通过开发机签名检查和实际加载验证，其全局驱动文件、服务配置及 catalog 保持原样。

这条路径只向所选手机的 `UpperFilters` 追加 `libusb0`，然后重启该手机。不运行会影响其他 libusb 设备的安装器，不更改共享服务、SYS 文件或 catalog，记录 `OwnService=false / OwnFile=false`，恢复时同样保留原有组件。旧驱动只用于非零长度的描述符/Apple 模式控制请求，USBMUX 和媒体传输由其他系统后端承担。`driver_api.h` 在上游 [2012 年实现](https://github.com/mcuee/libusb-win32/blob/79d596df81a27b27545a7a2806ba0f1279a1bd9a/libusb/src/driver/driver_api.h) 和 1.4.0.2 中的相关结构/IOCTL 未变；Apple GET_MODE/SET_MODE 的长度为 4/1，不触发 [libusb 文档记录的旧版零长度控制传输限制](https://github.com/libusb/libusb/wiki/Windows#known-restrictions)。这不构成对旧驱动其他用途或整体稳定性的承诺。

以下是统一入口内部使用的低层管理员操作，必须以 `Inspect` 返回的完整 token 选定同一个父设备。命令中的 `<token>` 是占位符；本机有原有驱动，单独调用过滤器 `Install` 时需加 `-UseExistingFilter`。

```powershell
./scripts/windows-usb-config.ps1 -Action Prepare -DeviceToken '<token>' -DetachAppleLowerFilter
./scripts/windows-usb-filter.ps1 -Action Install -DeviceToken '<token>'
```

- `Prepare` 先保存原值及原来的“不存在”状态，然后仅从所选父设备的 `LowerFilters` 移除 `AppleLowerFilter`，保留其余条目和顺序；在该父设备关联的软件键设置 `EnumeratorClass` 为二进制 `02 00 00`，让 `usbccgp` 按 CDC Union 描述符合并 NCM 控制/数据接口。这个操作自身不重启设备，也不预设目标配置。
- 首次 `Install` 仅在系统不存在 `libusb0` 服务、目标驱动文件和该手机的已有过滤器时执行。先保存恢复记录、注册已验证的 Microsoft 签名 catalog、复制对应 x64 驱动，再调用上游安装程序的 **`--device-id=<完整实例 ID>`**。不能改成 `--device=`，后者按硬件 ID 匹配范围更大。安装器会重启所选父设备。
- 不修改 USB 类过滤器，不替换 USBMUX/NCM 功能驱动，不禁用照片接口，不开启 ICS、DHCP、系统热点或路由转发。不触碰其他手机和 USB 设备的过滤器。
- 安装器会删除设备硬件键的 `SurpriseRemovalOK`；脚本也备份并在恢复时还原它。完整实例路径只保存在用户数据目录下的 `.local/windows-usb/` 本地恢复文件，勿将该目录加入诊断包。

## 按描述符切换配置

不能照抄其他项目 mode 3 的配置索引，也不能将 `bConfigurationValue` 当成索引。需要在同一部手机发送 `0xC0 / 0x52 / value=0 / index=4` 后，以实际完整描述符找到 USBMUX + CDC NCM 配置，保存只包含这一台手机的 `usb_probe` JSON 报告。首次探测和读取不会自动写注册表。

```powershell
$state = Join-Path $env:LOCALAPPDATA 'RustCarPlay/.local/windows-usb'
[IO.Directory]::CreateDirectory($state) | Out-Null
$report = Join-Path $state 'phone-descriptors.json'
cargo run --locked -p carplay-platform --example usb_probe | Set-Content -LiteralPath $report -Encoding UTF8
./scripts/windows-usb-config.ps1 -Action Arm -DeviceToken '<token>' -DescriptorReport $report
```

`Arm` 验证报告只有一台相同 PID、相同 `windows_device_token` 的设备且包含已解析的 CarPlay 配置，使用 `configuration_index` 写 `OriginalConfigurationValue`，将最初的安全索引作为 `AltConfigurationValue`。`usb_probe` 与脚本对完整父实例 ID 使用相同散列；同型号的另一部手机不能拿来替代。软件后端还应按实例和物理端口验证重枚举身份。

实机验证顺序是：记录/准备 → 在普通安全配置中完成设备重启或必要的物理拔插 → 写已探测到的 mode 4 目标索引 → 发送 mode 4 请求，由手机主动重新枚举 → 验证 USBMUX、NCM 和带 scope 的 IPv6 link-local → 认证及首帧。之后不要再随意执行 PnP restart，因为它可能使手机回到普通模式。本机已验证到 GUI 首帧和触控返回，实际索引 `5`/配置值 `6` 仍只是本次设备的实测值，不应硬编码到其他手机。

Microsoft 当前配置文档的表格和部分正文对 index/value 用词不一致；本工具按 USB 团队说明与本机 `2 → 活动配置 3` 的实测使用零基描述符索引，保留报告中的配置值用于核对。首次 mode 4 的目标索引必须实测后再写。

## 退出与恢复

仅解除本次目标配置预置、保留驱动准备：

```powershell
./scripts/windows-usb-config.ps1 -Action Disarm -DeviceToken '<token>'
```

恢复本项目改动的设备配置及过滤器：

```powershell
./scripts/windows-usb-config.ps1 -Action Restore -DeviceToken '<token>'
./scripts/windows-usb-filter.ps1 -Action Restore -DeviceToken '<token>'
```

先恢复配置，再恢复过滤器；后者会重启仍在连接的所选设备。若已拔出，则下次插入使用恢复后的设置。恢复会校验当前值是否仍是原值或本项目写入值，遇到其他软件改动会停止，避免覆盖。崩溃造成的部分写入有持久记录可恢复。还应复测 Apple Devices/USBMUX、照片导入和个人热点等原有功能。

过滤器恢复不会删除全局 `libusb0` 服务、驱动文件或签名 catalog：本机这些组件原本属于其他软件，其他电脑也可能在安装后开始共享使用。恢复操作还原的是手机的过滤器关联，不能把这一行为宣称为全系统零残留卸载。若需要完全清理，应先检查全部设备和类过滤器引用，再单独删除确实仍由本项目独占的服务/文件/catalog。脚本拒绝覆盖未知来源的共享安装。

恢复记录保留 `Detached` 状态，支持恢复后的再次安装：仅当记录证明服务/文件由本项目创建，文件哈希、服务类型/启动类型/路径以及手机原过滤器仍匹配时，复用这些组件并只重启所选手机；不会再次运行会影响已有 libusb 设备的上游安装器。部分安装失败也应先 `Restore`，再重试。

本地恢复记录必须保留到恢复并复测结束。无需重新创建认证身份文件。USB 模式不使用 Wi-Fi 密码。

## 验证

`./scripts/windows-usb-tests.ps1` 使用内存注册表验证：只删除目标过滤器、保留原顺序/类型/缺失状态、部分写入和中断恢复、外部改动冲突拒绝、恢复文件目标白名单。`windows-usb-paths-tests.ps1` 验证共享状态路径、真实临时文件创建/原子替换、旧记录精确迁移与冲突拒绝。`windows-usb-workflow-tests.ps1` 验证首次无描述符与第二次已有描述符的两次拔插推进、过期报告重探以及错误手机和未回初始模式的拒绝。这些测试已在 Windows PowerShell 5.1 和 PowerShell 7 通过，不修改真实设备。

本次 v0.1.2 修复还完成了 PowerShell 5.1 的真实只读标准驱动 Verify 与已知旧驱动 `/kp` 验证。静态签名 helper 的实际文件测试覆盖正确 catalog/驱动通过，以及错误成员、篡改文件、篡改 catalog 和无签名文件拒绝。尚未用本次修复版执行管理员驱动安装或重新连接手机；以下真机结果来自此前开发版。

本轮真机已通过：原有 1.2.6.0 过滤器复用与加载、物理拔插后的续接、mode 4 索引 `5`/值 `6` 选择、原生 UsbNcm 网络、CLI/GUI 认证、`1280×720` 画面，以及用户确认的点击、返回、声音、暂停和重连初测。Siri/麦克风、音频混音与设备切换、原有 Apple Devices/照片导入/个人热点功能的实际恢复、20 次连接循环及两小时媒体运行均未完成验收。其他 iPhone、其他 Windows 配置及全新驱动安装路径仍需单独验证。

音频初测曾出现播放即断线，原因是用户态媒体队列在音频设备初始化期间堆满；现已将音频初始化移到 SETUP 响应之前，并避免阻塞既有媒体流。修复版两轮连接日志 `.local/logs/session-1791513484.log` 均记录首帧和 type 100 音频启动，未再出现队列满或 `Disconnected` 事件；用户确认修复效果。此轮没有重新安装、升级或调整 USB 驱动，详细回归记录见 [验收记录](ACCEPTANCE.md)。

主要依据：[Microsoft usbccgp 配置](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/selecting-the-configuration-for-a-multiple-interface--composite--usb-d)、[Microsoft USB 团队的多配置说明](https://techcommunity.microsoft.com/blog/microsoftusbblog/multi-config-usb-devices-and-windows/270702)、[Microsoft CDC 接口集合枚举](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/support-for-interface-collections)、[Windows 内置 USB 类驱动](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/supported-usb-classes)、[SignTool catalog 安装](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/installing-a-catalog-file-by-using-signtool)。

Windows 特定时序和 AppleLowerFilter 的先例来自作者的 [mode 3 反向共享实机记录](https://github.com/0xbaksa/iphone-usb-reverse-tethering-windows)，它不是 mode 4 CarPlay 成功证据。本实现没有执行该项目的安装脚本，也没有采用其关闭照片接口和网络共享设置。过滤器实例匹配/重启副作用按 [libusb-win32 install.c](https://github.com/mcuee/libusb-win32/blob/release_1.4.0.2/libusb/src/install.c) 和 [registry.c](https://github.com/mcuee/libusb-win32/blob/release_1.4.0.2/libusb/src/registry.c) 核对。
