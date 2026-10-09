<!-- SPDX-License-Identifier: GPL-3.0-only -->
# carplay-discovery

DiPlay 的无线链路由接收端发布 `_airplay._tcp`，手机发布 `_carplay-ctrl._tcp`。本 crate 浏览手机服务，将真实选定的手机 IP/端口用于 `GET /ctrl-int/1/connect`；服务注册由 receiver crate 负责。

```rust,ignore
let mut options = carplay_discovery::Options::new(
    receiver.address, local_bluetooth.octets(), SOURCE_VERSION, iphone.octets(),
);
options.target_ip = explicitly_selected_phone_ip;
let mut discovery = carplay_discovery::start(options)?;
// Periodically drain discovery.events. Stop/Drop unregisters the browser and shuts down its daemon.
```

`target_ip` 是用户明确选定的手机地址，若提供则只允许该地址；否则仅匹配手机 TXT 中能解析为 MAC 的 `id`/`deviceid` 与 `target_bluetooth`。不会按服务名、首个发现结果或“同网段”猜测手机。缺少可识别 TXT 时发出 `PhoneSelectionRequired`，应用应提示选择手机 IP，再重建 discovery；事件的地址列表供明确选择使用，Debug 只显示数量。

TCP 绑定接收端选定的本地地址，使用相同 IP 地址族，IPv6 保留 mDNS 返回的 scope。每个已匹配端点最多执行配置的次数，成功后停止自动探测。新一轮 Bluetooth bootstrap 可调用 `handle.retry()` 重置已知端点的有限重试；活跃 AirPlay 会话中不要周期调用。HTTP 响应只读取受限头部，要求合法状态行；2xx 表示手机接受连接请求，不代表已通过鉴权或输出画面。

每次尝试都有总时限；连接最多阻塞 1 秒，读写以 100 毫秒轮询，stop 会关闭活跃 socket。跟踪端点与外部事件队列均限量。TXT 记录只是筛选线索，手机身份仍须由 AirPlay 配对验证；这里不实现密码、证书或 Android/BYD 插件管理。
