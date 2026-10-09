// SPDX-License-Identifier: GPL-3.0-only
// Original desktop UI. Apple CarPlay icon attribution: docs/THIRD_PARTY_NOTICES.md.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod chrome;
mod navigation;
mod style;
mod touch;
#[cfg(test)]
mod ui_tests;
use carplay_app::{
    AppEvent, Connection, ConnectionMode, ConnectionOptions, TransportOptions, WirelessDevice,
};
use carplay_core::{
    config::ReceiverConfig,
    input::{self, Rotation},
};
use eframe::egui;
use navigation::{Navigation, Page};
use std::{
    collections::VecDeque,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroize;

fn main() -> eframe::Result<()> {
    let smoke = std::env::args().any(|arg| arg == "--smoke-test");
    let screenshot = std::env::args()
        .skip_while(|arg| arg != "--screenshot")
        .nth(1);
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/carplay.png"))
        .expect("bundled CarPlay icon is valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180., 880.])
            .with_min_inner_size([820., 620.])
            .with_decorations(false)
            .with_icon(icon.clone()),
        ..Default::default()
    };
    eframe::run_native(
        "RustCarPlay",
        options,
        Box::new(move |cc| {
            style::install(&cc.egui_ctx);
            let font_path = if cfg!(windows) {
                "C:/Windows/Fonts/msyh.ttc"
            } else if cfg!(target_os = "macos") {
                "/System/Library/Fonts/PingFang.ttc"
            } else {
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"
            };
            if let Ok(bytes) = std::fs::read(font_path) {
                let mut fonts = egui::FontDefinitions::default();
                fonts
                    .font_data
                    .insert("cjk".into(), Arc::new(egui::FontData::from_owned(bytes)));
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .push("cjk".into());
                cc.egui_ctx.set_fonts(fonts);
            }
            let logo = cc.egui_ctx.load_texture(
                "carplay-logo",
                egui::ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                ),
                egui::TextureOptions::LINEAR,
            );
            Ok(Box::new(Desktop::new(smoke, screenshot, logo)))
        }),
    )
}

enum JobResult {
    Diagnostics(String),
    Auth(String),
    Hosts(HostChoices),
    HotspotCapability(String),
}
struct HostChoices {
    adapters: Vec<carplay_platform::bluetooth::LocalAdapter>,
    peers: Vec<carplay_platform::bluetooth::PairedDevice>,
    addresses: Vec<String>,
    wifi: Option<carplay_platform::wifi::CurrentWifi>,
    wifi_address: Option<String>,
    usb: Result<Vec<carplay_platform::usb::UsbPhone>, String>,
}
impl HostChoices {
    fn read() -> Self {
        let network = carplay_platform::network::diagnose();
        let wifi = carplay_platform::wifi::current_wifi().ok().flatten();
        let wifi_address = wifi
            .as_ref()
            .and_then(|wifi| wifi.interface_index)
            .and_then(|index| {
                network
                    .addresses
                    .iter()
                    .find(|a| a.is_up && a.interface_index == index && a.address.is_ipv4())
            })
            .map(|a| format!("{}:7000", a.address));
        Self {
            adapters: carplay_platform::bluetooth::local_adapters().unwrap_or_default(),
            peers: carplay_platform::bluetooth::paired_devices().unwrap_or_default(),
            addresses: network
                .addresses
                .into_iter()
                .filter(|a| {
                    a.is_up
                        && a.address.is_ipv4()
                        && !a.address.is_loopback()
                        && !a.address.is_unspecified()
                })
                .map(|a| format!("{}:7000", a.address))
                .collect(),
            wifi,
            wifi_address,
            usb: {
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                {
                    carplay_platform::usb::native::discover().map_err(|e| e.to_string())
                }
                #[cfg(not(any(target_os = "windows", target_os = "linux")))]
                {
                    Err("当前平台尚未实现 USB CarPlay 通道".into())
                }
            },
        }
    }
}
struct Desktop {
    config: ReceiverConfig,
    bind: String,
    local_bt: String,
    iphone: String,
    iphone_ip: String,
    mode: ConnectionMode,
    hotspot_custom: bool,
    hotspot_ssid: String,
    hotspot_password: zeroize::Zeroizing<String>,
    hotspot_status: Option<String>,
    usb_device: String,
    startup_cancel: Option<Arc<AtomicBool>>,
    auth_dir: String,
    status: String,
    logs: VecDeque<String>,
    diagnostics: Option<String>,
    job: Option<Receiver<JobResult>>,
    pending: Option<Receiver<Result<Connection, String>>>,
    connection: Option<Connection>,
    closing: Option<Receiver<Result<(), String>>>,
    texture: Option<egui::TextureHandle>,
    last_frame: Option<Arc<carplay_media::RgbaFrame>>,
    navigation: Navigation,
    logo: egui::TextureHandle,
    diagnostics_open: bool,
    night: bool,
    fullscreen: bool,
    suppress_touch: bool,
    video_session_ready: bool,
    screenshot: Option<String>,
    screenshot_requested: bool,
    touch: touch::TouchInput,
    created: Instant,
    smoke: bool,
    hosts: Option<HostChoices>,
    journal: Option<std::fs::File>,
}

impl Desktop {
    fn new(smoke: bool, screenshot: Option<String>, logo: egui::TextureHandle) -> Self {
        let profile = std::fs::read(".local/connection.json")
            .ok()
            .filter(|b| b.len() <= 16384)
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .unwrap_or_default();
        let field = |name: &str| {
            profile
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let config = carplay_app::load_config(Some(std::path::Path::new(".local/settings.json")))
            .unwrap_or_default();
        let _ = std::fs::create_dir_all(".local/logs");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let journal = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(format!(".local/logs/session-{stamp}.log"))
            .ok();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(JobResult::Hosts(HostChoices::read()));
        });
        Self {
            config,
            bind: field("bind"),
            local_bt: field("local_bt"),
            iphone: field("iphone"),
            iphone_ip: field("iphone_ip"),
            mode: profile
                .get("mode")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default(),
            hotspot_custom: false,
            hotspot_ssid: field("hotspot_ssid"),
            hotspot_password: zeroize::Zeroizing::new(String::new()),
            hotspot_status: None,
            usb_device: field("usb_device"),
            startup_cancel: None,
            auth_dir: ".local/auth".into(),
            status: "准备连接".into(),
            logs: VecDeque::new(),
            diagnostics: None,
            job: Some(rx),
            pending: None,
            connection: None,
            closing: None,
            texture: None,
            last_frame: None,
            navigation: Navigation::default(),
            logo,
            diagnostics_open: false,
            night: false,
            fullscreen: false,
            suppress_touch: false,
            video_session_ready: false,
            screenshot,
            screenshot_requested: false,
            touch: touch::TouchInput::default(),
            created: Instant::now(),
            smoke,
            hosts: None,
            journal,
        }
    }
    fn log(&mut self, line: String) {
        if let Some(file) = &mut self.journal {
            let _ = writeln!(file, "{:.3} {line}", self.created.elapsed().as_secs_f64());
        }
        if self.logs.len() == 200 {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
    }
    fn connection_options(&self) -> Result<ConnectionOptions, String> {
        let wireless = || -> Result<WirelessDevice, String> {
            Ok(WirelessDevice {
                local_bluetooth: self.local_bt.parse().map_err(|_| "请选择本机蓝牙适配器")?,
                iphone: self.iphone.parse().map_err(|_| "请选择已配对的 iPhone")?,
                iphone_ip: if self.mode == ConnectionMode::Hotspot
                    || self.iphone_ip.trim().is_empty()
                {
                    None
                } else {
                    Some(
                        self.iphone_ip
                            .trim()
                            .parse()
                            .map_err(|_| "iPhone Wi-Fi IP 地址格式不正确")?,
                    )
                },
                rfcomm_channel: None,
            })
        };
        let transport = match self.mode {
            ConnectionMode::Lan => TransportOptions::Lan {
                bind: self
                    .bind
                    .parse()
                    .map_err(|_| "请先在系统中连接 Wi-Fi，然后刷新网络与设备")?,
                device: wireless()?,
            },
            ConnectionMode::Hotspot => {
                let config = if self.hotspot_custom {
                    let config = carplay_platform::hotspot::HotspotConfig {
                        ssid: self.hotspot_ssid.clone(),
                        passphrase: self.hotspot_password.clone(),
                    };
                    config.validate().map_err(|e| e.to_string())?;
                    Some(config)
                } else {
                    None
                };
                TransportOptions::Hotspot {
                    device: wireless()?,
                    port: 7000,
                    config,
                }
            }
            ConnectionMode::Usb => TransportOptions::Usb {
                device_id: if self.usb_device.is_empty() {
                    None
                } else {
                    Some(self.usb_device.clone())
                },
            },
        };
        Ok(ConnectionOptions {
            config: self.config.clone(),
            transport,
            auth_dir: PathBuf::from(&self.auth_dir),
            state_dir: PathBuf::from(".local/state"),
        })
    }

    fn start(&mut self) {
        let parsed = self.connection_options();
        let options = match parsed {
            Ok(o) => o,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        let profile = serde_json::json!({"mode":self.mode,"bind":self.bind,"local_bt":self.local_bt,"iphone":self.iphone,"iphone_ip":self.iphone_ip,"hotspot_ssid":self.hotspot_ssid,"usb_device":self.usb_device});
        if let Ok(bytes) = serde_json::to_vec_pretty(&profile) {
            let _ = std::fs::write(".local/connection.json", bytes);
        }
        self.pending = Some(rx);
        self.navigation.begin_connection();
        self.video_session_ready = false;
        self.night = false;
        self.status = match self.mode {
            ConnectionMode::Lan => "正在读取系统 Wi-Fi 配置…",
            ConnectionMode::Hotspot => "正在准备电脑热点…",
            ConnectionMode::Usb => "正在准备 USB 链路…",
        }
        .into();
        let cancel = Arc::new(AtomicBool::new(false));
        self.startup_cancel = Some(cancel.clone());
        std::thread::spawn(move || {
            let _ = tx
                .send(Connection::start_with_cancel(options, cancel).map_err(|e| format!("{e:#}")));
        });
    }
    fn disconnect(&mut self) {
        if let Some(cancel) = &self.startup_cancel {
            cancel.store(true, Ordering::Release);
            self.status = "正在取消连接…".into();
        }
        self.release_touch();
        if let Some(mut c) = self.connection.take() {
            let (tx, rx) = mpsc::channel();
            self.closing = Some(rx);
            self.status = "正在断开…".into();
            std::thread::spawn(move || {
                let _ = tx.send(c.stop_checked().map_err(|e| format!("{e:#}")));
            });
        }
        self.texture = None;
        self.last_frame = None;
        self.navigation.disconnected();
        self.video_session_ready = false;
    }
    fn release_touch(&mut self) {
        if let Some(contact) = self.touch.release()
            && let Some(c) = &self.connection
        {
            c.hid(
                input::TOUCH_UID,
                input::touch_report(
                    &[contact],
                    self.config.width,
                    self.config.height,
                    Rotation::None,
                )
                .to_vec(),
            );
        }
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.pending.as_ref().and_then(|r| r.try_recv().ok()) {
            self.pending = None;
            let cancelled = self
                .startup_cancel
                .take()
                .is_some_and(|c| c.load(Ordering::Acquire));
            match result {
                Ok(c) => {
                    self.hotspot_password.zeroize();
                    self.connection = Some(c);
                    if cancelled {
                        self.disconnect();
                    } else {
                        self.status = "等待 iPhone 握手".into();
                    }
                }
                Err(e) => {
                    self.status = if cancelled {
                        "已取消连接".into()
                    } else {
                        e.clone()
                    };
                    self.log(e);
                }
            }
        }
        if let Some(result) = self.closing.as_ref().and_then(|r| r.try_recv().ok()) {
            self.closing = None;
            self.status = match result {
                Ok(()) => "已断开".into(),
                Err(error) => error,
            };
        }
        if let Some(result) = self.job.as_ref().and_then(|r| r.try_recv().ok()) {
            self.job = None;
            match result {
                JobResult::Diagnostics(report) => {
                    self.diagnostics = Some(report);
                    self.status = "平台检查完成".into();
                }
                JobResult::Auth(report) => {
                    self.status = report.clone();
                    self.log(report);
                }
                JobResult::Hosts(hosts) => {
                    if self.bind.is_empty()
                        && let Some(address) = &hosts.wifi_address
                    {
                        self.bind = address.clone();
                    }
                    if self.local_bt.is_empty() && hosts.adapters.len() == 1 {
                        self.local_bt = hosts.adapters[0].address.to_colon_string();
                    }
                    if self.bind.is_empty() && hosts.addresses.len() == 1 {
                        self.bind = hosts.addresses[0].clone();
                    }
                    if !self.busy() {
                        if let Some(address) = &hosts.wifi_address {
                            self.bind = address.clone();
                        }
                        if let Ok(phones) = &hosts.usb
                            && phones.len() == 1
                        {
                            self.usb_device = phones[0].id.clone();
                        }
                    }
                    self.hosts = Some(hosts);
                }
                JobResult::HotspotCapability(status) => self.hotspot_status = Some(status),
            }
        }
        let events = self
            .connection
            .as_mut()
            .map(Connection::poll)
            .unwrap_or_default();
        for e in events {
            self.handle_event(e);
        }
        if let Some(frame) = self
            .connection
            .as_ref()
            .and_then(|c| c.media.latest_frame(110))
        {
            self.display_frame(ctx, frame);
        }
    }
    fn handle_event(&mut self, event: AppEvent) {
        match &event {
            AppEvent::BootstrapStarted { .. } => self.status = "正在连接蓝牙引导…".into(),
            AppEvent::PhoneSelectionRequired => {
                self.status = "尚未匹配手机服务；可断开后填写 iPhone Wi-Fi IP 重试".into()
            }
            AppEvent::Error(error) => self.status = error.clone(),
            AppEvent::Receiver(carplay_receiver::ReceiverEvent::Verified) => {
                self.status = "AirPlay 配对验证通过".into();
                self.video_session_ready = true;
            }
            AppEvent::Receiver(carplay_receiver::ReceiverEvent::TcpAccepted) => {
                self.clear_video();
                self.navigation.begin_connection();
                self.video_session_ready = false;
                self.night = false;
            }
            AppEvent::Receiver(carplay_receiver::ReceiverEvent::Disconnected) => {
                self.status = "iPhone 已断开".into();
                self.clear_video();
                self.navigation.disconnected();
                self.video_session_ready = false;
            }
            _ => {}
        }
        self.log(format!("{event:?}"));
    }
    fn display_frame(&mut self, ctx: &egui::Context, frame: Arc<carplay_media::RgbaFrame>) {
        if !self.video_session_ready
            || self
                .last_frame
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(old, &frame))
        {
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.rgba,
        );
        if let Some(texture) = self.texture.as_mut() {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("carplay-frame", image, egui::TextureOptions::LINEAR));
        }
        self.last_frame = Some(frame);
        self.status = "正在接收画面".into();
        self.suppress_touch |= self.navigation.first_frame();
    }
    fn clear_video(&mut self) {
        // Retain the last decoded Arc so a buffered frame from the previous session
        // cannot reopen the player after a disconnect or a new TCP connection.
        self.release_touch();
        self.texture = None;
    }
    fn busy(&self) -> bool {
        self.connection.is_some() || self.pending.is_some() || self.closing.is_some()
    }

    fn go_home(&mut self, ctx: &egui::Context) {
        self.release_touch();
        self.navigation.show_home();
        self.set_fullscreen(ctx, false);
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, enabled: bool) {
        self.release_touch();
        self.suppress_touch = true;
        self.fullscreen = enabled;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(enabled));
    }

    fn home_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let width = ui.available_width().min(1120.);
                let inset = ((ui.available_width() - width) / 2.).max(0.);
                ui.horizontal(|ui| {
                    ui.add_space(inset);
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        ui.add_space(0.);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Image::new(&self.logo)
                                    .fit_to_exact_size(egui::vec2(56., 56.)),
                            );
                            ui.add_space(10.);
                            ui.vertical(|ui| {
                                ui.add_space(3.);
                                ui.label(
                                    egui::RichText::new("你的 iPhone，连接到这里。")
                                        .size(28.)
                                        .strong(),
                                );
                                style::muted(ui, "选择 USB、局域网或电脑热点，连接你的 CarPlay。");
                            });
                        });
                        ui.add_space(6.);
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(22, 41, 32))
                            .corner_radius(10)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                ui.set_width((width - 20.).max(0.));
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new(if self.texture.is_some() {
                                            "●  正在播放"
                                        } else if self.busy() {
                                            "●  连接中"
                                        } else {
                                            "●  未连接"
                                        })
                                        .color(style::GREEN)
                                        .strong(),
                                    );
                                    ui.separator();
                                    ui.label(&self.status);
                                    if self.texture.is_some()
                                        && ui.button("返回 CarPlay 画面 →").clicked()
                                    {
                                        self.navigation.show_player();
                                    }
                                });
                            });
                        ui.add_space(6.);
                        if width >= 960. {
                            let gap = 20.;
                            let left = (width - gap) * 0.56;
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                ui.allocate_ui_with_layout(
                                    egui::vec2(left, 0.),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        style::card().show(ui, |ui| {
                                            ui.set_width(left - 50.);
                                            self.connect_panel(ui);
                                        });
                                    },
                                );
                                ui.allocate_ui_with_layout(
                                    egui::vec2(width - gap - left, 0.),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        style::card().show(ui, |ui| {
                                            ui.set_width(width - gap - left - 50.);
                                            self.settings_panel(ui);
                                        });
                                    },
                                );
                            });
                        } else {
                            style::card().show(ui, |ui| {
                                ui.set_width(width - 50.);
                                self.connect_panel(ui);
                            });
                            ui.add_space(8.);
                            style::card().show(ui, |ui| {
                                ui.set_width(width - 50.);
                                self.settings_panel(ui);
                            });
                        }
                        ui.add_space(14.);
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("连接诊断").clicked() {
                                self.diagnostics_open = true;
                            }
                            style::muted(ui, "按连接模式完成准备，收到画面后自动进入 CarPlay");
                        });
                        ui.add_space(8.);
                        ui.small(
                            egui::RichText::new("开发预览版  ·  USB 取决于系统驱动与设备支持")
                                .color(style::MUTED),
                        );
                        ui.add_space(24.);
                    });
                });
            });
    }

    fn refresh_hosts(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.job = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(JobResult::Hosts(HostChoices::read()));
        });
    }

    fn connect_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("连接方式");
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        ui.label("当前平台为界面与核心预览，蓝牙、USB 和热点连接尚未实现。");
        let busy = self.busy();
        ui.add_enabled_ui(!busy, |ui| {
            let old_mode = self.mode;
            ui.horizontal(|ui| {
                for (mode, label) in [(ConnectionMode::Lan, "局域网"), (ConnectionMode::Usb, "USB"), (ConnectionMode::Hotspot, "电脑热点")] {
                    ui.selectable_value(&mut self.mode, mode, label);
                }
            });
            if old_mode != self.mode {
                self.hotspot_password.zeroize();
                self.status = "准备连接".into();
                if self.mode == ConnectionMode::Usb && self.job.is_none() { self.refresh_hosts(); }
            }
            ui.add_space(8.);
            match self.mode {
                ConnectionMode::Lan => {
                    style::muted(ui, "在系统中让电脑和 iPhone 连接同一 Wi-Fi。");
                    ui.label("无需在软件里输入 Wi-Fi 密码。");
                    let wifi = self.hosts.as_ref().and_then(|h| h.wifi.as_ref());
                    egui::Frame::new().fill(style::BACKGROUND).corner_radius(8).inner_margin(12).show(ui, |ui| {
                        ui.set_width((ui.available_width() - 24.).max(0.));
                        ui.small(egui::RichText::new("系统当前网络").color(style::MUTED));
                        ui.label(wifi.map(|w| w.ssid.as_str()).unwrap_or("尚未读取到 Wi-Fi，请连接后刷新"));
                        ui.small(egui::RichText::new(&self.bind).color(style::MUTED));
                    });
                }
                ConnectionMode::Hotspot => {
                    style::muted(ui, "由电脑提供热点，通过蓝牙引导 iPhone 加入。");
                    ui.label("默认使用系统热点配置。");
                    ui.checkbox(&mut self.hotspot_custom, "自定义热点名称和密码");
                    if self.hotspot_custom {
                        style::field(ui, "电脑热点名称", &mut self.hotspot_ssid, "RustCarPlay");
                        ui.label("热点密码（8–63 位）");
                        ui.add(egui::TextEdit::singleline(&mut *self.hotspot_password)
                            .password(true).desired_width(f32::INFINITY).hint_text("仅用于本次热点配置"));
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui.add_enabled(self.job.is_none(), egui::Button::new("检查热点支持")).clicked() {
                            let (tx, rx) = mpsc::channel();
                            self.job = Some(rx);
                            std::thread::spawn(move || {
                                let result = carplay_platform::hotspot::capability().map(|_| "系统热点接口可用".into()).unwrap_or_else(|e| e.to_string());
                                let _ = tx.send(JobResult::HotspotCapability(result));
                            });
                        }
                        #[cfg(target_os = "windows")]
                        if ui.button("系统热点设置").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::same_tab("ms-settings:network-mobilehotspot"));
                        }
                    });
                    if let Some(status) = &self.hotspot_status { ui.small(status); }
                    ui.small(egui::RichText::new("断开时仅停止由本程序启动的热点。").color(style::MUTED));
                }
                ConnectionMode::Usb => {
                    style::muted(ui, "用数据线连接 iPhone，解锁后处理手机上的信任提示。");
                    ui.label("USB 模式不需要蓝牙或 Wi-Fi 配置。");
                    let phones = self.hosts.as_ref().and_then(|h| h.usb.as_ref().ok());
                    let selected = phones.and_then(|p| p.iter().find(|p| p.id == self.usb_device));
                    egui::ComboBox::from_id_salt("usb-phone").width(ui.available_width())
                        .selected_text(selected.map(|p| p.name.as_str()).unwrap_or("选择 USB iPhone"))
                        .show_ui(ui, |ui| {
                            if let Some(phones) = phones {
                                for phone in phones {
                                    ui.selectable_value(&mut self.usb_device, phone.id.clone(), format!("{} · {:04x}", phone.name, phone.product_id));
                                }
                            }
                        });
                    if let Some(phone) = selected {
                        use carplay_platform::usb::Readiness;
                        ui.label(match &phone.readiness {
                            Readiness::InterfacesAvailable => "USB 接口可访问，连接时继续配对与会话握手".into(),
                            Readiness::ModeSwitchRequired => "连接时需要切换 iPhone 的 USB 配置并等待重枚举".into(),
                            Readiness::DriverSetupRequired(reason) => format!("需要 USB 驱动配置：{reason}"),
                            Readiness::Unavailable(reason) => format!("USB 暂不可用：{reason}"),
                        });
                    } else if let Some(Err(error)) = self.hosts.as_ref().map(|h| &h.usb) {
                        ui.label(error);
                    } else {
                        ui.label("未发现设备时，请检查数据线并刷新。");
                    }
                    #[cfg(target_os = "windows")]
                    ui.small(egui::RichText::new("Windows 有线链路需要兼容的 USBMUX + NCM 驱动配置，当前不会自动替换系统驱动。").color(style::MUTED));
                }
            }
            ui.add_space(8.);
            if self.mode != ConnectionMode::Usb {
                ui.label(egui::RichText::new("已配对 iPhone").size(13.).color(style::MUTED));
                let phone_name = self.hosts.as_ref().and_then(|h| h.peers.iter().find(|p| p.address.to_colon_string() == self.iphone))
                    .map(|p| p.name.clone()).unwrap_or_else(|| if self.iphone.is_empty() { "选择你的 iPhone".into() } else { self.iphone.clone() });
                egui::ComboBox::from_id_salt("phone-picker").width(ui.available_width()).selected_text(phone_name).show_ui(ui, |ui| {
                    if let Some(hosts) = &self.hosts {
                        for peer in &hosts.peers {
                            ui.selectable_value(&mut self.iphone, peer.address.to_colon_string(), &peer.name);
                        }
                    }
                    if self.hosts.as_ref().is_none_or(|h| h.peers.is_empty()) {
                        ui.label("请先在系统设置中完成蓝牙配对。");
                    }
                });
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(self.job.is_none(), egui::Button::new("刷新网络与设备")).clicked() {
                    self.refresh_hosts();
                }
                if self.job.is_some() { ui.spinner(); }
            });
            egui::CollapsingHeader::new("高级连接设置").id_salt("advanced-connection").show(ui, |ui| {
                if self.mode != ConnectionMode::Usb {
                    if let Some(hosts) = &self.hosts {
                        if self.mode == ConnectionMode::Lan {
                            egui::ComboBox::from_id_salt("network-picker").width(ui.available_width())
                                .selected_text(if self.bind.is_empty() { "选择网卡地址" } else { &self.bind }).show_ui(ui, |ui| {
                                    for address in &hosts.addresses { ui.selectable_value(&mut self.bind, address.clone(), address); }
                                });
                        }
                        let adapter_name = hosts.adapters.iter().find(|a| a.address.to_colon_string() == self.local_bt)
                            .map(|a| a.name.as_str()).unwrap_or("选择蓝牙适配器");
                        egui::ComboBox::from_id_salt("adapter-picker").width(ui.available_width()).selected_text(adapter_name).show_ui(ui, |ui| {
                            for adapter in &hosts.adapters { ui.selectable_value(&mut self.local_bt, adapter.address.to_colon_string(), &adapter.name); }
                        });
                    }
                    style::field(ui, "本机蓝牙地址", &mut self.local_bt, "AA:BB:CC:DD:EE:FF");
                    style::field(ui, "iPhone 蓝牙地址", &mut self.iphone, "AA:BB:CC:DD:EE:FF");
                    if self.mode == ConnectionMode::Lan {
                        style::field(ui, "本机 Wi-Fi 地址", &mut self.bind, "192.168.1.20:7000");
                        style::field(ui, "iPhone IP（可选）", &mut self.iphone_ip, "自动发现失败时填写");
                    }
                }
                style::field(ui, "认证资源目录", &mut self.auth_dir, ".local/auth");
            });
        });
        ui.add_space(14.);
        if self.texture.is_some() {
            if ui
                .add_sized(
                    [ui.available_width(), 44.],
                    style::primary("打开 CarPlay  →"),
                )
                .clicked()
            {
                self.navigation.show_player();
            }
        } else if ui
            .add_enabled(
                !busy && cfg!(any(target_os = "windows", target_os = "linux")),
                style::primary(if busy {
                    "正在连接…"
                } else {
                    "连接 CarPlay  →"
                })
                .min_size(egui::vec2(ui.available_width(), 44.)),
            )
            .clicked()
        {
            self.start();
        }
        if self.connection.is_some() && ui.button("断开连接").clicked() {
            self.disconnect();
        }
        if self.pending.is_some() && ui.button("取消连接").clicked() {
            self.disconnect();
        }
    }

    fn player_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("←  返回首页")
                .on_hover_text("保持连接，调整前请先断开")
                .clicked()
            {
                self.go_home(ui.ctx());
            }
            ui.separator();
            ui.label(
                egui::RichText::new("●  CarPlay")
                    .color(style::GREEN)
                    .strong(),
            );
            if let Some(frame) = &self.last_frame {
                ui.label(
                    egui::RichText::new(format!("{} × {}", frame.width, frame.height))
                        .color(style::MUTED)
                        .size(12.),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(if self.fullscreen {
                        "退出全屏"
                    } else {
                        "全屏"
                    })
                    .on_hover_text("F11 · Esc 退出全屏")
                    .clicked()
                {
                    self.set_fullscreen(ui.ctx(), !self.fullscreen);
                }
                let mut night = self.night;
                if ui.toggle_value(&mut night, "夜间模式").clicked()
                    && let Some(c) = &self.connection
                    && c.night(night)
                {
                    self.night = night;
                }
                if ui.button("断开").clicked() {
                    self.disconnect();
                }
            });
        });
    }

    fn video_panel(&mut self, ui: &mut egui::Ui) {
        if let Some(texture) = &self.texture {
            let bounds = ui.available_rect_before_wrap();
            let size = fit_size(texture.size_vec2(), bounds.size());
            let rect = egui::Rect::from_center_size(bounds.center(), size);
            let response = ui.put(
                rect,
                egui::Image::new(texture)
                    .fit_to_exact_size(size)
                    .sense(egui::Sense::click_and_drag()),
            );
            let contacts = ui.input(|i| {
                self.touch.process(
                    response.rect,
                    &i.events,
                    i.pointer.primary_down(),
                    i.focused && !self.diagnostics_open && !self.suppress_touch,
                )
            });
            for contact in contacts {
                if let Some(c) = &self.connection {
                    c.hid(
                        input::TOUCH_UID,
                        input::touch_report(
                            &[contact],
                            self.config.width,
                            self.config.height,
                            Rotation::None,
                        )
                        .to_vec(),
                    );
                }
            }
        } else {
            self.release_touch();
            ui.centered_and_justified(|ui| {
                ui.label("正在等待 iPhone 画面…");
            });
        }
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("显示与声音");
        style::muted(ui, "在连接前，设置适合你的播放体验。");
        ui.add_space(12.);
        ui.add_enabled_ui(!self.busy(), |ui| {
            style::field(ui, "接收端名称", &mut self.config.name, "RustCarPlay");
            ui.label(
                egui::RichText::new("画面分辨率")
                    .size(13.)
                    .color(style::MUTED),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.config.width)
                        .range(320..=3840)
                        .speed(8),
                );
                ui.label("×");
                ui.add(
                    egui::DragValue::new(&mut self.config.height)
                        .range(240..=2160)
                        .speed(8),
                );
                ui.label(egui::RichText::new("px").color(style::MUTED));
            });
            ui.label(egui::RichText::new("帧率").size(13.).color(style::MUTED));
            egui::ComboBox::from_id_salt("fps")
                .width(ui.available_width())
                .selected_text(format!(
                    "{} fps{}",
                    self.config.fps,
                    if self.config.fps == 30 {
                        " · 推荐"
                    } else {
                        ""
                    }
                ))
                .show_ui(ui, |ui| {
                    for fps in [24, 25, 30, 50, 60] {
                        ui.selectable_value(&mut self.config.fps, fps, format!("{fps} fps"));
                    }
                });
            ui.add_space(4.);
            ui.separator();
            ui.checkbox(&mut self.config.audio_output, "播放音乐和导航语音");
            ui.add_enabled_ui(self.config.audio_output, |ui| {
                ui.checkbox(&mut self.config.microphone, "Siri / 通话麦克风（实验性）");
            });
            if !self.config.audio_output {
                self.config.microphone = false;
            }
            egui::CollapsingHeader::new("更多显示选项").show(ui, |ui| {
                ui.checkbox(&mut self.config.hevc, "HEVC 视频（实验性）");
                ui.checkbox(&mut self.config.right_hand_drive, "右舵布局");
                style::muted(ui, "默认使用 H.264，画面始终保持原比例。");
            });
            ui.add_space(12.);
            if ui
                .add_sized([ui.available_width(), 40.], egui::Button::new("保存设置"))
                .clicked()
            {
                match self.config.validate() {
                    Ok(()) => {
                        let result = std::fs::create_dir_all(".local").and_then(|_| {
                            std::fs::write(
                                ".local/settings.json",
                                serde_json::to_vec_pretty(&self.config).unwrap(),
                            )
                        });
                        self.status = match result {
                            Ok(()) => "设置已保存，下次连接生效".into(),
                            Err(e) => e.to_string(),
                        };
                    }
                    Err(e) => self.status = e.into(),
                }
            }
        });
        ui.add_space(6.);
        ui.small(
            egui::RichText::new(if self.busy() {
                "当前连接使用以上设置，断开后可以修改。"
            } else {
                "设置用于下次连接；保存后重启仍会保留。"
            })
            .color(style::MUTED),
        );
    }
    fn diagnostics_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("诊断");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.job.is_none(), egui::Button::new("检查平台"))
                .clicked()
            {
                let (tx, rx) = mpsc::channel();
                self.job = Some(rx);
                std::thread::spawn(move || {
                    let report = carplay_platform::collect_diagnostics();
                    let _ = tx.send(JobResult::Diagnostics(
                        serde_json::to_string_pretty(&report).unwrap_or_default(),
                    ));
                });
            }
            if ui
                .add_enabled(self.job.is_none(), egui::Button::new("检查认证资源"))
                .clicked()
            {
                let path = self.auth_dir.clone();
                let (tx, rx) = mpsc::channel();
                self.job = Some(rx);
                std::thread::spawn(move || {
                    let result = match carplay_auth::LocalIdentity::load(path) {
                        Ok(_) => "私钥与证书匹配；iPhone 是否接受仍需连接验证".into(),
                        Err(e) => format!("认证资源检查失败：{e}"),
                    };
                    let _ = tx.send(JobResult::Auth(result));
                });
            }
            if ui.button("复制诊断").clicked() {
                ui.ctx()
                    .copy_text(self.diagnostics.clone().unwrap_or_default());
            }
        });
        if self.job.is_some() {
            ui.spinner();
        }
        ui.label(&self.status);
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            if let Some(report) = &self.diagnostics {
                ui.monospace(report);
            }
            for line in &self.logs {
                ui.monospace(line);
            }
        });
    }
}
fn fit_size(source: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    source * (available.x / source.x).min(available.y / source.y).max(0.)
}

impl eframe::App for Desktop {
    fn on_exit(&mut self) {
        if let Some(cancel) = &self.startup_cancel {
            cancel.store(true, Ordering::Release);
        }
        if let Some(pending) = self.pending.take()
            && let Ok(Ok(mut connection)) = pending.recv()
        {
            connection.stop();
        }
        if let Some(c) = &mut self.connection {
            c.stop();
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.suppress_touch = false;
        self.poll(ctx);
        // Window state may also change through native window manager shortcuts.
        self.fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(self.fullscreen));
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.diagnostics_open {
                self.diagnostics_open = false;
            } else if self.fullscreen {
                self.set_fullscreen(ctx, false);
            } else if self.navigation.page() == Page::Player {
                self.go_home(ctx);
            }
        }
        if self.navigation.page() == Page::Player && ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            self.set_fullscreen(ctx, !self.fullscreen);
        }
        if self.navigation.page() != Page::Player
            || !ctx.input(|i| i.focused)
            || self.diagnostics_open
        {
            self.release_touch();
        }
        if self.navigation.page() == Page::Home && self.fullscreen {
            self.set_fullscreen(ctx, false);
        }
        if !self.fullscreen {
            chrome::title_bar(
                ctx,
                &self.logo,
                if self.navigation.page() == Page::Home {
                    "连接与设置"
                } else {
                    "CarPlay"
                },
            );
        }
        if self.navigation.page() == Page::Player {
            egui::TopBottomPanel::top("player-controls")
                .frame(
                    egui::Frame::new()
                        .fill(style::SURFACE)
                        .inner_margin(egui::Margin::symmetric(12, 6)),
                )
                .show(ctx, |ui| self.player_toolbar(ui));
        }
        let player = self.navigation.page() == Page::Player;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(if player {
                        egui::Color32::BLACK
                    } else {
                        style::BACKGROUND
                    })
                    .inner_margin(if player { 4 } else { 22 }),
            )
            .show(ctx, |ui| {
                if player {
                    self.video_panel(ui);
                } else {
                    self.home_panel(ui);
                }
            });
        if self.diagnostics_open {
            let mut open = true;
            egui::Window::new("连接诊断")
                .open(&mut open)
                .default_width(720.)
                .default_height(420.)
                .collapsible(false)
                .show(ctx, |ui| self.diagnostics_panel(ui));
            self.diagnostics_open = open;
        }
        chrome::resize_edges(ctx);
        self.capture_screenshot(ctx);
        ctx.request_repaint_after(Duration::from_millis(if player || self.busy() {
            33
        } else {
            150
        }));
        if self.smoke && self.created.elapsed() > Duration::from_secs(3) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl Desktop {
    // Capture the app's own rendered viewport for native visual smoke checks.
    fn capture_screenshot(&mut self, ctx: &egui::Context) {
        use eframe::icon_data::IconDataExt;
        if let Some(path) = &self.screenshot {
            if !self.screenshot_requested && self.created.elapsed() > Duration::from_millis(700) {
                self.screenshot_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            let image = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = image {
                let icon = egui::IconData {
                    width: image.width() as u32,
                    height: image.height() as u32,
                    rgba: image
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_srgba_unmultiplied())
                        .collect(),
                };
                match icon
                    .to_png_bytes()
                    .and_then(|bytes| std::fs::write(path, bytes).map_err(|e| e.to_string()))
                {
                    Ok(()) => self.screenshot = None,
                    Err(e) => {
                        self.log(format!("UI capture failed: {e}"));
                        self.screenshot = None;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn letterbox_preserves_aspect_ratio() {
        assert_eq!(
            fit_size(egui::vec2(1280., 720.), egui::vec2(640., 640.)),
            egui::vec2(640., 360.)
        );
    }
}
