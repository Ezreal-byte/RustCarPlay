# USB 连接后端

USB 模式不使用蓝牙地址、Wi-Fi 名称或密码。Windows 和 Linux 都已有系统 USBMUX / Lockdown / CarKit / NCM 接收代码路径。2026-10-09，本机 Windows 与一台 iPhone 已完成有线认证和 NCM 视频传输，桌面实际显示 1280×720 CarPlay 主页并自动进入画面页。用户确认点击和返回正常，音频启动修复后声音、暂停和重连也正常；完整输入、20 次连接循环、两小时稳定性与异常恢复仍待验收。Linux 尚未完成真机连接。

## Linux 系统后端

`carplay_platform::usb::system::prepare_system` 的路径是：

1. 枚举 iPhone，以物理 USB 端口选定设备。存在多台时要求选择，不取第一台。
2. 读取完整配置描述符。需要时发送 DiPlay 使用的 Apple vendor request `0xC0/0x52`，`value=0`、`index=4`，等待同一端口重新枚举，并校验设备序列号。
3. 按 USBMUX 和 CDC NCM 描述符选择配置，不把配置值固定为 6。
4. 保留系统 `usbmuxd` 对 USBMUX 的管理和 `cdc_ncm` 对网络接口的管理。不会抢占或卸载它们。
5. 将该 USB 设备的序列号与 usbmuxd 的 USB UDID 精确匹配。USB 连接不会回退到 Wi-Fi 配对设备。
6. 通过系统 `libimobiledevice-1.0.so.6` 完成 Lockdown 配对、TLS 会话和 `com.apple.carkit.service` 启动。新设备需要在 iPhone 上解锁并选择“信任此电脑”。配对材料由系统库保存在其既有存储中。
7. 返回 CarKit 字节流、该手机 NCM 接口的 IPv6 link-local 地址及 scope ID、接口 MAC 和 USBMUX 接口号，交给 Rust iAP2/AirPlay 会话。

Linux 主机需要安装发行版提供的 `libimobiledevice6`、`usbmuxd`，具备 USB 访问权限，并让该手机的 `cdc_ncm` 接口处于启用状态且拥有 IPv6 link-local 地址。连接函数不安装软件、修改驱动绑定、建立路由或更改 Wi-Fi。系统前置条件缺失时返回明确错误。

Ubuntu 24.04 可使用发行版包安装前置组件（软件不会自动运行这些管理员命令）：

```sh
sudo apt update
sudo apt install libimobiledevice6 libimobiledevice-utils usbmuxd
systemctl status usbmuxd --no-pager
```

`usbmuxd` 可能由设备事件按需启动；连接 iPhone 后服务仍未运行时，可以执行 `sudo systemctl start usbmuxd`。安装包提供的设备访问规则和当前登录会话权限必须允许读取与配置所选 USB 设备。不要用全局放宽 `/dev/bus/usb` 权限的方式解决访问错误。

只读检查 NCM 状态：

```sh
ip -br link
ip -6 address show scope link
```

Linux 系统后端当前要求操作系统已启用目标 NCM 接口。若没有 IPv6 link-local，先检查该接口的网络管理策略、IPv6 是否关闭以及 `cdc_ncm` 是否绑定，不要用局域网 IP 代替。部分系统在 USB/NCM carrier 建立前不会自动配置 IPv6，这一时序还需要真机验证；程序会给出网络前置条件错误，不伪装为已连接。

发现网络接口时同时校验 sysfs 设备父路径、USB 接口号和 `cdc_ncm` 驱动，不能将另一部手机的热点网卡或普通局域网地址误当作 USB 媒体链路。地址交给 socket 时必须保留 IPv6 scope ID。

CarKit 流保留创建服务时的 Lockdown 会话；关闭时依次释放服务、Lockdown 和设备句柄。读操作使用可调整的有界超时，写操作具有 2 秒 socket 超时。libimobiledevice 1.3.x 的 TLS 超时读取采用读满语义，适配层在启用 TLS 后每次请求一个字节，防止短 iAP2 帧在超时时被库消费却未返回；媒体数据直接走 NCM，不受该控制流策略影响。原生库配对和 TLS 调用受库自身超时控制，必须放在工作线程中，不能在界面线程调用。

## Windows 系统后端

Windows 的 `usb::system::prepare_system` 使用动态加载的 libimobiledevice，通过 Apple Mobile Device Service 的本机 USBMUX 服务完成配对和 `com.apple.carkit.service`。它按所选手机序列号精确匹配 USB UDID，保留 Lockdown/TLS 生命周期，再把 CarKit 字节流交给已有 Rust iAP2 实现。普通连接不安装驱动，也不从 USB 回退到蓝牙或 Wi-Fi。

需要以下组件：

1. Windows 11 x64，系统 Apple Mobile Device Service 可用，iPhone 已解锁并完成电脑信任。通常由 Apple Devices/iTunes 与其移动设备支持组件提供。
2. 当前项目目录的用户态运行时：普通 PowerShell 7 执行 `./scripts/setup-usb-runtime.ps1`。固定依赖与 SHA-256 见 `scripts/usb-runtime-packages.json`，下载、DLL、诊断工具和许可证均放在 `.local/usb-runtime/`。脚本不安装系统软件或改变 PATH。
3. 所选 iPhone 的 usbccgp 配置能够同时暴露 USBMUX 和 CDC NCM，NCM 控制/数据接口由 UsbNcm 正确绑定。准备与恢复步骤见 [Windows USB 驱动配置](WINDOWS_USB_DRIVER.md)。

独立启动程序时可设置当前进程的 `RUSTCARPLAY_LIBIMOBILEDEVICE_DIR` 为 `.local/usb-runtime/bin` 的绝对路径；无显式配置时，发行布局为程序旁 `runtime/libimobiledevice/`。DLL 加载使用受限搜索路径，避免从当前目录或 PATH 意外加载同名依赖。需要保留的 Apple USBMUX 服务地址为本机 `localhost:27015`，物理 USB 模式不接受远程 USBMUX 重定向。

开发环境的准备入口如下。先在普通终端构建工具，再在管理员 PowerShell 7 中运行准备脚本；脚本只准备所选手机，不启动桌面。重新插线或设备重启后，手机可能退回普通模式，需要再次准备。准备成功的 JSON 仅表示所选配置已激活，仍须在桌面 USB 模式完成 iAP2 和媒体连接。

```powershell
cargo build --locked -p carplay-platform --examples
./scripts/start-windows-usb.ps1 -Action Prepare
```

有多台手机时，先运行 `usb_probe`，将所选结果的 `id` 传给准备脚本的 `-DeviceId`。完成管理员准备后，在普通终端用 `with-gstreamer.ps1` 启动桌面；该开发包装脚本自动发现本工作区已准备的运行时绝对目录。

本机已有 `libusb0.sys`，此次准备使用 `-UseExistingFilter` 复用原驱动，没有全局升级。该选项只适用于通过脚本校验的现有过滤驱动，不能用来跳过驱动检查。系统 PnP restart 在本机曾返回 3010/50，实际需要手动拔线、等待五秒、重新插入并解锁同一部手机，再运行：

```powershell
./scripts/start-windows-usb.ps1 -Action Prepare -UseExistingFilter -ResumeAfterReplug
```

这个续跑路径已实际成功。它要求先前流程已保存同一手机的近期完整描述符，并确认重插后处于初始 USB 模式；它不能代替首次准备或复用其他手机的描述符。若缺少这些条件，按错误提示重新完成正常准备，不手填配置索引。

WinUSB 子接口不能改变整个复合设备配置，但能够使用 **usbccgp 已激活** 的非首配置。当前预检按实际激活的配置值判断；管理员准备工具按完整描述符的索引配置 usbccgp，不能把 `bConfigurationValue` 和描述符索引混用。设备启用或重启可能使 iPhone 退回初始模式，因此运行流程需先恢复安全配置，再按正确顺序写目标索引并请求 mode 4；恢复工具保存原值，不能长期留下手机初始模式不支持的配置索引。

部分 Apple 驱动组合阻止从 WinUSB 接口发出设备控制请求。本机首次尝试已观察到接口打开返回系统错误 50。可选 `libusb-win32` 父设备过滤器只用于发送 Apple `0xC0/0x52` mode 4 控制请求；它不接管 USBMUX bulk 或 NCM 数据。安装独立于普通连接，并仅针对选定设备实例，具有原值备份和恢复入口。是否需要临时移除该实例的 AppleLowerFilter、启用 CDC Union 枚举，由描述符和驱动状态决定，详见驱动说明。

Windows NCM 发现检查精确 PnP 父链、NCM 控制接口号和 `UsbNcm` 服务，再读取其 MAC 和可绑定的 IPv6 link-local 地址。仅要求接口已启用，不等待 carrier 或对端可达，因为 iPhone 可能在 iAP2 `StartCarPlaySession` 之前不发送 NCM 流量。系统没有生成本地 IPv6 地址时返回具体网络前置条件错误，不使用其他网卡地址代替。

低层 `usb::native::open` 提供 USBMUX/NCM bulk pipe，供后续独立驱动和网络桥集成使用；获得 pipe 不代表完成 Lockdown、iAP2、AirPlay 或首帧验证。

## 验证状态

- 描述符匹配、已激活非首配置的接受、未激活配置的拒绝、模式请求参数、取消前置校验有测试。
- Windows/Linux 系统 FFI、UDID 精确匹配、MAC 校验、信任错误分类与旧版库 TLS 短帧兼容已实现。Linux 分支通过交叉编译检查。
- 2026-10-09 已在 Windows 本机安装经校验的 MSYS2 用户态运行时，`idevice_id -l` 枚举到一台 USB 手机，`idevicepair validate` 成功验证已有配对；未创建新配对。
- 同日 Windows 有线会话日志 `.local/checks/windows-usb-session-2.log` 记录 `USB: Authenticated`、`Verified`、`StreamStarted(110)` 和 `FirstVideoFrame(110)`。本次设备激活配置为 6，系统 UsbNcm 的 MI03 接口为 Up，IPv6 scope 为 63；这些是本机观测值，不是其他设备的固定配置。
- 桌面会话 `.local/logs/session-1791512463.log` 记录 USB 与上述 AirPlay 事件；另经实际窗口观察确认 1280×720 CarPlay 地图/媒体主页可见，且自动跳转到画面页。日志中的首帧接收事件本身不作为可见画面的证明。
- 用户确认 USB 点击和返回正常；修复音频启动导致的断线后，又明确确认“声音正常，暂停和重连也正常”。新日志 `.local/logs/session-1791513484.log` 中两轮连接均到达首帧和 type 100 音频启动，未再出现队列满或 `Disconnected` 事件。音频管线在 SETUP 调用线程完成初始化并交给媒体工作线程确认后，才返回音频端口，避免首包初始化阻塞既有媒体流。本轮没有修改驱动。
- 仅验证了这一台 iPhone 和当前 Windows 环境；旋转/分辨率变化等完整输入、音频混音/Siri/设备切换、20 次连接循环、两小时媒体运行、休眠与异常拔插恢复仍待验收。Linux 有线真机仍未测试。详细证据记录在 [验收记录](ACCEPTANCE.md)。

参考：[Microsoft usbccgp 非默认配置](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/selecting-the-configuration-for-a-multiple-interface--composite--usb-d)、[CDC 接口集合枚举](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/support-for-interface-collections)、[libimobiledevice 设备连接接口](https://github.com/libimobiledevice/libimobiledevice/blob/master/include/libimobiledevice/libimobiledevice.h)、[Lockdown 接口](https://github.com/libimobiledevice/libimobiledevice/blob/master/include/libimobiledevice/lockdown.h)。

## 只读预检与退出

在工程目录运行以下命令，只会读取 USB 枚举、配置描述符和驱动信息：

```sh
cargo run --locked -p carplay-platform --example usb_probe
```

输出 `[]` 表示没有枚举到符合 iPhone/USBMUX 特征的设备；`driver_setup_required` 表示 Windows 驱动或配置条件未满足；`interfaces_available` 只表示描述符条件通过，不能证明配对、NCM、CarPlay 会话或画面成功。结果不包含手机序列号或 Wi-Fi 凭据。

第一次 USB 真机验证应先保存预检结果，然后在桌面选择 USB 模式连接，依次确认信任提示、iAP2 认证、AirPlay 验证和实际视频首帧，最后验证断开与拔插恢复。取消会关闭已打开的本次 CarKit/Lockdown 句柄；库内部正在执行的原生调用会在自身超时后返回。

Linux 普通连接没有持久安装驱动或改写系统路由。Windows 如果使用管理员准备工具，配置与过滤器会有持久变更，**拔插不能替代恢复**：应按 [驱动说明](WINDOWS_USB_DRIVER.md) 恢复对应实例的备份，再重启设备/拔插并确认原有 iPhone 功能。过滤器恢复会解绑本次安装到该实例的过滤器；为避免影响其他设备，其共享服务、驱动文件和已登记目录保留，不能称为完整系统卸载。备份位于 `.local/windows-usb/`，包含真实设备实例路径，不应作为公开诊断附件。

准备流程的 `Disarm` 会恢复原先的配置选择注册表值，同时保留当前已经激活的 USB 配置；它不是完整恢复。此次成功连接后已完成 Disarm，但该手机实例的 CDC 枚举与过滤器设置仍持久保留，后续继续联调可复用。

手机仍可枚举时可用 `./scripts/start-windows-usb.ps1 -Action Restore` 依次恢复配置与过滤器。手机已拔出或枚举异常时，可向该命令提供已有备份的 `-DeviceToken`，或使用驱动说明中的单独恢复命令。完整恢复及原有 iPhone 功能恢复仍应按验收要求验证。

安装命令的包名已按 [Ubuntu 24.04 的 libimobiledevice6](https://packages.ubuntu.com/noble/libimobiledevice6) 和 [usbmuxd](https://packages.ubuntu.com/noble/usbmuxd) 包索引核对。Lockdown 的 `StartService` 按 DiPlay 的公开流程调用，不更改 `EnableWifiConnections` 等设备设置。
