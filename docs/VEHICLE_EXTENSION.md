# 车辆扩展入口 / OEM extension entry

CarPlay 的应用列表现在包含一个通用车辆图标，默认名称为 **RustCarPlay**。点击后显示桌面端的“车辆扩展”占位窗口，保持当前连接，点击“返回 CarPlay”继续显示手机画面。当前没有接入车辆状态或控制能力。

The CarPlay launcher includes an original, neutral car tile named **RustCarPlay**. It opens the desktop vehicle placeholder without disconnecting the session. “返回 CarPlay” returns to the phone display. No vehicle data or controls are claimed by this placeholder.

二次开发入口 / Customization points:

- `ReceiverConfig.oem_label`：修改名称；桌面“更多显示选项”也可以设置，下次连接生效。
- `crates/carplay-receiver/assets/oem-car.png`：256×256 PNG，占位图由 `scripts/generate-oem-icon.py` 生成，未使用任何汽车品牌素材。替换图像尺寸时同步修改 `oem.rs` 的尺寸声明。
- `crates/carplay-receiver/src/oem.rs`：沿用 DiPlay 的 `oemIconVisible`、`oemIconLabel` 和 `oemIcons` 协议字段。
- `ReceiverEvent::UiRequested` → `Desktop::handle_event`：点击后的原生页面入口。将占位界面替换为自己的车辆页面，保留返回 CarPlay 和触摸释放行为。来自手机的 URL 是不可信数据，不能作为 shell 命令执行。
- `crates/carplay-core/src/vehicle.rs`：停车等车辆能力的真实数据与有效性校验。没有适配器时保持未提供，不生成虚构车辆状态。

Customize `oem_label`, replace the PNG and matching size declaration, and handle `UiRequested` in the native UI. Use real vehicle adapters for capabilities; keep phone-provided URLs as data and retain touch-release/session behavior when changing pages.
