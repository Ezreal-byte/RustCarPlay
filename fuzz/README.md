<!-- SPDX-License-Identifier: GPL-3.0-only -->
# 协议模糊测试

本目录是独立 Cargo workspace，应用的 `cargo test --workspace` 不会构建 libFuzzer。六个 target 不访问 USB、蓝牙、真实网络、麦克风或证书目录；媒体 target 不加载 GStreamer。

| target | 范围 |
|---|---|
| `iap2` | 校验和、增量帧、SYN 参数、已建立链路输入、重传时钟、关闭 |
| `tlv` | CSM/TLV 分片及往返、UTF-8、歌曲/导航/通话 metadata |
| `usbmux` | USB 设备帧、边界长度、iOS 额外四字节恢复 |
| `ncm` | NTB16/NDP 链表、数据报范围、编码/解码往返 |
| `rtsp` | 流式请求、拼接消息、头/body 长度边界 |
| `media_framing` | screen/RTP、NAL、avcC/hvcC、PCM/AAC/Opus、RGBA stride |

推荐在 Linux 上执行 sanitizer fuzz；也可以按 [Rust Fuzz Book 的 Windows 配置说明](https://rust-fuzz.github.io/book/cargo-fuzz/windows/setup.html) 安装 MSVC C++ 工具和 AddressSanitizer，在开发者终端执行。模糊测试需要 nightly 和 C++ 工具链，见 [官方安装文档](https://rust-fuzz.github.io/book/cargo-fuzz/setup.html)。以下命令不更改默认 Rust 工具链：

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --locked
cd fuzz
cargo +nightly fuzz run iap2 -- -max_total_time=60 -max_len=131072
cargo +nightly fuzz run tlv -- -max_total_time=60 -max_len=131072
cargo +nightly fuzz run usbmux -- -max_total_time=60 -max_len=131072
cargo +nightly fuzz run ncm -- -max_total_time=60 -max_len=131072
cargo +nightly fuzz run rtsp -- -max_total_time=60 -max_len=2113537
cargo +nightly fuzz run media_framing -- -max_total_time=60 -max_len=1048576
```

也可从仓库根目录执行 `cargo check --manifest-path fuzz/Cargo.toml --bins` 检查 target 类型与依赖编译。普通 `cargo build` 不会自动添加 `cargo fuzz` 所需的链接及插桩参数；Windows 上可能报告 `LNK1561` 缺少入口点。稳定版下的无插桩 smoke 不能代替上面的覆盖率引导和 sanitizer 检查。当前测试结果以任务验证记录为准；仓库存在 target 不代表已经执行过长时间 fuzz。

发现问题后保留最小复现输入并转为对应 crate 的回归测试。`corpus/`、`artifacts/`、`coverage/`、`target/` 默认不提交；不要把带身份信息的真实抓包或凭据用作公开 corpus。这里只验证协议解析，不实现 Android/BYD 车机服务插件。
